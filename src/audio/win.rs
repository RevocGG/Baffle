//! Windows: WASAPI loopback capture + IAudioEndpointVolume control.
//!
//! Capture runs event-driven on the engine thread, requested at 48 kHz f32
//! (WASAPI's AUTOCONVERT resamples the device mix — analyzing 48 kHz instead
//! of e.g. 192 kHz cuts CPU and memory traffic ~4x for zero quality loss at
//! our analysis rates). Volume is applied to the endpoint exactly like a
//! person turning the knob; media players keep their own volume untouched.

use super::{Engine, VolumeActuator, APPLY_PERIOD};
use crate::SharedTelemetry;
use anyhow::{anyhow, Result};
use parking_lot::RwLock;
use std::sync::Arc;
use std::time::{Duration, Instant};

use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{eConsole, eRender, IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED};

const ANALYSIS_SR: usize = 48_000;
const ANALYSIS_CH: usize = 2;

pub fn spawn(
    cfg: Arc<RwLock<crate::config::Config>>,
    tel: SharedTelemetry,
) -> Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("engine".into())
        .spawn(move || run(cfg, tel))
        .map_err(|e| anyhow!("spawn: {e}"))
}

fn run(cfg: Arc<RwLock<crate::config::Config>>, tel: SharedTelemetry) {
    if let Err(e) = real_main(&cfg, &tel) {
        log::error!("audio engine stopped: {e:#}");
    }
}

fn real_main(cfg: &Arc<RwLock<crate::config::Config>>, tel: &SharedTelemetry) -> Result<()> {
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED)
            .ok()
            .map_err(|e| anyhow!("CoInitializeEx: {e}"))?;
    }

    let mut engine = Engine::new(cfg.clone(), tel.clone());
    let mut actuator = VolumeActuator::new();
    let mut last_tel_log = Instant::now();

    unsafe {
        let enumr: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
            .map_err(|e| anyhow!("CoCreateInstance: {e}"))?;
        let dev: IMMDevice = enumr
            .GetDefaultAudioEndpoint(eRender, eConsole)
            .map_err(|e| anyhow!("GetDefaultAudioEndpoint: {e}"))?;
        let endpoint_volume: IAudioEndpointVolume =
            dev.Activate(CLSCTX_ALL, None).map_err(|e| anyhow!("Activate volume: {e}"))?;

        loop {
            let r = capture_session(
                &mut engine,
                &endpoint_volume,
                &mut actuator,
                cfg,
                &mut last_tel_log,
            );
            if crate::SHUTDOWN.load(std::sync::atomic::Ordering::Relaxed) {
                break Ok(());
            }
            if let Err(e) = r {
                log::warn!("capture session ended ({e:#}); retrying in 2 s");
                std::thread::sleep(Duration::from_secs(2));
            }
        }
    }
}

/// One capture "session": loopback of the default render device until error.
fn capture_session(
    engine: &mut Engine,
    endpoint_volume: &IAudioEndpointVolume,
    actuator: &mut VolumeActuator,
    cfg: &Arc<RwLock<crate::config::Config>>,
    last_tel_log: &mut Instant,
) -> Result<()> {
    use wasapi::*;

    initialize_mta().ok().map_err(|e| anyhow!("wasapi MTA: {e}"))?;
    let enumerator = DeviceEnumerator::new()?;
    let device = enumerator.get_default_device(&Direction::Render)?;
    let mut audio_client = device.get_iaudioclient()?;

    // Analyze at a fixed 48 kHz stereo f32 regardless of the device mix rate.
    let format = WaveFormat::new(32, 32, &SampleType::Float, ANALYSIS_SR, ANALYSIS_CH, None);

    let (def_time, _min) = audio_client.get_device_period()?;
    let mode = StreamMode::EventsShared {
        autoconvert: true,
        buffer_duration_hns: def_time.max(2_000_000), // >= 200 ms
    };
    audio_client.initialize_client(&format, &Direction::Capture, &mode)?;

    let h_event = audio_client.set_get_eventhandle()?;
    let capture_client = audio_client.get_audiocaptureclient()?;
    audio_client.start_stream()?;

    let sr = ANALYSIS_SR;
    let ch = ANALYSIS_CH;
    {
        let c = cfg.read();
        engine.analyzer = crate::dsp::Analyzer::new(sr as u32, ANALYSIS_CH as u16, c.target_loudness, c.strength);
    }
    log::info!("loopback capture started: {sr} Hz, {ch} ch (autoconverted)");

    let blockalign = format.get_blockalign() as usize;
    let mut queue: std::collections::VecDeque<u8> = std::collections::VecDeque::new();
    let chunk_frames = (sr as usize / 50).max(256); // ~20 ms DSP chunks
    let chunk_bytes = chunk_frames * blockalign;
    let mut floats: Vec<f32> = vec![0.0; chunk_frames * ch];

    let mut last_apply = Instant::now();
    let mut user_base: f32 = get_endpoint_volume(endpoint_volume);
    // Value we last wrote to the endpoint. When the live volume equals this,
    // nobody else touched it; when it differs, the user (or the system) did.
    let mut last_written: f32 = f32::NAN; // NAN = "unknown, adopt current as base"

    loop {
        // ---------- shutdown: restore volume and exit ----------
        if crate::SHUTDOWN.load(std::sync::atomic::Ordering::Relaxed) {
            // Restore the user's chosen volume (undo any applied gain).
            if !user_base.is_nan() {
                let _ = set_endpoint_volume(endpoint_volume, user_base);
            }
            crate::SHUTDOWN_DONE.store(true, std::sync::atomic::Ordering::Relaxed);

            // Stop the capture stream cleanly and leave.
            let _ = audio_client.stop_stream();
            return Ok(());
        }

        // ---------- capture (event-driven, 200 ms timeout keeps apply alive) ----------
        let _ = h_event.wait_for_event(200);
        capture_client.read_from_device_to_deque(&mut queue)?;
        while queue.len() >= chunk_bytes {
            pack_floats(&mut queue, chunk_bytes, &mut floats);
            engine.push_chunk(&floats);
        }

        // ---------- apply (20 Hz) ----------
        if last_apply.elapsed() >= APPLY_PERIOD {
            last_apply = Instant::now();
            engine.sync_settings();

            // --- Base tracking ---
            // If the live volume differs from what we last wrote, someone else
            // (the user) moved it: that value IS the new base. If it matches,
            // our own previous write, the base is unchanged.
            let now_vol = get_endpoint_volume(endpoint_volume);
            if !last_written.is_nan() && (now_vol - last_written).abs() > 0.01 {
                log::debug!("external volume change detected: {now_vol:.3} (was {last_written:.3})");
                user_base = now_vol;
                actuator.current = now_vol;
            }
            last_written = now_vol; // after adoption, we "own" this value

            let action_db = if engine.enabled { engine.tel.read().action_db } else { 0.0 };

            // 1 Hz telemetry trace (debug builds / RUST_LOG=debug).
            if last_tel_log.elapsed() >= Duration::from_secs(1) {
                *last_tel_log = Instant::now();
                let t = engine.tel.read();
                log::debug!(
                    "tel: loud={:+.1}dB anchor={:+.1}dB action={action_db:+.1}dB bands={:?} bang={}",
                    t.loudness,
                    t.anchor,
                    t.band_levels_db,
                    t.ducking
                );
            }

            let new_vol = actuator.update(action_db, user_base, APPLY_PERIOD.as_secs_f32());
            if (new_vol - last_written).abs() > 0.002 {
                log::debug!(
                    "apply: base={user_base:.3} action={action_db:+.1}dB -> vol={new_vol:.3}"
                );
                let _ = set_endpoint_volume(endpoint_volume, new_vol);
                last_written = new_vol;
            }
        }
    }
}

/// Pop `n` bytes off the queue and pack them into little-endian f32s.
fn pack_floats(queue: &mut std::collections::VecDeque<u8>, n: usize, out: &mut [f32]) {
    let mut bytes = [0u8; 4];
    for slot in out.iter_mut().take(n / 4) {
        for b in bytes.iter_mut() {
            *b = queue.pop_front().unwrap_or(0);
        }
        *slot = f32::from_le_bytes(bytes);
    }
    // Drain any leftovers (shouldn't happen when n is a multiple of 4).
    for _ in 0..(n % 4) {
        queue.pop_front();
    }
}

fn get_endpoint_volume(v: &IAudioEndpointVolume) -> f32 {
    unsafe { v.GetMasterVolumeLevelScalar().unwrap_or(0.5).clamp(0.0, 1.0) }
}

fn set_endpoint_volume(v: &IAudioEndpointVolume, vol: f32) -> Result<()> {
    unsafe {
        v.SetMasterVolumeLevelScalar(vol.clamp(0.0, 1.0), std::ptr::null())
            .map_err(|e| anyhow!("SetMasterVolumeLevelScalar: {e}"))
    }
}
