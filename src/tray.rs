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

use crate::config::Config;
use crate::notify as toast;
use crate::watcher::{self, WatcherSink};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
use tracing::{error, warn};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
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

/// Menu items kept on hand so the dispatcher can compare `event.id`
/// against each one (and flip `set_checked` / `set_text`).
struct MenuItems {
    _menu: Menu,
    watching: MenuItem,
    last_capture: MenuItem,
    pushes: MenuItem,
    open_folder: MenuItem,
    push_again: MenuItem,
    open_log: MenuItem,
    start_at_login: CheckMenuItem,
    notify_success: CheckMenuItem,
    notify_error: CheckMenuItem,
    quit: MenuItem,
}

/// Mutable session state held on the main thread.
struct AppState {
    watch_dir: PathBuf,
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
}

/// Run the tray UI. Acquires the single-instance lock implicitly (caller
/// holds it), spawns the watcher thread, then blocks until the user picks
/// "Quit" or the event loop is otherwise terminated.
pub fn run(dir: &Path) -> Result<()> {
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

    // Spawn the watcher on a background thread. The sink wakes us via
    // `EventLoopProxy::send_event`.
    let watcher_dir = dir.to_path_buf();
    let sink = ChannelSink(Mutex::new(proxy.clone()));
    std::thread::Builder::new()
        .name("shotpaste-watcher".into())
        .spawn(move || {
            if let Err(e) = watcher::run(&watcher_dir, sink) {
                error!("watcher exited with error: {e:#}");
            }
        })
        .context("failed to spawn watcher thread")?;

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
        watch_dir: dir.to_path_buf(),
        config: Config::load(),
        last_capture_at: None,
        last_path: None,
        pushes: 0,
        toast_pending: 0,
        toast_path: None,
        toast_flush_at: None,
    };

    // tray_icon and menu live in Options so they're constructed inside
    // StartCause::Init (per tauri-apps/tray-icon#90). The leading underscore
    // silences a false-positive `unused_assignments` — dropping the TrayIcon
    // would remove the icon from the OS, so we must hold it for the whole
    // event loop.
    let mut _tray_icon: Option<tray_icon::TrayIcon> = None;
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
                    Ok(t) => _tray_icon = Some(t),
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
                if let Some(items) = items.as_ref() {
                    handle_menu_event(&e.id, items, &mut state, control_flow);
                }
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
                if let Some(items) = items.as_ref() {
                    refresh_status_items(items, &state);
                }
            }

            Event::UserEvent(AppEvent::Failed { path, error }) if state.config.notify_on_error => {
                toast::error(&path, &error);
            }

            Event::UserEvent(AppEvent::Tick) => {
                if let Some(items) = items.as_ref() {
                    items
                        .last_capture
                        .set_text(format_last_capture(state.last_capture_at));
                }
            }

            _ => {}
        }
    })
}

fn build_menu(state: &AppState) -> Result<MenuItems> {
    let menu = Menu::new();

    let watching = MenuItem::new(
        format!("Watching: {}", abbreviate_path(&state.watch_dir)),
        false,
        None,
    );
    let last_capture = MenuItem::new(format_last_capture(state.last_capture_at), false, None);
    let pushes = MenuItem::new(
        format!("Pushes this session: {}", state.pushes),
        false,
        None,
    );
    let open_folder = MenuItem::new("Open watched folder", true, None);
    let push_again = MenuItem::new("Push last screenshot again", false, None);
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

    menu.append_items(&[
        &watching,
        &last_capture,
        &pushes,
        &PredefinedMenuItem::separator(),
        &open_folder,
        &push_again,
        &open_log,
        &PredefinedMenuItem::separator(),
        &start_at_login,
        &notify_success,
        &notify_error,
        &PredefinedMenuItem::separator(),
        &quit,
    ])
    .context("failed to populate tray menu")?;

    Ok(MenuItems {
        _menu: menu,
        watching,
        last_capture,
        pushes,
        open_folder,
        push_again,
        open_log,
        start_at_login,
        notify_success,
        notify_error,
        quit,
    })
}

fn build_tray(menu: &Menu, state: &AppState) -> Result<tray_icon::TrayIcon> {
    let icon = load_icon_color()?;

    let tooltip = format!(
        "shotpaste {} — watching {}",
        env!("CARGO_PKG_VERSION"),
        abbreviate_path(&state.watch_dir)
    );

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

fn handle_menu_event(
    id: &MenuId,
    items: &MenuItems,
    state: &mut AppState,
    control_flow: &mut ControlFlow,
) {
    if id == items.quit.id() {
        *control_flow = ControlFlow::Exit;
    } else if id == items.open_folder.id() {
        if let Err(e) = open_in_file_manager(&state.watch_dir) {
            warn!("failed to open watched folder: {e:#}");
        }
    } else if id == items.push_again.id() {
        if let Some(path) = state.last_path.clone() {
            // Run on a side thread — clipboard::write_png blocks for tens of
            // ms on Windows under contention and we don't want to stall the
            // event loop.
            std::thread::Builder::new()
                .name("shotpaste-replay".into())
                .spawn(move || {
                    if let Err(e) = crate::clipboard::write_png(&path) {
                        warn!("manual replay failed: {e:#}");
                    }
                })
                .ok();
        }
    } else if id == items.open_log.id() {
        if let Some(path) = crate::log_path() {
            if let Err(e) = open_in_file_manager(&path) {
                warn!("failed to open log file: {e:#}");
            }
        }
    } else if id == items.start_at_login.id() {
        let checked = items.start_at_login.is_checked();
        // Mark our process so installer::install doesn't auto-start a
        // duplicate watcher under wscript/launchd/systemd.
        unsafe { std::env::set_var("SHOTPASTE_FROM_TRAY", "1") };
        let outcome = if checked {
            crate::installer::install()
        } else {
            crate::installer::uninstall(false)
        };
        unsafe { std::env::remove_var("SHOTPASTE_FROM_TRAY") };
        if let Err(e) = outcome {
            warn!("toggle 'Start at login' failed: {e:#}");
            // Roll the visible checkbox back to truth so it doesn't lie.
            items.start_at_login.set_checked(installer_installed());
        }
    } else if id == items.notify_success.id() {
        state.config.notify_on_success = items.notify_success.is_checked();
        let _ = state.config.save();
    } else if id == items.notify_error.id() {
        state.config.notify_on_error = items.notify_error.is_checked();
        let _ = state.config.save();
    }
    // (informational rows — watching/last_capture/pushes — are disabled and
    // don't fire menu events.)
    let _ = &items.watching;
    let _ = &items.last_capture;
    let _ = &items.pushes;
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
        // Truncate middle, keep ends.
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
    // Best-effort probe — none of the installer modules currently return a
    // bool, so check the on-disk artifacts directly. False on any error.
    #[cfg(target_os = "windows")]
    {
        // The Scheduled Task is the source of truth, but querying it
        // requires PowerShell. The VBS shim is a cheap proxy that's
        // written next to the task on every install.
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
