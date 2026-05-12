use crate::clipboard;
use anyhow::{Context, Result};
#[cfg(target_os = "linux")]
use notify::event::{ModifyKind, RenameMode};
use notify::{EventKind, RecursiveMode, event::CreateKind};
use notify_debouncer_full::{DebounceEventResult, new_debouncer};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, SystemTime};
use tracing::{debug, error, info, warn};

/// Cap on the dedup map. 256 is generous for any realistic burst of
/// screenshots between watcher restarts; eviction below is arbitrary, not
/// LRU, since at this size the policy doesn't matter.
const SEEN_CAP: usize = 256;

/// How often the loop wakes to check the shutdown channel. Trade-off:
/// shorter = more responsive Quit / dir reconfiguration, slightly more CPU
/// idle. 500 ms is invisible to the user and effectively zero CPU.
const SHUTDOWN_POLL: Duration = Duration::from_millis(500);

/// Observer for the watcher loop. The headless CLI path uses [`LogSink`];
/// tray mode uses a `ChannelSink` (defined in `tray.rs`) that forwards
/// events into the event loop. Decoupling lets the watcher stay free of
/// `tao`/`crossbeam` types so it builds with `--no-default-features`.
pub trait WatcherSink: Send + 'static {
    fn pushed(&self, path: &Path);
    fn failed(&self, path: &Path, err: &anyhow::Error);
}

/// Default sink — relies purely on `tracing` logs already emitted by the
/// watcher and clipboard modules. Cheap, allocation-free.
pub struct LogSink;

impl WatcherSink for LogSink {
    fn pushed(&self, _path: &Path) {}
    fn failed(&self, _path: &Path, _err: &anyhow::Error) {}
}

/// Convenience wrapper for callers that never want to stop the watcher
/// (the headless CLI path). Creates a never-signaled shutdown channel and
/// delegates to [`run_until`].
pub fn run<S: WatcherSink>(dirs: &[PathBuf], sink: S) -> Result<()> {
    let (_keep_alive_tx, never_stop_rx) = mpsc::channel::<()>();
    // `_keep_alive_tx` stays in scope until this function returns, so
    // `never_stop_rx` never sees a disconnect — exactly the semantics
    // pre-multi-watch had.
    let result = run_until(dirs, sink, &never_stop_rx);
    drop(_keep_alive_tx);
    result
}

/// Watch one or more directories for newly-created PNG files and push
/// each one to the clipboard. Returns when:
///   - `stop` channel receives a value (graceful shutdown), or
///   - `stop` channel is disconnected (sender dropped — also graceful), or
///   - the debouncer's event channel disconnects (watcher backend died).
///
/// All watch dirs feed one `notify_debouncer_full::Debouncer` and share one
/// dedup map keyed on full `PathBuf` — so the same screenshot landing in
/// two watched folders only pushes once, and same-named files in different
/// folders push independently.
pub fn run_until<S: WatcherSink>(
    dirs: &[PathBuf],
    sink: S,
    stop: &mpsc::Receiver<()>,
) -> Result<()> {
    if dirs.is_empty() {
        anyhow::bail!("no directories to watch");
    }

    let (tx, rx) = mpsc::channel::<DebounceEventResult>();
    let mut debouncer = new_debouncer(Duration::from_millis(200), None, move |result| {
        let _ = tx.send(result);
    })
    .context("failed to create file watcher")?;

    // Register each dir individually. If one fails (missing, not a dir,
    // permission denied) we log and continue — losing all dirs because one
    // is broken is the wrong default.
    let mut registered = 0usize;
    for dir in dirs {
        match debouncer.watch(dir, RecursiveMode::NonRecursive) {
            Ok(()) => {
                info!(dir = %dir.display(), "watching");
                registered += 1;
            }
            Err(e) => {
                warn!(dir = %dir.display(), error = %e, "skipping unwatchable folder");
            }
        }
    }
    if registered == 0 {
        anyhow::bail!("no watchable directories — see preceding warnings");
    }
    info!(count = registered, "shotpaste watcher started");

    // Dedup pushed screenshots by (mtime, size). Belt-and-suspenders against
    // the event filter: macOS FSEvents and iCloud / Spotlight / xattr churn
    // can re-fire events for the same file long after we've already pushed
    // it. Without this, a single trickle source pegs NSPasteboard and makes
    // Cmd+V hang in other apps. mtime alone isn't enough — APFS resolution
    // can collide on rapid writes — so we tiebreak with size.
    let mut seen: HashMap<PathBuf, (SystemTime, u64)> = HashMap::new();

    loop {
        match rx.recv_timeout(SHUTDOWN_POLL) {
            Ok(Ok(events)) => {
                for event in events {
                    if !is_new_file_event(&event.kind) {
                        continue;
                    }
                    for path in &event.paths {
                        if path.extension().and_then(|s| s.to_str()) != Some("png") {
                            continue;
                        }
                        let fingerprint = std::fs::metadata(path)
                            .ok()
                            .and_then(|m| m.modified().ok().map(|t| (t, m.len())));
                        if let Some(fp) = fingerprint
                            && seen.get(path).is_some_and(|prev| *prev == fp)
                        {
                            continue;
                        }
                        debug!(path = %path.display(), kind = ?event.kind, "screenshot event");
                        match clipboard::write_png(path) {
                            Ok(()) => sink.pushed(path),
                            Err(e) => {
                                error!("failed to set clipboard for {}: {e:#}", path.display());
                                sink.failed(path, &e);
                                continue;
                            }
                        }
                        if let Some(fp) = fingerprint {
                            if seen.len() >= SEEN_CAP && !seen.contains_key(path) {
                                if let Some(k) = seen.keys().next().cloned() {
                                    seen.remove(&k);
                                }
                            }
                            seen.insert(path.clone(), fp);
                        }
                    }
                }
            }
            Ok(Err(errors)) => {
                for e in errors {
                    warn!("watcher error: {e}");
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // Periodic wake — check whether we've been asked to stop.
                match stop.try_recv() {
                    Ok(()) | Err(mpsc::TryRecvError::Disconnected) => {
                        info!("watcher stopping");
                        break;
                    }
                    Err(mpsc::TryRecvError::Empty) => continue,
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                warn!("watcher backend channel closed unexpectedly");
                break;
            }
        }
    }

    Ok(())
}

fn is_new_file_event(kind: &EventKind) -> bool {
    match kind {
        EventKind::Create(CreateKind::File | CreateKind::Any) => true,
        // Linux atomic-rename screenshot tools (Flameshot, GNOME Screenshot)
        // write a `.tmp` then rename to `.png`; the destination surfaces as
        // Modify(Name) rather than Create. macOS `screencapture` and Windows
        // Snipping Tool / ShareX both emit Create, so the rename arms would
        // only add noise on those platforms (FSEvents in particular fires
        // Modify generously for metadata, Spotlight, iCloud, Quick Look).
        #[cfg(target_os = "linux")]
        EventKind::Modify(ModifyKind::Name(RenameMode::To | RenameMode::Both)) => true,
        _ => false,
    }
}
