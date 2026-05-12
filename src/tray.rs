//! System-tray UI for shotpaste.
//!
//! Owns the `tao::EventLoop`, builds the `TrayIcon` and its menu, spawns
//! the watcher in a background thread, and dispatches everything onto the
//! main thread via [`AppEvent`]. Headless mode (`--headless`, no display,
//! tray host unavailable) skips this module entirely and falls back to
//! [`crate::watcher::run`] on the main thread.
//!
//! Threading: `TrayIcon` is created on the main thread inside
//! `StartCause::Init` per upstream guidance (otherwise the icon doesn't
//! show until the next event on macOS — issue tauri-apps/tray-icon#90).
//! All menu/state mutations happen on the main thread. The watcher thread
//! only sends `AppEvent::Pushed/Failed`; the tick thread sends `Tick`.
//!
//! Multi-folder support (v0.3.0): the watch list is mutable at runtime via
//! the tray's "Add watched folder…" / per-folder Remove items. On each
//! mutation the existing watcher thread is signalled to stop, joined, and
//! a fresh thread spawned with the new dir list. Reconfiguration latency
//! is one `watcher::SHUTDOWN_POLL` (~500 ms) — invisible.

use crate::config::Config;
use crate::notify as toast;
use crate::watcher::{self, WatcherSink};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
use tracing::{error, warn};
use tray_icon::menu::{
    CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu,
};
use tray_icon::{Icon, TrayIconBuilder, TrayIconEvent};

/// Coalescing window: bursts within this interval collapse into one toast.
const TOAST_COALESCE: Duration = Duration::from_millis(1500);
/// Menu "Last capture" text refresh cadence.
const TICK_INTERVAL: Duration = Duration::from_secs(60);

/// Embedded icon assets. The color icon is used on Windows / Linux and as
/// the macOS dock fallback; the template variant is the menu-bar icon on
/// macOS (auto-inverts in dark mode).
const ICON_COLOR: &[u8] = include_bytes!("../assets/icon.png");
#[cfg(target_os = "macos")]
const ICON_TEMPLATE: &[u8] = include_bytes!("../assets/icon-template.png");

/// Everything the main-thread dispatcher needs to handle. Toast flushing
/// rides on tao's `ControlFlow::WaitUntil` deadline + the resulting
/// `StartCause::ResumeTimeReached` wake — no dedicated variant needed.
#[derive(Debug)]
enum AppEvent {
    Tray(TrayIconEvent),
    Menu(MenuEvent),
    Pushed(PathBuf),
    Failed {
        path: PathBuf,
        error: String,
    },
    /// Periodic wake to refresh the relative "Last capture: X ago" label.
    Tick,
    /// Result of an `rfd::FileDialog::pick_folder()` running on a worker
    /// thread. `None` means the user cancelled.
    PickedFolder(Option<PathBuf>),
}

/// Sink that the watcher thread uses to forward events to the main loop.
struct ChannelSink(Mutex<EventLoopProxy<AppEvent>>);

impl WatcherSink for ChannelSink {
    fn pushed(&self, path: &Path) {
        if let Ok(p) = self.0.lock() {
            let _ = p.send_event(AppEvent::Pushed(path.to_path_buf()));
        }
    }
    fn failed(&self, path: &Path, err: &anyhow::Error) {
        if let Ok(p) = self.0.lock() {
            let _ = p.send_event(AppEvent::Failed {
                path: path.to_path_buf(),
                error: format!("{err:#}"),
            });
        }
    }
}

/// Handle to a running watcher thread + its shutdown channel.
struct WatcherHandle {
    stop_tx: mpsc::Sender<()>,
    join: Option<JoinHandle<()>>,
}

impl WatcherHandle {
    fn shutdown(mut self) {
        let _ = self.stop_tx.send(());
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

/// Spawn the watcher thread. Returns immediately. Caller keeps the handle
/// alive; dropping it without calling `shutdown` leaks the thread (it'll
/// exit when the process does).
fn spawn_watcher(dirs: Vec<PathBuf>, proxy: EventLoopProxy<AppEvent>) -> WatcherHandle {
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let sink = ChannelSink(Mutex::new(proxy));
    let join = std::thread::Builder::new()
        .name("shotpaste-watcher".into())
        .spawn(move || {
            if let Err(e) = watcher::run_until(&dirs, sink, &stop_rx) {
                error!("watcher exited with error: {e:#}");
            }
        })
        .expect("spawn watcher thread");
    WatcherHandle {
        stop_tx,
        join: Some(join),
    }
}

/// Menu items kept on hand so the dispatcher can compare `event.id`
/// against each one (and flip `set_checked` / `set_text`). Items that
/// only appear in certain shapes (single-dir flat vs multi-dir submenu)
/// are wrapped in `Option`.
struct MenuItems {
    _menu: Menu,
    /// Disabled header — "Watching: <abbrev>" or "Watching: N folders".
    watching: MenuItem,
    last_capture: MenuItem,
    pushes: MenuItem,
    /// Single-dir shape only — flat "Open watched folder" item.
    open_folder: Option<MenuItem>,
    /// Multi-dir shape only — submenu containing per-folder open/remove.
    /// We keep references to its children so the dispatcher can match ids.
    folders_submenu: Option<Submenu>,
    /// Parallel to `state.watch_dirs`. Entry `i` is `(open_id, remove_id)`
    /// for `state.watch_dirs[i]`.
    folder_actions: Vec<(MenuId, MenuId)>,
    add_folder: MenuItem,
    push_again: MenuItem,
    open_log: MenuItem,
    start_at_login: CheckMenuItem,
    notify_success: CheckMenuItem,
    notify_error: CheckMenuItem,
    quit: MenuItem,
}

/// Mutable session state held on the main thread.
struct AppState {
    watch_dirs: Vec<PathBuf>,
    config: Config,
    last_capture_at: Option<SystemTime>,
    last_path: Option<PathBuf>,
    pushes: usize,
    /// Number of pushes queued for the next toast emission.
    toast_pending: usize,
    /// Path attached to the most recently queued push — used in the toast body.
    toast_path: Option<PathBuf>,
    /// Wall-clock instant at which the queued toast should fire.
    toast_flush_at: Option<Instant>,
    /// True while an `rfd::FileDialog` is open — prevents stacking multiple
    /// pickers if the user spams "Add watched folder…".
    picker_open: bool,
}

/// Run the tray UI. Spawns the watcher thread, blocks until the user picks
/// "Quit" or the event loop is otherwise terminated.
pub fn run(initial_dirs: Vec<PathBuf>) -> Result<()> {
    let event_loop = EventLoopBuilder::<AppEvent>::with_user_event().build();

    // Wire menu/tray event handlers BEFORE creating the tray, so the very
    // first user click can't race a not-yet-installed handler.
    let proxy = event_loop.create_proxy();
    {
        let proxy = proxy.clone();
        TrayIconEvent::set_event_handler(Some(move |event| {
            let _ = proxy.send_event(AppEvent::Tray(event));
        }));
    }
    {
        let proxy = proxy.clone();
        MenuEvent::set_event_handler(Some(move |event| {
            let _ = proxy.send_event(AppEvent::Menu(event));
        }));
    }

    // Periodic relative-time refresh. Cheap — one wake/min.
    {
        let proxy = proxy.clone();
        std::thread::Builder::new()
            .name("shotpaste-tick".into())
            .spawn(move || {
                loop {
                    std::thread::sleep(TICK_INTERVAL);
                    if proxy.send_event(AppEvent::Tick).is_err() {
                        return; // loop has shut down
                    }
                }
            })
            .ok();
    }

    let mut state = AppState {
        watch_dirs: initial_dirs.clone(),
        config: Config::load(),
        last_capture_at: None,
        last_path: None,
        pushes: 0,
        toast_pending: 0,
        toast_path: None,
        toast_flush_at: None,
        picker_open: false,
    };

    // Initial watcher thread.
    let mut watcher_handle: Option<WatcherHandle> =
        Some(spawn_watcher(initial_dirs, proxy.clone()));

    // tray_icon and menu live in Options so they're constructed inside
    // StartCause::Init (per tauri-apps/tray-icon#90). The leading underscore
    // silences a false-positive `unused_assignments` — dropping the TrayIcon
    // would remove the icon from the OS, so we must hold it for the whole
    // event loop.
    let mut tray: Option<tray_icon::TrayIcon> = None;
    let mut items: Option<MenuItems> = None;

    event_loop.run(move |event, _, control_flow| {
        *control_flow = match state.toast_flush_at {
            Some(deadline) => ControlFlow::WaitUntil(deadline),
            None => ControlFlow::Wait,
        };

        match event {
            Event::NewEvents(StartCause::Init) => {
                let menu_items = build_menu(&state).expect("build menu");
                match build_tray(&menu_items._menu, &state) {
                    Ok(t) => tray = Some(t),
                    Err(e) => {
                        warn!("tray host unavailable; running with notifications only ({e:#})")
                    }
                }
                items = Some(menu_items);
            }

            Event::NewEvents(StartCause::ResumeTimeReached { .. }) => {
                maybe_flush_toast(&mut state);
            }

            Event::UserEvent(AppEvent::Tray(_e)) => {
                // No-op for now: clicks/double-clicks aren't bound to actions.
                // Right-click is handled natively by the OS to show the menu.
            }

            Event::UserEvent(AppEvent::Menu(e)) => {
                // Two-phase: read `items` immutably to classify the click,
                // then drop that borrow before mutating anything (which
                // may include rebuilding `items` itself).
                let action = items
                    .as_ref()
                    .map(|it| classify_menu_event(&e.id, it))
                    .unwrap_or(MenuAction::Unknown);
                handle_menu_action(
                    action,
                    &mut state,
                    control_flow,
                    &proxy,
                    &mut tray,
                    &mut items,
                    &mut watcher_handle,
                );
            }

            Event::UserEvent(AppEvent::Pushed(path)) => {
                state.pushes += 1;
                state.last_capture_at = Some(SystemTime::now());
                state.last_path = Some(path.clone());
                state.toast_pending += 1;
                state.toast_path = Some(path);
                if state.toast_flush_at.is_none() {
                    state.toast_flush_at = Some(Instant::now() + TOAST_COALESCE);
                }
                if let Some(it) = items.as_ref() {
                    refresh_status_items(it, &state);
                }
            }

            Event::UserEvent(AppEvent::Failed { path, error }) if state.config.notify_on_error => {
                toast::error(&path, &error);
            }

            Event::UserEvent(AppEvent::Tick) => {
                if let Some(it) = items.as_ref() {
                    it.last_capture
                        .set_text(format_last_capture(state.last_capture_at));
                }
            }

            Event::UserEvent(AppEvent::PickedFolder(picked)) => {
                state.picker_open = false;
                if let Some(p) = picked {
                    let canon = std::fs::canonicalize(&p).unwrap_or(p);
                    if state.watch_dirs.iter().any(|d| d == &canon) {
                        // Already watching this folder — silent no-op.
                    } else {
                        state.watch_dirs.push(canon);
                        persist_watch_dirs(&mut state);
                        restart_watcher(&state, &proxy, &mut watcher_handle);
                        rebuild_menu(&state, &mut tray, &mut items);
                    }
                }
            }

            _ => {}
        }
    })
}

/// Top-level menu builder. Shape depends on `state.watch_dirs.len()`.
fn build_menu(state: &AppState) -> Result<MenuItems> {
    let menu = Menu::new();

    let watching = MenuItem::new(watching_header(&state.watch_dirs), false, None);
    let last_capture = MenuItem::new(format_last_capture(state.last_capture_at), false, None);
    let pushes = MenuItem::new(
        format!("Pushes this session: {}", state.pushes),
        false,
        None,
    );

    // Folder-management surface differs by count.
    let (open_folder, folders_submenu, folder_actions);
    if state.watch_dirs.len() <= 1 {
        let open = MenuItem::new("Open watched folder", !state.watch_dirs.is_empty(), None);
        open_folder = Some(open);
        folders_submenu = None;
        folder_actions = Vec::new();
    } else {
        let (sub, actions) = build_folders_submenu(&state.watch_dirs)?;
        open_folder = None;
        folders_submenu = Some(sub);
        folder_actions = actions;
    }

    let add_folder = MenuItem::new("Add watched folder…", true, None);
    let push_again = MenuItem::new(
        "Push last screenshot again",
        state.last_path.is_some(),
        None,
    );
    let open_log = MenuItem::new("Open log file", true, None);

    let start_at_login = CheckMenuItem::new(
        "Start at login (next session)",
        true,
        installer_installed(),
        None,
    );
    let notify_success = CheckMenuItem::new(
        "Notify on success",
        true,
        state.config.notify_on_success,
        None,
    );
    let notify_error =
        CheckMenuItem::new("Notify on error", true, state.config.notify_on_error, None);

    let quit = MenuItem::new("Quit shotpaste", true, None);

    // Assemble — order matters.
    menu.append_items(&[
        &watching,
        &last_capture,
        &pushes,
        &PredefinedMenuItem::separator(),
    ])
    .context("failed to populate tray menu header")?;
    if let Some(open) = &open_folder {
        menu.append(open).context("append Open watched folder")?;
    }
    if let Some(sub) = &folders_submenu {
        menu.append(sub).context("append folders submenu")?;
    }
    menu.append_items(&[
        &add_folder,
        &push_again,
        &open_log,
        &PredefinedMenuItem::separator(),
        &start_at_login,
        &notify_success,
        &notify_error,
        &PredefinedMenuItem::separator(),
        &quit,
    ])
    .context("failed to populate tray menu tail")?;

    Ok(MenuItems {
        _menu: menu,
        watching,
        last_capture,
        pushes,
        open_folder,
        folders_submenu,
        folder_actions,
        add_folder,
        push_again,
        open_log,
        start_at_login,
        notify_success,
        notify_error,
        quit,
    })
}

/// Build the "Watched folders ▶" submenu for the multi-dir shape. Each
/// entry is itself a submenu with `Open in file manager` + `Remove from
/// watch list` children. Returns the parent submenu and a parallel vec of
/// `(open_id, remove_id)` for dispatch matching.
fn build_folders_submenu(dirs: &[PathBuf]) -> Result<(Submenu, Vec<(MenuId, MenuId)>)> {
    let parent = Submenu::new("Watched folders", true);
    let mut actions = Vec::with_capacity(dirs.len());
    // Allow Remove only when 2+ remain — never let the user delete the last one.
    let allow_remove = dirs.len() > 1;
    for dir in dirs {
        let label = abbreviate_path(dir);
        let entry = Submenu::new(label, true);
        let open = MenuItem::new("Open in file manager", true, None);
        let remove = MenuItem::new("Remove from watch list", allow_remove, None);
        let open_id = open.id().clone();
        let remove_id = remove.id().clone();
        entry
            .append_items(&[&open, &PredefinedMenuItem::separator(), &remove])
            .context("populate per-folder submenu")?;
        parent.append(&entry).context("append per-folder submenu")?;
        actions.push((open_id, remove_id));
    }
    Ok((parent, actions))
}

fn watching_header(dirs: &[PathBuf]) -> String {
    match dirs.len() {
        0 => "Watching: (none)".to_string(),
        1 => format!("Watching: {}", abbreviate_path(&dirs[0])),
        n => format!("Watching: {} folders", n),
    }
}

fn build_tray(menu: &Menu, state: &AppState) -> Result<tray_icon::TrayIcon> {
    let icon = load_icon_color()?;

    let tooltip = build_tooltip(&state.watch_dirs);

    // `mut` is needed on macOS where we reassign with the template icon;
    // suppress the unused-mut warning on the other platforms.
    #[allow(unused_mut)]
    let mut builder = TrayIconBuilder::new()
        .with_menu(Box::new(menu.clone()))
        .with_tooltip(tooltip)
        .with_icon(icon);

    #[cfg(target_os = "macos")]
    {
        // Replace the color icon with the monochrome template variant on
        // macOS so it auto-inverts in dark mode.
        if let Ok(template) = load_icon_template() {
            builder = builder.with_icon(template).with_icon_as_template(true);
        }
    }

    builder.build().context("failed to build tray icon")
}

fn build_tooltip(dirs: &[PathBuf]) -> String {
    let version = env!("CARGO_PKG_VERSION");
    match dirs.len() {
        0 => format!("shotpaste {version} — no folders watched"),
        1 => format!(
            "shotpaste {version} — watching {}",
            abbreviate_path(&dirs[0])
        ),
        _ => {
            let list = dirs
                .iter()
                .map(|d| format!("  • {}", abbreviate_path(d)))
                .collect::<Vec<_>>()
                .join("\n");
            format!("shotpaste {version} — watching:\n{list}")
        }
    }
}

fn load_icon_color() -> Result<Icon> {
    let img = image::load_from_memory(ICON_COLOR)
        .context("failed to decode embedded color icon")?
        .into_rgba8();
    let (w, h) = img.dimensions();
    Icon::from_rgba(img.into_raw(), w, h).context("failed to build tray Icon from color png")
}

#[cfg(target_os = "macos")]
fn load_icon_template() -> Result<Icon> {
    let img = image::load_from_memory(ICON_TEMPLATE)
        .context("failed to decode embedded template icon")?
        .into_rgba8();
    let (w, h) = img.dimensions();
    Icon::from_rgba(img.into_raw(), w, h).context("failed to build tray Icon from template png")
}

/// Decoded menu action. Lets us classify a click while holding only an
/// immutable borrow on `MenuItems`, then drop it before mutating state.
enum MenuAction {
    Quit,
    OpenSingleFolder,
    OpenAt(usize),
    RemoveAt(usize),
    AddFolder,
    PushAgain,
    OpenLog,
    /// Toggle Start-at-login. Carries the *new* desired state read from
    /// the CheckMenuItem at click time.
    StartAtLogin {
        checked: bool,
    },
    NotifySuccess(bool),
    NotifyError(bool),
    Unknown,
}

fn classify_menu_event(id: &MenuId, items: &MenuItems) -> MenuAction {
    if id == items.quit.id() {
        return MenuAction::Quit;
    }
    if let Some(open) = items.open_folder.as_ref()
        && id == open.id()
    {
        return MenuAction::OpenSingleFolder;
    }
    for (idx, (open_id, remove_id)) in items.folder_actions.iter().enumerate() {
        if id == open_id {
            return MenuAction::OpenAt(idx);
        }
        if id == remove_id {
            return MenuAction::RemoveAt(idx);
        }
    }
    if id == items.add_folder.id() {
        return MenuAction::AddFolder;
    }
    if id == items.push_again.id() {
        return MenuAction::PushAgain;
    }
    if id == items.open_log.id() {
        return MenuAction::OpenLog;
    }
    if id == items.start_at_login.id() {
        return MenuAction::StartAtLogin {
            checked: items.start_at_login.is_checked(),
        };
    }
    if id == items.notify_success.id() {
        return MenuAction::NotifySuccess(items.notify_success.is_checked());
    }
    if id == items.notify_error.id() {
        return MenuAction::NotifyError(items.notify_error.is_checked());
    }
    // Touch the informational fields so the compiler doesn't think they
    // belong elsewhere; they're disabled and never fire events.
    let _ = (
        &items.watching,
        &items.last_capture,
        &items.pushes,
        &items.folders_submenu,
    );
    MenuAction::Unknown
}

#[allow(clippy::too_many_arguments)]
fn handle_menu_action(
    action: MenuAction,
    state: &mut AppState,
    control_flow: &mut ControlFlow,
    proxy: &EventLoopProxy<AppEvent>,
    tray: &mut Option<tray_icon::TrayIcon>,
    items_slot: &mut Option<MenuItems>,
    watcher_handle: &mut Option<WatcherHandle>,
) {
    match action {
        MenuAction::Quit => {
            if let Some(h) = watcher_handle.take() {
                h.shutdown();
            }
            *control_flow = ControlFlow::Exit;
        }
        MenuAction::OpenSingleFolder => {
            if let Some(dir) = state.watch_dirs.first()
                && let Err(e) = open_in_file_manager(dir)
            {
                warn!("failed to open watched folder: {e:#}");
            }
        }
        MenuAction::OpenAt(idx) => {
            if let Some(dir) = state.watch_dirs.get(idx)
                && let Err(e) = open_in_file_manager(dir)
            {
                warn!("failed to open watched folder: {e:#}");
            }
        }
        MenuAction::RemoveAt(idx) => {
            if state.watch_dirs.len() > 1 && idx < state.watch_dirs.len() {
                state.watch_dirs.remove(idx);
                persist_watch_dirs(state);
                restart_watcher(state, proxy, watcher_handle);
                rebuild_menu(state, tray, items_slot);
            }
        }
        MenuAction::AddFolder => {
            if state.picker_open {
                return;
            }
            state.picker_open = true;
            let proxy = proxy.clone();
            std::thread::Builder::new()
                .name("shotpaste-picker".into())
                .spawn(move || {
                    let start = dirs::picture_dir()
                        .or_else(dirs::home_dir)
                        .unwrap_or_else(|| PathBuf::from("."));
                    let picked = rfd::FileDialog::new()
                        .set_title("shotpaste — pick a folder to watch")
                        .set_directory(start)
                        .pick_folder();
                    let _ = proxy.send_event(AppEvent::PickedFolder(picked));
                })
                .ok();
        }
        MenuAction::PushAgain => {
            if let Some(path) = state.last_path.clone() {
                std::thread::Builder::new()
                    .name("shotpaste-replay".into())
                    .spawn(move || {
                        if let Err(e) = crate::clipboard::write_png(&path) {
                            warn!("manual replay failed: {e:#}");
                        }
                    })
                    .ok();
            }
        }
        MenuAction::OpenLog => {
            if let Some(path) = crate::log_path()
                && let Err(e) = open_in_file_manager(&path)
            {
                warn!("failed to open log file: {e:#}");
            }
        }
        MenuAction::StartAtLogin { checked } => {
            unsafe { std::env::set_var("SHOTPASTE_FROM_TRAY", "1") };
            let outcome = if checked {
                crate::installer::install()
            } else {
                crate::installer::uninstall(false)
            };
            unsafe { std::env::remove_var("SHOTPASTE_FROM_TRAY") };
            if let Err(e) = outcome {
                warn!("toggle 'Start at login' failed: {e:#}");
                if let Some(it) = items_slot.as_ref() {
                    it.start_at_login.set_checked(installer_installed());
                }
            }
        }
        MenuAction::NotifySuccess(checked) => {
            state.config.notify_on_success = checked;
            let _ = state.config.save();
        }
        MenuAction::NotifyError(checked) => {
            state.config.notify_on_error = checked;
            let _ = state.config.save();
        }
        MenuAction::Unknown => {}
    }
}

fn maybe_flush_toast(state: &mut AppState) {
    let Some(deadline) = state.toast_flush_at else {
        return;
    };
    if Instant::now() < deadline {
        return;
    }
    let count = state.toast_pending;
    state.toast_pending = 0;
    state.toast_flush_at = None;
    let path = state.toast_path.take();
    if !state.config.notify_on_success || count == 0 {
        return;
    }
    if count == 1 {
        if let Some(p) = path {
            toast::success_single(&p);
        }
    } else {
        toast::success_burst(count);
    }
}

fn refresh_status_items(items: &MenuItems, state: &AppState) {
    items
        .last_capture
        .set_text(format_last_capture(state.last_capture_at));
    items
        .pushes
        .set_text(format!("Pushes this session: {}", state.pushes));
    items.push_again.set_enabled(state.last_path.is_some());
}

/// Save the current `watch_dirs` to config.toml, best-effort.
fn persist_watch_dirs(state: &mut AppState) {
    state.config.watch_dirs = state.watch_dirs.clone();
    if let Err(e) = state.config.save() {
        warn!("could not persist watch_dirs: {e:#}");
    }
}

/// Tear down the running watcher (~500 ms) and spawn a fresh one with
/// `state.watch_dirs`. Called after every Add/Remove mutation.
fn restart_watcher(
    state: &AppState,
    proxy: &EventLoopProxy<AppEvent>,
    handle_slot: &mut Option<WatcherHandle>,
) {
    if let Some(old) = handle_slot.take() {
        old.shutdown();
    }
    if state.watch_dirs.is_empty() {
        return; // No-op — should be unreachable because Remove blocks at len==1.
    }
    *handle_slot = Some(spawn_watcher(state.watch_dirs.clone(), proxy.clone()));
}

/// Tear down the current menu and build a fresh one reflecting the new
/// `state.watch_dirs`. Swaps it into the tray atomically.
fn rebuild_menu(
    state: &AppState,
    tray: &mut Option<tray_icon::TrayIcon>,
    items_slot: &mut Option<MenuItems>,
) {
    match build_menu(state) {
        Ok(new) => {
            if let Some(t) = tray.as_ref() {
                t.set_menu(Some(Box::new(new._menu.clone())));
                if let Err(e) = t.set_tooltip(Some(build_tooltip(&state.watch_dirs))) {
                    warn!("could not update tray tooltip: {e:#}");
                }
            }
            *items_slot = Some(new);
        }
        Err(e) => warn!("could not rebuild menu: {e:#}"),
    }
}

fn format_last_capture(at: Option<SystemTime>) -> String {
    let Some(at) = at else {
        return "Last capture: never".to_string();
    };
    let secs = SystemTime::now()
        .duration_since(at)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let human = if secs < 60 {
        format!("{}s ago", secs)
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else {
        format!("{}h ago", secs / 3600)
    };
    format!("Last capture: {human}")
}

fn abbreviate_path(p: &Path) -> String {
    let raw = p.display().to_string();
    let collapsed = if let Some(home) = dirs::home_dir() {
        let home_str = home.display().to_string();
        if let Some(rest) = raw.strip_prefix(&home_str) {
            format!("~{}", rest)
        } else {
            raw
        }
    } else {
        raw
    };
    if collapsed.len() > 48 {
        let chars: Vec<char> = collapsed.chars().collect();
        let head: String = chars.iter().take(22).collect();
        let tail: String = chars
            .iter()
            .rev()
            .take(22)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        format!("{head}…{tail}")
    } else {
        collapsed
    }
}

fn installer_installed() -> bool {
    #[cfg(target_os = "windows")]
    {
        if let Some(local) = dirs::data_local_dir() {
            return local.join("shotpaste").join("shotpaste-watch.vbs").exists();
        }
        false
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = dirs::home_dir() {
            return home
                .join("Library/LaunchAgents")
                .join("dev.shotpaste.watcher.plist")
                .exists();
        }
        false
    }
    #[cfg(target_os = "linux")]
    {
        if let Some(cfg) = dirs::config_dir() {
            return cfg.join("systemd/user/shotpaste.service").exists();
        }
        false
    }
}

#[cfg(target_os = "windows")]
fn open_in_file_manager(path: &Path) -> Result<()> {
    use std::process::Command;
    Command::new("explorer.exe")
        .arg(path)
        .spawn()
        .context("failed to spawn explorer.exe")?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn open_in_file_manager(path: &Path) -> Result<()> {
    use std::process::Command;
    Command::new("open")
        .arg(path)
        .spawn()
        .context("failed to spawn open")?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn open_in_file_manager(path: &Path) -> Result<()> {
    use std::process::Command;
    Command::new("xdg-open")
        .arg(path)
        .spawn()
        .context("failed to spawn xdg-open")?;
    Ok(())
}
