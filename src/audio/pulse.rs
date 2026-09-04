//! Linux: PulseAudio monitor capture + sink volume control.
//!
//! Recording the default sink's `.monitor` source gives us the final mix
//! (exactly what the speakers output). Volume is applied with
//! `set_sink_volume_by_name` on the default sink — the same knob the user
//! turns, so their mental model of "50%" stays intact. On PipeWire systems
//! this all works through the PulseAudio compatibility layer.

use super::{Engine, VolumeActuator, APPLY_PERIOD};
use crate::SharedTelemetry;
use anyhow::{anyhow, Result};
use libpulse_binding as pulse;
use parking_lot::RwLock;
use std::sync::{mpsc, Arc};

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
    match real_main(&cfg, &tel) {
        Ok(()) => {}
        Err(e) => log::error!("audio engine stopped: {e:#}"),
    }
}

fn real_main(cfg: &Arc<RwLock<crate::config::Config>>, tel: &SharedTelemetry) -> Result<()> {
    let (tx, rx) = mpsc::sync_channel::<Vec<f32>>(16);

    let cfg2 = cfg.clone();
    let tel2 = tel.clone();
    std::thread::Builder::new()
        .name("dsp".into())
        .spawn(move || dsp_and_apply_loop(cfg2, tel2, rx))?;

    std::thread::Builder::new()
        .name("capture".into())
        .spawn(move || {
            if let Err(e) = capture_loop(tx) {
                log::error!("PA capture: {e:#}");
            }
        })?;

    Ok(())
}

fn wait_for<F: Fn() -> Option<bool>>(
    mainloop: &mut pulse::mainloop::standard::Mainloop,
    done: F,
) -> Result<()> {
    for _ in 0..5000 {
        mainloop.iterate(false);
        if let Some(true) = done() {
            return Ok(());
        }
        if let Some(false) = done() {
            return Err(anyhow!("failed"));
        }
    }
    Err(anyhow!("timeout"))
}

fn capture_loop(tx: mpsc::SyncSender<Vec<f32>>) -> Result<()> {
    use pulse::context::{Context, FlagSet as CtxFlags, State as CtxState};
    use pulse::mainloop::standard::Mainloop;
    use pulse::sample::{Format, Spec};
    use pulse::stream::Stream;
    use pulse::stream::{Direction, FlagSet as StreamFlags, PeekResult, State as StreamState};

    let mut mainloop = Mainloop::new().ok_or_else(|| anyhow!("PA mainloop"))?;
    let mut ctx = Context::new(&mut mainloop, "baffle").ok_or_else(|| anyhow!("PA context"))?;
    ctx.connect(None, CtxFlags::NOFAIL, None)
        .map_err(|e| anyhow!("PA connect: {e}"))?;
    wait_for(&mut mainloop, || match ctx.get_state() {
        CtxState::Ready => Some(true),
        CtxState::Failed | CtxState::Terminated => Some(false),
        _ => None,
    })?;

    let sink_name: String = {
        let introspector = ctx.introspect();
        let (stx, srx) = mpsc::channel::<String>();
        introspector.get_server_info(move |info| {
            let _ = stx.send(info.default_sink_name.clone().unwrap_or_default());
        });
        srx.recv_timeout(std::time::Duration::from_secs(5))?
    };

    let spec = Spec {
        format: Format::F32le,
        channels: 2,
        rate: 44_100,
    };
    assert!(spec.is_valid());

    let mut stream =
        Stream::new(&mut ctx, "baffle-monitor", &spec, None).ok_or_else(|| anyhow!("PA stream"))?;
    stream
        .connect_record(
            Some(&format!("{sink_name}.monitor")),
            None,
            StreamFlags::ADJUST_LATENCY,
        )
        .map_err(|e| anyhow!("connect_record: {e}"))?;
    wait_for(&mut mainloop, || match stream.get_state() {
        StreamState::Ready => Some(true),
        StreamState::Failed | StreamState::Terminated => Some(false),
        _ => None,
    })?;

    let ss = stream.get_sample_spec();
    log::info!(
        "PA monitor capture started: {} Hz, {} ch ({sink_name}.monitor)",
        ss.rate,
        ss.channels
    );

    loop {
        if crate::SHUTDOWN.load(std::sync::atomic::Ordering::Relaxed) {
            return Ok(());
        }
        mainloop.iterate(false);
        loop {
            match stream.peek() {
                Ok(PeekResult::Data(data)) => {
                    let floats: Vec<f32> = data
                        .chunks_exact(4)
                        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                        .collect();
                    let _ = stream.discard();
                    if tx.send(floats).is_err() {
                        return Ok(());
                    }
                }
                Ok(PeekResult::Empty) => break,
                Ok(PeekResult::Hole(_)) => {
                    let _ = stream.discard();
                }
                Err(e) => return Err(anyhow!("peek: {e}")),
            }
        }
    }
}

fn dsp_and_apply_loop(
    cfg: Arc<RwLock<crate::config::Config>>,
    tel: SharedTelemetry,
    rx: mpsc::Receiver<Vec<f32>>,
) {
    let mut engine = Engine::new(cfg.clone(), tel.clone());
    let mut actuator = VolumeActuator::new();
    let mut last_apply = std::time::Instant::now();
    let mut pa = PaCtl::connect();
    let mut user_base = pa.as_ref().map_or(0.6, |p| p.get_sink_volume());

    loop {
        let chunk = match rx.recv_timeout(std::time::Duration::from_millis(200)) {
            Ok(chunk) => chunk,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if crate::SHUTDOWN.load(std::sync::atomic::Ordering::Relaxed) {
                    break;
                }
                continue;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        engine.push_chunk(&chunk);
        if last_apply.elapsed() >= APPLY_PERIOD {
            last_apply = std::time::Instant::now();
            engine.sync_settings();

            if pa.is_none() {
                pa = PaCtl::connect();
            }
            if let Some(p) = pa.as_mut() {
                let now_vol = p.get_sink_volume();
                let implied_base = if actuator.current > 0.05 {
                    now_vol / actuator.current
                } else {
                    now_vol
                };
                if (implied_base - user_base).abs() > 0.01 {
                    user_base = implied_base.clamp(0.0, 1.0);
                }

                let action_db = if engine.enabled {
                    engine.tel.read().action_db
                } else {
                    0.0
                };
                let new_vol = actuator.update(action_db, user_base, APPLY_PERIOD.as_secs_f32());
                p.set_sink_volume(new_vol);
            } else {
                std::thread::sleep(std::time::Duration::from_secs(2));
            }
        }
    }

    // The DSP loop owns the last known base level, so it can undo Baffle's
    // correction even when the UI or tray initiated the shutdown.
    if let Some(p) = pa.as_ref() {
        p.set_sink_volume(user_base);
    }
    crate::SHUTDOWN_DONE.store(true, std::sync::atomic::Ordering::Relaxed);
}

// ---- Blocking PA volume control on its own mainloop ------------------------

struct PaCtl {
    mainloop: *mut pulse::mainloop::standard::Mainloop,
    context: *mut pulse::context::Context,
}

// SAFETY: PaCtl exclusively owns its mainloop+context and is only used from
// the dsp thread; the raw pointers are never aliased.
unsafe impl Send for PaCtl {}

const PA_NORM: f32 = 65536.0; // pa_volume_t for 100%

impl PaCtl {
    fn connect() -> Option<Self> {
        use pulse::context::{Context, FlagSet as CtxFlags};
        use pulse::mainloop::standard::Mainloop;
        let mut mainloop = Mainloop::new()?;
        let mut context = Context::new(&mut mainloop, "baffle-ctl")?;
        context.connect(None, CtxFlags::NOFAIL, None).ok()?;
        let ctl = Self {
            mainloop: Box::into_raw(Box::new(mainloop)),
            context: Box::into_raw(Box::new(context)),
        };
        if ctl.wait_ready().is_err() {
            return None;
        }
        Some(ctl)
    }

    fn wait_ready(&self) -> Result<()> {
        unsafe {
            for _ in 0..5000 {
                (*self.mainloop).iterate(false);
                match (*self.context).get_state() {
                    pulse::context::State::Ready => return Ok(()),
                    pulse::context::State::Failed | pulse::context::State::Terminated => {
                        return Err(anyhow!("PA ctx failed"));
                    }
                    _ => {}
                }
            }
        }
        Err(anyhow!("PA not ready"))
    }

    fn drain(&self) {
        unsafe {
            for _ in 0..4 {
                (*self.mainloop).iterate(false);
            }
        }
    }

    fn default_sink(&self) -> Option<String> {
        let (tx, rx) = mpsc::channel::<String>();
        unsafe {
            (*self.context).introspect().get_server_info(move |info| {
                let _ = tx.send(info.default_sink_name.clone().unwrap_or_default());
            });
        }
        let name = rx.recv_timeout(std::time::Duration::from_secs(2)).ok()?;
        self.drain();
        Some(name)
    }

    fn get_sink_volume(&self) -> f32 {
        let Some(sink) = self.default_sink() else {
            return 0.6;
        };
        let (tx, rx) = mpsc::channel::<f32>();
        unsafe {
            (*self.context)
                .introspect()
                .get_sink_info_by_name(&sink, move |info| {
                    let v = info.volume.avg().0 as f32 / PA_NORM;
                    let _ = tx.send(v.clamp(0.0, 1.0));
                });
        }
        let v = rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap_or(0.6);
        self.drain();
        v
    }

    fn set_sink_volume(&self, vol: f32) {
        let Some(sink) = self.default_sink() else {
            return;
        };
        use pulse::volume::{ChannelVolumes, Volume};
        let mut cv = ChannelVolumes::default();
        cv.set(2, Volume((vol.clamp(0.0, 1.0) * PA_NORM) as u32));
        unsafe {
            (*self.context)
                .introspect()
                .set_sink_volume_by_name(&sink, &cv, None);
        }
        self.drain();
    }
}
