// Hide the console window on Windows release builds so launching the exe
// shows only the Baffle window (debug builds keep the console for logs).
#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

use anyhow::Result;
use parking_lot::RwLock;
use std::sync::Arc;

pub mod audio;
pub mod config;
pub mod dsp;
pub mod icon;
pub mod ui;
pub mod volume;

/// Live telemetry from the audio engine, consumed by the UI at ~20 Hz.
#[derive(Clone, Copy, Debug, Default)]
pub struct Telemetry {
    /// Current gains per analysis band, 0..1 (1 = untouched).
    pub band_gains: [f32; dsp::BANDS],
    /// Per-band signal levels (dBFS-ish).
    pub band_levels_db: [f32; dsp::BANDS],
    /// Short-term RMS level (dBFS) of the analysis tap.
    pub rms_db: f32,
    /// Current loudness estimate (LUBS ≈ dBFS-ish LUFS).
    pub loudness: f32,
    /// Long-term loudness the control loop is steering towards.
    pub anchor: f32,
    /// Total gain currently applied to the endpoint (0..1).
    pub applied: f32,
    /// Current controller action in dB (negative = clamping, positive = lifting).
    pub action_db: f32,
    /// True when a loud event (explosion) is being clamped.
    pub ducking: bool,
    /// True when quiet dialogue is being lifted.
    pub lifting: bool,
}

pub type SharedTelemetry = Arc<RwLock<Telemetry>>;

/// Set when the user closes the window: the engine restores the user's
/// endpoint volume and stops applying; the process then exits (tray included).
pub static SHUTDOWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// Acknowledged by the engine after it restored the volume (or it never will
/// if the engine already died); main waits for it before exiting.
pub static SHUTDOWN_DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(windows)]
mod single_instance {
    //! Named-mutex single-instance guard. A second launch focuses the
    //! existing window and exits immediately.

    use anyhow::{anyhow, Result};
    use windows::core::w;
    use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE};
    use windows::Win32::System::Threading::CreateMutexW;

    const MUTEX_NAME: windows::core::PCWSTR = w!("Local\\BaffleSingleInstanceMutex");

    /// Holds the mutex for the process lifetime. Returns Err when another
    /// instance is already running.
    pub fn acquire() -> Result<HANDLE> {
        // SAFETY: no special privileges; we only hold the handle open.
        let h = unsafe { CreateMutexW(None, false, MUTEX_NAME) }
            .map_err(|e| anyhow!("CreateMutexW: {e}"))?;
        // ERROR_ALREADY_EXISTS means the mutex existed -> another instance.
        if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
            unsafe {
                let _ = CloseHandle(h);
            }
            return Err(anyhow!("Baffle is already running"));
        }
        Ok(h)
    }

    /// Best-effort: bring the existing Baffle window to the front.
    pub fn focus_existing() {
        // SAFETY: read-only window lookup + foreground request.
        unsafe {
            unsafe extern "system" {
                fn FindWindowW(lpclassname: *const u16, lpwindowname: *const u16) -> isize;
                fn SetForegroundWindow(hwnd: isize) -> i32;
            }
            let title: Vec<u16> = "Baffle\0".encode_utf16().collect();
            let hwnd = FindWindowW(std::ptr::null(), title.as_ptr());
            if hwnd != 0 {
                SetForegroundWindow(hwnd);
            }
        }
    }
}

fn main() -> Result<()> {
    env_logger::builder()
        .format_timestamp(None)
        .filter_level(log::LevelFilter::Info)
        .parse_default_env()
        .init();

    // Single instance: if another Baffle already runs, focus its window and leave.
    #[cfg(windows)]
    let _single_instance = match single_instance::acquire() {
        Ok(handle) => handle,
        Err(_) => {
            single_instance::focus_existing();
            return Ok(());
        }
    };

    let cfg = Arc::new(RwLock::new(config::Config::load()));
    let tel: SharedTelemetry = Arc::new(RwLock::new(Telemetry::default()));

    // Engine runs on its own thread and polls `cfg` (single source of truth).
    #[cfg(not(feature = "no-engine"))]
    let _engine = audio::spawn(cfg.clone(), tel.clone())?;
    // Tray flips `cfg.enabled` (and persists it).
    #[cfg(not(feature = "no-engine"))]
    volume::run_tray(cfg.clone())?;
    // UI runs on the main thread; closing the window exits the app.
    ui::run(cfg.clone(), tel.clone());

    // Window closed: let the engine restore the user's endpoint volume, then
    // exit deterministically (this also stops the tray thread).
    shutdown_and_exit();
}

/// Request a coordinated shutdown, giving platform audio code time to restore
/// the user's original volume before the process is terminated.
pub fn shutdown_and_exit() -> ! {
    SHUTDOWN.store(true, std::sync::atomic::Ordering::Relaxed);
    for _ in 0..30 {
        if SHUTDOWN_DONE.load(std::sync::atomic::Ordering::Relaxed) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    std::process::exit(0);
}
