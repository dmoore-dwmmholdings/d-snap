//! The tray companion (DSNA-128): a tray icon whose left click toggles a small popover
//! window (`tray`, the same frontend rendering `TrayView`), a menu with Open and Quit, and
//! "keep running in the tray": closing the main window hides it when
//! `GlobalSettings::keep_in_tray` is on.

use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{
    AppHandle, Manager, PhysicalPosition, Rect, WebviewUrl, WebviewWindow, WebviewWindowBuilder,
    Window, WindowEvent,
};

use crate::commands::AppState;

/// Tray icon id (the frontend updates its title and tooltip by this id).
pub const TRAY_ID: &str = "dsnap-tray";
/// Label of the popover window.
pub const POPOVER: &str = "tray";
const POPOVER_W: f64 = 360.0;
const POPOVER_H: f64 = 500.0;
/// Gap between the tray icon and the popover, in logical pixels.
const GAP: f64 = 6.0;

/// Add the tray icon. Errors are returned to the caller, which runs without a tray then.
pub fn install(app: &AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Open d-snap", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit d-snap", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &quit])?;
    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .tooltip("d-snap")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "open" => show_main(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                rect,
                ..
            } = event
            {
                toggle_popover(tray.app_handle(), rect);
            }
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    Ok(())
}

/// Show, unminimize and focus the main window.
pub fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

fn toggle_popover(app: &AppHandle, icon: Rect) {
    let window = match app.get_webview_window(POPOVER) {
        Some(w) => {
            if w.is_visible().unwrap_or(false) {
                let _ = w.hide();
                return;
            }
            w
        }
        None => match WebviewWindowBuilder::new(
            app,
            POPOVER,
            WebviewUrl::App("index.html?view=tray".into()),
        )
        .title("d-snap")
        .inner_size(POPOVER_W, POPOVER_H)
        .resizable(false)
        .decorations(false)
        .skip_taskbar(true)
        .always_on_top(true)
        .visible(false)
        .build()
        {
            Ok(w) => w,
            Err(e) => {
                eprintln!("d-snap: cannot open the tray popover: {e}");
                return;
            }
        },
    };
    place(&window, icon);
    let _ = window.show();
    let _ = window.set_focus();
}

/// Put the popover next to the tray icon: below it when the icon is in a top bar (macOS),
/// above it when it is in a bottom taskbar (Windows), kept on the icon's monitor.
fn place(window: &WebviewWindow, icon: Rect) {
    let scale = window.scale_factor().unwrap_or(1.0);
    let pos = icon.position.to_physical::<f64>(scale);
    let size = icon.size.to_physical::<f64>(scale);
    let (w, h, gap) = (POPOVER_W * scale, POPOVER_H * scale, GAP * scale);
    let monitor = window
        .monitor_from_point(pos.x, pos.y)
        .ok()
        .flatten()
        .or_else(|| window.primary_monitor().ok().flatten());
    let (mx, my, mw, mh) = monitor.map_or((0.0, 0.0, f64::MAX, f64::MAX), |m| {
        let (p, s) = (m.position(), m.size());
        (
            f64::from(p.x),
            f64::from(p.y),
            f64::from(s.width),
            f64::from(s.height),
        )
    });
    let below = pos.y < my + mh / 2.0;
    let y = if below {
        pos.y + size.height + gap
    } else {
        pos.y - h - gap
    };
    let x = (pos.x + size.width / 2.0 - w / 2.0).clamp(mx + gap, (mx + mw - w - gap).max(mx));
    let _ = window.set_position(PhysicalPosition::new(x, y));
}

/// Window events: the popover hides when it loses focus; closing the main window hides it
/// instead when "keep running in the tray" is on.
pub fn on_window_event(window: &Window, event: &WindowEvent) {
    match (window.label(), event) {
        (POPOVER, WindowEvent::Focused(false)) => {
            let _ = window.hide();
        }
        ("main", WindowEvent::CloseRequested { api, .. }) => {
            let keep = window
                .try_state::<AppState>()
                .and_then(|s| s.0.get_global_settings().ok())
                .is_some_and(|g| g.keep_in_tray);
            let has_tray = window.app_handle().tray_by_id(TRAY_ID).is_some();
            if keep && has_tray {
                api.prevent_close();
                let _ = window.hide();
            }
        }
        _ => {}
    }
}
