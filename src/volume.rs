//! Tray icon with Enable/Disable + Show + Quit.
//!
//! The tray toggles the shared `Config.enabled`, which the engine polls and
//! the UI reflects — one source of truth, no message plumbing.

use anyhow::Result;
use parking_lot::RwLock;
use std::sync::Arc;

use crate::config::Config;

fn set_enabled(cfg: &Arc<RwLock<Config>>, on: bool) {
    let mut c = cfg.write();
    c.enabled = on;
    let _ = c.save();
}

#[cfg(target_os = "windows")]
pub fn run_tray(cfg: Arc<RwLock<Config>>) -> Result<()> {
    // tray-icon needs a thread with an OS message loop; the UI owns the main one.
    std::thread::Builder::new()
        .name("tray".into())
        .spawn(move || {
            if let Err(e) = tray_thread(cfg) {
                log::error!("tray: {e:#}");
            }
        })?;
    Ok(())
}

/// macOS: NSStatusItem must be created on the main thread and is serviced by
/// the app runloop that eframe starts, so we only *build* it here (non-blocking)
/// and poll menu events from the UI each frame (`poll_menu_events`).
#[cfg(target_os = "macos")]
pub fn run_tray(cfg: Arc<RwLock<Config>>) -> Result<()> {
    mac_tray_state::init(cfg)?;
    Ok(())
}

#[cfg(target_os = "macos")]
pub(crate) mod mac_tray_state {
    use super::set_enabled;
    use anyhow::Result;
    use parking_lot::RwLock;
    use std::{cell::RefCell, sync::Arc};

    use crate::config::Config;

    struct TrayItems {
        cfg: Arc<RwLock<Config>>,
        enable_item: tray_icon::menu::CheckMenuItem,
        // Keep the native status item alive for the lifetime of the app.
        _tray: tray_icon::TrayIcon,
    }

    thread_local! {
        static TRAY: RefCell<Option<TrayItems>> = RefCell::new(None);
    }

    pub fn init(cfg: Arc<RwLock<Config>>) -> Result<()> {
        use tray_icon::menu::{CheckMenuItem, Menu, MenuItem};
        use tray_icon::TrayIconBuilder;

        let enable_item =
            CheckMenuItem::with_id("enable", "Enabled", true, cfg.read().enabled, None);
        let show_item = MenuItem::with_id("show", "Show window", true, None);
        let quit_item = MenuItem::with_id("quit", "Quit", true, None);
        let menu = Menu::new();
        menu.append_items(&[&enable_item.clone(), &show_item, &quit_item])?;

        let icon = tray_icon::Icon::from_rgba(
            crate::icon::icon_rgba(),
            crate::icon::ICON_W,
            crate::icon::ICON_H,
        )?;

        let tray = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("Baffle — smart loudness")
            .with_icon(icon)
            .with_menu_on_left_click(true)
            .build()?;

        TRAY.with(|slot| {
            *slot.borrow_mut() = Some(TrayItems {
                cfg,
                enable_item,
                _tray: tray,
            });
        });
        Ok(())
    }

    /// Called from the UI loop each frame (main thread).
    pub fn poll_menu_events() {
        TRAY.with(|slot| {
            let tray = slot.borrow();
            let Some(state) = tray.as_ref() else {
                return;
            };
            while let Ok(ev) = tray_icon::menu::MenuEvent::receiver().try_recv() {
                match ev.id().0.as_str() {
                    "enable" => {
                        let on = state.enable_item.is_checked();
                        set_enabled(&state.cfg, on);
                    }
                    "quit" => crate::shutdown_and_exit(),
                    _ => {}
                }
            }
        });
    }
}

#[cfg(target_os = "linux")]
pub fn run_tray(cfg: Arc<RwLock<Config>>) -> Result<()> {
    std::thread::Builder::new()
        .name("tray".into())
        .spawn(move || {
            if let Err(e) = ksni_tray(cfg) {
                log::error!("tray: {e:#}");
            }
        })?;
    Ok(())
}

// ---- Windows / macOS (tray-icon crate) -------------------------------------

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn tray_thread(cfg: Arc<RwLock<Config>>) -> Result<()> {
    use tray_icon::menu::{CheckMenuItem, Menu, MenuItem};
    use tray_icon::TrayIconBuilder;

    let enable_item = CheckMenuItem::with_id("enable", "Enabled", true, true, None);
    let show_item = MenuItem::with_id("show", "Show window", true, None);
    let quit_item = MenuItem::with_id("quit", "Quit", true, None);
    let menu = Menu::new();
    menu.append_items(&[&enable_item, &show_item, &quit_item])?;

    let icon = tray_icon::Icon::from_rgba(
        crate::icon::icon_rgba(),
        crate::icon::ICON_W,
        crate::icon::ICON_H,
    )?;

    let _tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("Baffle — smart loudness")
        .with_icon(icon)
        .with_menu_on_left_click(true)
        .build()?;

    let menu_rx = tray_icon::menu::MenuEvent::receiver();
    let mut last_shown_enabled = cfg.read().enabled;

    loop {
        // Reflect engine/UI-side changes onto the checkmark (same thread —
        // muda menu items are not Send).
        let on = cfg.read().enabled;
        if on != last_shown_enabled {
            enable_item.set_checked(on);
            last_shown_enabled = on;
        }

        while let Ok(ev) = menu_rx.try_recv() {
            match ev.id().0.as_str() {
                "enable" => {
                    // CheckMenuItem already toggled visually; persist the new state.
                    let on = enable_item.is_checked();
                    set_enabled(&cfg, on);
                    last_shown_enabled = on;
                }
                "show" => { /* the window is always shown in v0.1 */ }
                "quit" => crate::shutdown_and_exit(),
                _ => {}
            }
        }

        std::thread::sleep(std::time::Duration::from_millis(40));

        #[cfg(target_os = "windows")]
        pump_windows();
    }
}

#[cfg(target_os = "windows")]
fn pump_windows() {
    // Minimal non-blocking Win32 message pump so tray/menu events flow.
    #[repr(C)]
    struct Msg {
        hwnd: isize,
        message: u32,
        wparam: usize,
        lparam: isize,
        time: u32,
        pt_x: i32,
        pt_y: i32,
    }
    unsafe extern "system" {
        fn PeekMessageW(lpmsg: *mut Msg, hwnd: isize, min: u32, max: u32, remove: u32) -> i32;
        fn TranslateMessage(lpmsg: *const Msg) -> i32;
        fn DispatchMessageW(lpmsg: *const Msg) -> isize;
    }
    const PM_REMOVE: u32 = 1;
    let mut msg = unsafe { std::mem::zeroed::<Msg>() };
    unsafe {
        while PeekMessageW(&mut msg, 0, 0, 0, PM_REMOVE) != 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

// ---- Linux (ksni / StatusNotifierItem) -------------------------------------

#[cfg(target_os = "linux")]
fn ksni_tray(cfg: Arc<RwLock<Config>>) -> Result<()> {
    use ksni::blocking::TrayMethods;
    use ksni::menu::*;
    use std::sync::mpsc;

    struct BaffleTray {
        cfg: Arc<RwLock<Config>>,
        reload_tx: mpsc::Sender<()>,
    }

    impl ksni::Tray for BaffleTray {
        fn id(&self) -> String {
            "baffle".into()
        }

        fn title(&self) -> String {
            if self.cfg.read().enabled {
                "Baffle — active".into()
            } else {
                "Baffle — paused".into()
            }
        }

        fn icon_pixmap(&self) -> Vec<ksni::Icon> {
            let rgba = crate::icon::icon_rgba();
            let mut argb = Vec::with_capacity(rgba.len());
            for px in rgba.chunks_exact(4) {
                argb.push(px[3]); // A
                argb.push(px[0]); // R
                argb.push(px[1]); // G
                argb.push(px[2]); // B
            }
            vec![ksni::Icon {
                width: crate::icon::ICON_W as i32,
                height: crate::icon::ICON_H as i32,
                data: argb,
            }]
        }

        fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
            let enabled = self.cfg.read().enabled;
            vec![
                CheckmarkItem {
                    label: "Enabled".into(),
                    checked: enabled,
                    activate: Box::new(|tray: &mut Self| {
                        let now = !tray.cfg.read().enabled;
                        set_enabled(&tray.cfg, now);
                        let _ = tray.reload_tx.send(());
                    }),
                    ..Default::default()
                }
                .into(),
                StandardItem {
                    label: "Quit".into(),
                    activate: Box::new(|_tray: &mut Self| {
                        crate::shutdown_and_exit();
                    }),
                    ..Default::default()
                }
                .into(),
            ]
        }
    }

    let (reload_tx, reload_rx) = mpsc::channel::<()>();
    let service = BaffleTray { cfg, reload_tx };
    let handle = service.spawn().map_err(|e| anyhow!("ksni: {e}"))?;
    // Refresh tray visuals after any toggle.
    while reload_rx.recv().is_ok() {
        handle.update(|_| {});
    }
    Ok(())
}
