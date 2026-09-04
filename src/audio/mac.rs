//! macOS: ScreenCaptureKit system-audio tap + CoreAudio default-device volume.
//!
//! SCK delivers f32 interleaved audio via CMSampleBuffers. Only audio is
//! requested (SCStreamOutputTypeAudio); no video is ever decoded, so the CPU
//! cost stays tiny. Volume is applied to the default output device's master
//! volume scalar via AudioObjectSetPropertyData.

use super::{Engine, VolumeActuator, APPLY_PERIOD};
use crate::SharedTelemetry;
use anyhow::{anyhow, Result};
use block2::RcBlock;
use objc2::define_class;
use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::{NSObject, ProtocolObject};
use objc2::{AnyThread, DefinedClass};
use objc2_core_audio::{
    kAudioDevicePropertyScopeOutput, kAudioDevicePropertyVolumeScalar,
    kAudioHardwarePropertyDefaultOutputDevice, kAudioObjectPropertyElementMaster,
    kAudioObjectPropertyScopeOutput, kAudioObjectSystemObject, AudioObjectGetPropertyData,
    AudioObjectID, AudioObjectPropertyAddress, AudioObjectSetPropertyData,
};
use objc2_core_foundation::CFRunLoop;
use objc2_core_media::{CMSampleBuffer, CMTimeMake};
use objc2_foundation::{NSError, NSObjectProtocol};
use objc2_screen_capture_kit::{
    SCContentFilter, SCShareableContent, SCStream, SCStreamConfiguration, SCStreamOutput,
    SCStreamOutputType,
};
use parking_lot::RwLock;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc};

static SAMPLE_RATE: AtomicU64 = AtomicU64::new(48_000);
static SAMPLE_CH: AtomicU64 = AtomicU64::new(2);

unsafe fn default_output_device() -> Result<AudioObjectID> {
    let mut size = std::mem::size_of::<AudioObjectID>() as u32;
    let mut dev: AudioObjectID = 0;
    let addr = AudioObjectPropertyAddress {
        mSelector: kAudioHardwarePropertyDefaultOutputDevice,
        mScope: kAudioObjectPropertyScopeOutput,
        mElement: kAudioObjectPropertyElementMaster,
    };
    let status = AudioObjectGetPropertyData(
        kAudioObjectSystemObject as AudioObjectID,
        NonNull::from(&addr),
        0,
        std::ptr::null(),
        NonNull::from(&mut size),
        NonNull::from(&mut dev).cast(),
    );
    if status != 0 {
        return Err(anyhow!("default_output_device: {status}"));
    }
    Ok(dev)
}

unsafe fn get_default_output_volume(dev: AudioObjectID) -> f32 {
    let addr = AudioObjectPropertyAddress {
        mSelector: kAudioDevicePropertyVolumeScalar,
        mScope: kAudioDevicePropertyScopeOutput,
        mElement: kAudioObjectPropertyElementMaster,
    };
    let mut vol = 0.5f32;
    let mut size = std::mem::size_of::<f32>() as u32;
    let status = AudioObjectGetPropertyData(
        dev,
        NonNull::from(&addr),
        0,
        std::ptr::null(),
        NonNull::from(&mut size),
        NonNull::from(&mut vol).cast(),
    );
    if status == 0 {
        vol.clamp(0.0, 1.0)
    } else {
        0.5
    }
}

unsafe fn set_default_output_volume(dev: AudioObjectID, vol: f32) {
    let addr = AudioObjectPropertyAddress {
        mSelector: kAudioDevicePropertyVolumeScalar,
        mScope: kAudioDevicePropertyScopeOutput,
        mElement: kAudioObjectPropertyElementMaster,
    };
    AudioObjectSetPropertyData(
        dev,
        NonNull::from(&addr),
        0,
        std::ptr::null(),
        std::mem::size_of::<f32>() as u32,
        NonNull::from(&vol).cast(),
    );
}

// ---- ObjC bridge: receives SCK audio sample buffers -----------------------

struct Ivars {
    tx: mpsc::SyncSender<Vec<f32>>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; we do not implement Drop.
    #[unsafe(super(NSObject))]
    #[name = "BaffleStreamOutput"]
    #[ivars = Ivars]
    struct StreamOutput;

    // SAFETY: we fully implement the (optional) protocol method we use.
    unsafe impl SCStreamOutput for StreamOutput {
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        unsafe fn stream_didOutputSampleBuffer_ofType(
            &self,
            _stream: &SCStream,
            sample_buffer: &CMSampleBuffer,
            of_type: SCStreamOutputType,
        ) {
            handle_sample(&self.ivars().tx, sample_buffer, of_type);
        }
    }

    unsafe impl NSObjectProtocol for StreamOutput {}
);

impl StreamOutput {
    fn new(tx: mpsc::SyncSender<Vec<f32>>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(Ivars { tx });
        unsafe { msg_send![super(this), init] }
    }
}

unsafe fn handle_sample(
    tx: &mpsc::SyncSender<Vec<f32>>,
    sb: &CMSampleBuffer,
    of_type: SCStreamOutputType,
) {
    if of_type != SCStreamOutputType::Audio {
        return;
    }
    if sb.num_samples() <= 0 || !sb.data_is_ready() {
        return;
    }
    let Some(block) = sb.data_buffer() else {
        return;
    };
    let len = block.data_length() as usize;
    if len == 0 {
        return;
    }
    let mut buf = vec![0u8; len];
    let Some(destination) = NonNull::new(buf.as_mut_ptr().cast()) else {
        return;
    };
    if block.copy_data_bytes(0, len, destination) != 0 {
        return;
    }
    let floats: Vec<f32> = buf
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect();
    let sr = SAMPLE_RATE.load(Ordering::Relaxed) as usize;
    let ch = SAMPLE_CH.load(Ordering::Relaxed).max(1) as usize;
    let chunk = (sr / 50 * ch).max(ch);
    for piece in floats.chunks(chunk) {
        if tx.send(piece.to_vec()).is_err() {
            return;
        }
    }
}

// ---- Engine entry ----------------------------------------------------------

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

    // DSP + volume thread.
    let cfg2 = cfg.clone();
    let tel2 = tel.clone();
    std::thread::Builder::new()
        .name("dsp".into())
        .spawn(move || dsp_and_apply_loop(cfg2, tel2, rx))?;

    // SCK setup on a thread with a runloop.
    std::thread::Builder::new()
        .name("sck".into())
        .spawn(move || unsafe {
            let r = sck_run(tx);
            if let Err(e) = r {
                log::error!("SCK: {e:#}");
            }
        })?;
    Ok(())
}

unsafe fn sck_run(tx: mpsc::SyncSender<Vec<f32>>) -> Result<()> {
    let (ready_tx, ready_rx) = mpsc::channel::<Result<()>>();
    let ready = RcBlock::new(
        move |_content: *mut SCShareableContent, err: *mut NSError| {
            let _ = ready_tx.send(if err.is_null() {
                Ok(())
            } else {
                Err(anyhow!("SCK denied"))
            });
        },
    );
    SCShareableContent::getShareableContentWithCompletionHandler(&ready);
    ready_rx
        .recv()
        .map_err(|_| anyhow!("SCK handler closed"))??;

    // Re-fetch synchronously-ish via another block to actually obtain content.
    let (ctx, crx) = mpsc::channel();
    let store = RcBlock::new(move |content: *mut SCShareableContent, err: *mut NSError| {
        if !err.is_null() || content.is_null() {
            let _ = ctx.send(Err(anyhow!("SCK content error")));
        } else {
            let retained = Retained::from_raw(content);
            let _ = ctx.send(Ok(retained));
        }
    });
    SCShareableContent::getShareableContentWithCompletionHandler(&store);
    let content: Retained<SCShareableContent> = crx
        .recv()
        .map_err(|_| anyhow!("SCK handler closed"))??
        .ok_or_else(|| anyhow!("SCK content was null"))?;

    let Some(display) = content.displays().firstObject() else {
        return Err(anyhow!("SCK: no displays"));
    };

    SAMPLE_RATE.store(48_000, Ordering::Relaxed);
    SAMPLE_CH.store(2, Ordering::Relaxed);

    let filter = SCContentFilter::initWithDisplay_excludingWindows(
        SCContentFilter::alloc(),
        &display,
        &objc2_foundation::NSArray::new(),
    );
    let sck_cfg = SCStreamConfiguration::new();
    sck_cfg.setCapturesAudio(true);
    sck_cfg.setExcludesCurrentProcessAudio(true);
    sck_cfg.setSampleRate(48_000);
    sck_cfg.setChannelCount(2);
    sck_cfg.setQueueDepth(8);
    // Audio-only interest: slow video pacing (audio is unaffected).
    sck_cfg.setMinimumFrameInterval(CMTimeMake(1, 10));

    let stream =
        SCStream::initWithFilter_configuration_delegate(SCStream::alloc(), &filter, &sck_cfg, None);
    let output = ProtocolObject::from_retained(StreamOutput::new(tx));
    let queue = dispatch2::DispatchQueue::new("baffle-audio", None);
    stream
        .addStreamOutput_type_sampleHandlerQueue_error(
            &output,
            SCStreamOutputType::Audio,
            Some(&queue),
        )
        .map_err(|e| anyhow!("addStreamOutput: {e}"))?;

    let started = RcBlock::new(|err: *mut NSError| {
        if !err.is_null() {
            log::error!("SCK startCapture failed");
        }
    });
    stream.startCaptureWithCompletionHandler(Some(&started));

    log::info!("ScreenCaptureKit audio tap started");
    CFRunLoop::run(); // park this thread servicing SCK callbacks
    Ok(())
}

fn dsp_and_apply_loop(
    cfg: Arc<RwLock<crate::config::Config>>,
    tel: SharedTelemetry,
    rx: mpsc::Receiver<Vec<f32>>,
) {
    let mut engine = Engine::new(cfg.clone(), tel.clone());
    let mut actuator = VolumeActuator::new();
    let mut last_apply = std::time::Instant::now();
    let mut user_base = unsafe { get_default_output_volume(default_output_device().unwrap_or(0)) };

    loop {
        if crate::SHUTDOWN.load(std::sync::atomic::Ordering::Relaxed) {
            break;
        }
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

            let dev = unsafe { default_output_device().unwrap_or(0) };
            let now_vol = unsafe { get_default_output_volume(dev) };
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
            unsafe { set_default_output_volume(dev, new_vol) };
        }
    }

    let dev = unsafe { default_output_device().unwrap_or(0) };
    unsafe { set_default_output_volume(dev, user_base) };
    crate::SHUTDOWN_DONE.store(true, std::sync::atomic::Ordering::Relaxed);
}
