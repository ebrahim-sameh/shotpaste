use crate::clipboard;
use anyhow::{Context, Result};
use notify::{EventKind, RecursiveMode, event::CreateKind};
use notify_debouncer_full::{DebounceEventResult, new_debouncer};
use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;
use tracing::{debug, error, info, warn};

/// Watch a directory for newly-created or modified PNG files and push each
/// one to the clipboard. Blocks the current thread until the channel closes.
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
                        debug!(path = %path.display(), kind = ?event.kind, "screenshot event");
                        if let Err(e) = clipboard::write_png(path) {
                            error!("failed to set clipboard for {}: {e:#}", path.display());
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
    matches!(
        kind,
        EventKind::Create(CreateKind::File)
            | EventKind::Create(CreateKind::Any)
            | EventKind::Modify(_)
    )
}
