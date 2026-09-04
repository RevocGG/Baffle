use anyhow::Result;
use parking_lot::RwLock;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::config::Config;
use crate::dsp::Analyzer;
use crate::{SharedTelemetry, Telemetry};

/// How often the apply-loop refreshes the endpoint volume.
pub const APPLY_HZ: f32 = 20.0;
pub const APPLY_PERIOD: Duration = Duration::from_millis(50);

/// Smoothed endpoint volume actuator with slew limiting (per platform impl).
pub struct VolumeActuator {
    pub current: f32,
    slew_per_s: f32,
}

impl VolumeActuator {
    pub fn new() -> Self {
        Self { current: f32::NAN, slew_per_s: 1.2 }
    }
    /// `delta_db` — controller action; `user_base` — user-chosen volume.
    pub fn update(&mut self, delta_db: f32, user_base: f32, dt: f32) -> f32 {
        // First call adopts the live volume (no startup glide).
        if self.current.is_nan() {
            self.current = user_base;
        }
        let target = (user_base * db_to_lin(delta_db)).clamp(0.0, 1.0);
        let max_step = self.slew_per_s * dt;
        let d = (target - self.current).clamp(-max_step, max_step);
        self.current = (self.current + d).clamp(0.0, 1.0);
        self.current
    }
}

/// Runs the analysis loop (platform capture feeds `push_chunk`) and the
/// volume-apply loop. Platform modules own the threads.
pub struct Engine {
    pub analyzer: Analyzer,
    pub tel: SharedTelemetry,
    pub cfg: Arc<RwLock<Config>>,
    pub enabled: bool,
    pub last_cfg_pull: Instant,
}

impl Engine {
    pub fn new(cfg: Arc<RwLock<Config>>, tel: SharedTelemetry) -> Self {
        let (target, strength, enabled) = {
            let c = cfg.read();
            (c.target_loudness, c.strength, c.enabled)
        };
        Self {
            // 48 kHz / 2ch placeholder; platform code re-creates with the real format.
            analyzer: Analyzer::new(48_000, 2, target, strength),
            tel,
            cfg,
            enabled,
            last_cfg_pull: Instant::now(),
        }
    }

    pub fn sync_settings(&mut self) {
        if self.last_cfg_pull.elapsed() > Duration::from_millis(200) {
            let c = self.cfg.read();
            self.analyzer.set_target(c.target_loudness);
            self.analyzer.set_strength(c.strength);
            self.enabled = c.enabled;
            self.last_cfg_pull = Instant::now();
        }
    }

    pub fn push_chunk(&mut self, interleaved: &[f32]) {
        // Bail out early once the app is shutting down.
        if crate::SHUTDOWN.load(std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        self.sync_settings();
        if !self.enabled {
            let mut t = self.tel.write();
            *t = Telemetry::default();
            return;
        }
        let mut t = self.tel.write();
        self.analyzer.process(interleaved, &mut t);
    }
}

/// Platform entry: spawns capture + apply threads, returns a join handle.
pub fn spawn(
    cfg: Arc<RwLock<Config>>,
    tel: SharedTelemetry,
) -> Result<std::thread::JoinHandle<()>> {
    #[cfg(target_os = "windows")]
    return win::spawn(cfg, tel);
    #[cfg(target_os = "macos")]
    return mac::spawn(cfg, tel);
    #[cfg(target_os = "linux")]
    return pulse::spawn(cfg, tel);
}

pub fn db_to_lin(db: f32) -> f32 {
    (10.0f32).powf(db / 20.0)
}

#[cfg(target_os = "windows")]
pub mod win;
#[cfg(target_os = "macos")]
pub mod mac;
#[cfg(target_os = "linux")]
pub mod pulse;
