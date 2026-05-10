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

/// Watch a directory for newly-created PNG files and push each one to the
/// clipboard. Blocks the current thread until the channel closes.
pub fn run(dir: &Path) -> Result<()> {
    if !dir.exists() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("failed to create watch dir {}", dir.display()))?;
    }

    let (tx, rx) = mpsc::channel::<DebounceEventResult>();
    let mut debouncer = new_debouncer(Duration::from_millis(200), None, move |result| {
        let _ = tx.send(result);
    })
    .context("failed to create file watcher")?;

    debouncer
        .watch(dir, RecursiveMode::NonRecursive)
        .with_context(|| format!("failed to watch {}", dir.display()))?;

    info!(dir = %dir.display(), "shotpaste watcher started");

    // Dedup pushed screenshots by (mtime, size). Belt-and-suspenders against
    // the event filter: macOS FSEvents and iCloud / Spotlight / xattr churn
    // can re-fire events for the same file long after we've already pushed
    // it. Without this, a single trickle source pegs NSPasteboard and makes
    // Cmd+V hang in other apps. mtime alone isn't enough — APFS resolution
    // can collide on rapid writes — so we tiebreak with size.
    let mut seen: HashMap<PathBuf, (SystemTime, u64)> = HashMap::new();

    while let Ok(result) = rx.recv() {
        match result {
            Ok(events) => {
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
                        if let Err(e) = clipboard::write_png(path) {
                            error!("failed to set clipboard for {}: {e:#}", path.display());
                            continue;
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
            Err(errors) => {
                for e in errors {
                    warn!("watcher error: {e}");
                }
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
