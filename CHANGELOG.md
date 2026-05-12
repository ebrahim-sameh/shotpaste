# Changelog

All notable changes to shotpaste are documented here. Format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/);
this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.3.0] - 2026-05-12

### Added
- **Watch multiple folders at once.** Pass several paths on the CLI
  (`shotpaste watch ~/Pictures/Screenshots ~/Desktop ~/Downloads`) or
  list them as `watch_dirs = [...]` in `<config>/shotpaste/config.toml`.
  One process, one debouncer, one atomic clipboard push per new PNG no
  matter which folder it lands in.
- **Tray-editable watch list.** New "Add watched folder…" menu item
  opens a native folder picker (via the `rfd` crate). With 2+ folders
  watched, the menu shows a "Watched folders ▶" submenu where each
  entry can be opened in the file manager or removed from the watch
  list. Removal is blocked at the last folder to avoid orphaning the
  daemon.

### Changed
- `Config` schema: `watch_dir: Option<PathBuf>` → `watch_dirs: Vec<PathBuf>`.
  Existing v0.2.0 configs migrate transparently — the legacy field is
  folded into `watch_dirs` on first load and dropped on next save.
- The watcher loop can now be shut down cleanly (~500 ms latency) via
  a new `watcher::run_until` entry point. The headless `watcher::run`
  wrapper preserves the prior never-stops semantics.
- Tray menu shape adapts to folder count: 1 folder → flat "Open
  watched folder" (today's layout); 2+ folders → "Watched folders ▶"
  submenu. Tooltip lists all watched paths on hover.

## [0.2.0] - 2026-05-12

### Added
- **System tray icon** with right-click context menu: watching folder, last
  capture time, session push counter, "Open watched folder", "Push last
  screenshot again", "Open log file", toggles for "Start at login (next
  session)", "Notify on success" / "Notify on error", and Quit.
- **Toast notifications** for each successful push (Windows / macOS / Linux),
  branded as "shotpaste". Bursts coalesce into a single toast within a
  ~1.5-second window. Errors always toast immediately.
- `--headless` flag on `shotpaste watch` to skip the tray (useful for SSH
  sessions, servers, custom supervisors). Linux auto-detects headless mode
  when neither `$DISPLAY` nor `$WAYLAND_DISPLAY` is set.
- New default-on Cargo feature `tray`. Build a slim headless binary with
  `cargo install shotpaste --no-default-features`.
- Single-instance lock (Windows named mutex / Unix flock) so two watchers
  in the same session can't fight over the clipboard.
- Rolling daily log file at `<cache>/shotpaste/shotpaste.log.YYYY-MM-DD`
  whenever the tray is active (stderr is invisible under `wscript.exe` /
  launchd / systemd).
- Persistent config at `<config>/shotpaste/config.toml` for the notify
  toggles.
- Windows: registers an AppUserModelID in
  `HKCU\Software\Classes\AppUserModelId\dev.shotpaste.watcher` on first
  toast so notifications display as "shotpaste" rather than the default
  "Windows PowerShell" sender.

### Changed
- The watcher loop now takes a `WatcherSink` trait so tray mode can observe
  push/fail events without coupling the watcher to GUI types. No behavior
  change for `--headless` / `--no-default-features` builds.
- Windows installer skips `Start-ScheduledTask` when invoked from the tray
  ("Start at login" toggle) to avoid spawning a duplicate watcher under
  `wscript.exe`.

### Notes
- GNOME doesn't show legacy tray icons natively. Install
  [`gnome-shell-extension-appindicator`](https://extensions.gnome.org/extension/615/appindicator-support/)
  (pre-packaged on Ubuntu) to see the icon. The watcher and toasts still
  work without it.
