//! The macOS status-bar (system tray) menu and its live synchronization with
//! engine/app state.
//!
//! The menu order is fixed (design decision 20): Enable/Disable EQ toggle |
//! Bypass (checkbox) | separator | "Profile" submenu | separator | Show Window
//! | Quit ParaEQ. The toggle text and bypass checkmark mirror the live engine
//! state; the Profile submenu carries a `CheckMenuItem` per saved profile
//! (checked = active) or a single disabled "(no profiles)" item when empty.
//!
//! THREADING (verified against Tauri 2.11 / muda + tao): AppKit menu and tray
//! mutations are main-thread-affine on macOS. [`sync_tray`] is called from the
//! background forwarder thread (via `engine_bridge::publish`), so every
//! mutation is scheduled onto the main thread with
//! [`tauri::AppHandle::run_on_main_thread`]. Building menus likewise happens on
//! the main thread inside that closure.
//!
//! We keep the `toggle` and `bypass` item handles in managed state so their
//! text/checked state can be updated in place on every publish. The Profile
//! submenu, by contrast, has a variable item set, so it is rebuilt wholesale
//! via `TrayIcon::set_menu` -- but ONLY when the profile list or active profile
//! actually changed (coarse but simple; see [`menu_fingerprint`]). The rebuild
//! reuses the same `toggle`/`bypass` handles so they stay valid across it.

use crate::state::AppState;
use std::sync::Mutex;
use tauri::menu::{
    CheckMenuItem, CheckMenuItemBuilder, Menu, MenuBuilder, MenuEvent, MenuItem, MenuItemBuilder,
    Submenu, SubmenuBuilder,
};
use tauri::tray::{TrayIcon, TrayIconBuilder};
use tauri::{Manager, Wry};

/// Item handles the forwarder needs for live sync. Stored via `app.manage()`.
pub struct TrayHandles {
    pub bypass: CheckMenuItem<Wry>,
    pub toggle: MenuItem<Wry>, // "Enable EQ" / "Disable EQ"
    pub tray: TrayIcon<Wry>,
    /// Fingerprint of the last-built Profile submenu, to detect when a rebuild
    /// is needed (see [`menu_fingerprint`]).
    profile_names: Mutex<Vec<String>>,
}

/// Build the tray icon + menu from the initial [`AppState`]. Called ONCE from
/// `.setup` after `AppShared` is managed. The returned [`TrayHandles`] must be
/// `app.manage()`d so [`sync_tray`] can find it.
///
/// Menu order (decision 20): toggle-eq | bypass (check) | separator | "Profile"
/// submenu | separator | "Show Window" | "Quit ParaEQ". Icon: the default
/// window icon. Left click opens the menu (`show_menu_on_left_click(true)`).
pub fn build_tray(app: &tauri::AppHandle, state: &AppState) -> tauri::Result<TrayHandles> {
    let enabled = state.engine.enabled;
    let bypass_checked = state.engine.bypass;

    let toggle = MenuItemBuilder::with_id("toggle-eq", toggle_text(enabled)).build(app)?;
    let bypass = CheckMenuItemBuilder::with_id("bypass", "Bypass")
        .checked(bypass_checked)
        .build(app)?;
    let menu = build_menu(
        app,
        &toggle,
        &bypass,
        &state.profiles,
        state.active_profile.as_deref(),
    )?;

    let mut builder = TrayIconBuilder::new()
        .menu(&menu)
        .show_menu_on_left_click(true)
        .tooltip(tooltip_text(bypass_checked))
        .on_menu_event(handle_menu_event);
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    let tray = builder.build(app)?;

    Ok(TrayHandles {
        bypass,
        toggle,
        tray,
        profile_names: Mutex::new(menu_fingerprint(
            &state.profiles,
            state.active_profile.as_deref(),
        )),
    })
}

/// Called on every publish (via `engine_bridge::publish`): update the toggle
/// text and bypass checkmark to the live engine state, refresh the tooltip, and
/// -- only when the profile list or active profile changed -- rebuild the whole
/// menu so the Profile submenu and its checkmark stay accurate. No-op if the
/// tray has not been built yet.
///
/// All mutations run on the main thread (macOS menu affinity); this function
/// only schedules them and returns immediately.
pub fn sync_tray(app: &tauri::AppHandle, state: &AppState) {
    let enabled = state.engine.enabled;
    let bypass = state.engine.bypass;
    let profiles = state.profiles.clone();
    let active = state.active_profile.clone();
    let app = app.clone();
    let _ = app.clone().run_on_main_thread(move || {
        let Some(handles) = app.try_state::<TrayHandles>() else {
            return;
        };
        let _ = handles.toggle.set_text(toggle_text(enabled));
        let _ = handles.bypass.set_checked(bypass);
        let _ = handles.tray.set_tooltip(Some(tooltip_text(bypass)));

        let fingerprint = menu_fingerprint(&profiles, active.as_deref());
        let mut last = handles.profile_names.lock().unwrap();
        if *last != fingerprint {
            match build_menu(
                &app,
                &handles.toggle,
                &handles.bypass,
                &profiles,
                active.as_deref(),
            ) {
                Ok(menu) => match handles.tray.set_menu(Some(menu)) {
                    Ok(()) => *last = fingerprint,
                    Err(e) => log::warn!("failed to update tray menu: {e}"),
                },
                Err(e) => log::warn!("failed to rebuild tray menu: {e}"),
            }
        }
    });
}

/// The full tray menu, reusing the caller's `toggle`/`bypass` handles (so those
/// stay valid across a `set_menu` rebuild) and a freshly built Profile submenu.
fn build_menu(
    app: &tauri::AppHandle,
    toggle: &MenuItem<Wry>,
    bypass: &CheckMenuItem<Wry>,
    profiles: &[String],
    active: Option<&str>,
) -> tauri::Result<Menu<Wry>> {
    let profile_submenu = build_profile_submenu(app, profiles, active)?;
    let show = MenuItemBuilder::with_id("show", "Show Window").build(app)?;
    let quit = MenuItemBuilder::with_id("quit", "Quit ParaEQ").build(app)?;
    MenuBuilder::new(app)
        .item(toggle)
        .item(bypass)
        .separator()
        .item(&profile_submenu)
        .separator()
        .item(&show)
        .item(&quit)
        .build()
}

/// The "Profile" submenu: a `CheckMenuItem` per saved profile (id
/// `profile:<name>`, checked = active), or a single disabled "(no profiles)"
/// item when the list is empty.
fn build_profile_submenu(
    app: &tauri::AppHandle,
    profiles: &[String],
    active: Option<&str>,
) -> tauri::Result<Submenu<Wry>> {
    let mut builder = SubmenuBuilder::new(app, "Profile");
    if profiles.is_empty() {
        let none = MenuItemBuilder::with_id("profile-none", "(no profiles)")
            .enabled(false)
            .build(app)?;
        builder = builder.item(&none);
    } else {
        for name in profiles {
            let item = CheckMenuItemBuilder::with_id(format!("profile:{name}"), name)
                .checked(active == Some(name.as_str()))
                .build(app)?;
            builder = builder.item(&item);
        }
    }
    builder.build()
}

/// Positional change-detection key for the Profile submenu: element 0 is the
/// active profile (empty when none), followed by the profile names in order.
/// Two states share a fingerprint iff they have the same active profile AND the
/// same ordered profile list -- exactly the conditions under which the submenu
/// (item set + checkmark) is unchanged, so we can skip the coarse `set_menu`
/// rebuild. Because the comparison is positional (active is always element 0),
/// there is no collision between an active name and a profile name.
fn menu_fingerprint(profiles: &[String], active: Option<&str>) -> Vec<String> {
    let mut fp = Vec::with_capacity(profiles.len() + 1);
    fp.push(active.unwrap_or_default().to_string());
    fp.extend(profiles.iter().cloned());
    fp
}

/// Toggle item label: "Disable EQ" iff the engine is currently enabled.
fn toggle_text(enabled: bool) -> &'static str {
    if enabled {
        "Disable EQ"
    } else {
        "Enable EQ"
    }
}

/// Tray tooltip, flipped when bypassed.
fn tooltip_text(bypass: bool) -> &'static str {
    if bypass {
        "ParaEQ (bypassed)"
    } else {
        "ParaEQ"
    }
}

/// Tray menu click dispatch. Reuses the command bodies directly (NOT via
/// `invoke`) so the tray and the UI share one apply path. Tray menu events fire
/// on the main thread.
fn handle_menu_event(app: &tauri::AppHandle, event: MenuEvent) {
    match event.id().as_ref() {
        "toggle-eq" => {
            // Act on the displayed (live) enabled state, matching the label.
            let enabled = {
                let shared = app.state::<crate::state::AppShared>();
                crate::engine_bridge::current_engine_state(&shared).enabled
            };
            let res = if enabled {
                crate::commands::engine_disable(app.clone())
            } else {
                crate::commands::engine_enable(app.clone())
            };
            if let Err(e) = res {
                log::warn!("tray toggle-eq failed: {e}");
            }
        }
        "bypass" => {
            // The OS has already flipped the checkmark; read the new state.
            let checked = app
                .state::<TrayHandles>()
                .bypass
                .is_checked()
                .unwrap_or(false);
            if let Err(e) = crate::commands::engine_set_bypass(app.clone(), checked) {
                log::warn!("tray bypass toggle failed: {e}");
            }
        }
        "show" => {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
        }
        "quit" => {
            app.exit(0);
        }
        other => {
            if let Some(name) = other.strip_prefix("profile:") {
                if let Err(e) = crate::commands::profiles_activate(app.clone(), name.to_string()) {
                    log::warn!("tray profile activation failed: {e}");
                }
            }
        }
    }
}
