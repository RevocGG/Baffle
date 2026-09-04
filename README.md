# Baffle — smart loudness for movies

**Baffle watches the loudness of whatever is playing on your device and rides
the system volume for you**: quiet dialogue is lifted so you can hear it,
sudden explosions are clamped so you don't reach for the remote, and your
chosen volume level is always the anchor everything is balanced around.

One window. One toggle. One live equalizer showing exactly what the app is
doing to the sound, band by band.

- Native **Rust** binary — ~4 MB, no runtime, no installer, no background
  services beyond the app itself
- **Zero added latency and zero audio DSP on your content** — Baffle only
  *analyzes* the output mix and moves the *device volume*; your player's own
  volume and the audio path are never touched
- Works system-wide with **any** player: VLC, MPV, browsers, Netflix, Plex…
- Lives in the tray; close the window and it keeps working

---

## How it works

```
loopback tap of the output mix (system audio, 48 kHz)
        │
        ▼
5-band analysis (one-pole filter bank, 30 ms hops)  ──►  live spectrum UI
        │
        ▼
weighted loudness (dialogue band dominates)
        │
        ├─► slow anchor: "the level you've been watching at" (silence-frozen)
        │
        ▼
two-goal controller
   • LIFT   quiet dialogue towards your target     (speech band favoured)
   • CLAMP  sudden bangs back towards your target  (lows favoured, fast attack)
   • BIAS   gently rebalance if the whole film drifts away from the target
        │
        ▼
device volume (slew-limited, ~1.2 units/s, respects your slider as the base)
```

The controller is deliberately conservative:

- Movements are rate-limited (max ~2 dB per 30 ms) so changes read as
  "the mix opened up", not as pumping.
- Lifting **never** happens during silence — silence is not quiet dialogue.
- Clamping only fires on genuine transients above your target (explosions,
  shouty ads), never on loud-but-intended scenes.
- If **you** move the system volume, Baffle instantly adopts the new level as
  the base and keeps balancing around it.

## Per-platform notes

| OS | Audio tap | Volume control | Notes |
|----|-----------|----------------|-------|
| Windows | WASAPI loopback (shared, autoconvert to 48 kHz) | `IAudioEndpointVolume` on the default render device | No admin needed |
| macOS | ScreenCaptureKit (audio-only stream, excludes Baffle's own audio) | CoreAudio `kAudioDevicePropertyVolumeScalar` | macOS ≥ 13 required; grant Screen Recording permission once |
| Linux | PulseAudio monitor of the default sink (works on PipeWire via pipewire-pulse) | `set_sink_volume_by_name` on the default sink | Tray needs an SNI-capable shell (GNOME needs AppIndicator extension) |

## Building

Prereqs for all platforms: Rust (https://rustup.rs).

```bash
# Windows (MSYS/Git Bash + MinGW-w64 binutils on PATH)
scripts/build-windows.sh        # -> target/release/baffle.exe

# macOS (builds a universal binary)
scripts/build-macos.sh          # -> target/baffle-universal

# Linux (fully static musl binary)
scripts/build-linux.sh          # -> target/x86_64-unknown-linux-musl/release/baffle
```

Or simply `cargo build --release` on any of the three.

## Running

Just launch the binary. A small window appears with:

- **Active/Paused** toggle (also in the tray menu)
- A live spectrum view: restrained frequency bars show what's playing, the
  teal target line marks the level Baffle is steering toward, and a solid
  primary-text profile shows the live signal
- Target loudness + correction strength sliders

Closing the window exits the app; the tray icon keeps it alive if you prefer
to park it. Config lives in the standard per-user config directory
(`%APPDATA%\Baffle\config.json` on Windows).

### Permissions (first run)

- **macOS**: System Settings will ask for *Screen Recording* (ScreenCaptureKit
  needs it even though only audio is captured) — allow it once.
- **Windows / Linux**: no special permissions.

## Measured footprint (Windows, release build)

- Binary: **~4 MB**
- RAM: **~115 MB** with UI open (a GUI app showing a live plot; far less when
  closed to tray)
- CPU: **~10% of one core** while the window is open and animating at 15 fps;
  the audio engine itself costs well under 1% of one core

The heavy part is intentionally the pretty UI you asked for — the actual
audio analysis is a few dozen multiply-adds per sample on a 20 ms cadence.

## Why Rust

The three requirements were: tiny binary, minimal CPU/RAM, no runtime. Rust
compiles to a native executable with no VM, no garbage collector, and no
bundled browser (a Electron/WebView build of this same app would be ~150 MB
and idle at 3–5% CPU). The whole DSP loop allocates nothing after startup, so
there are no GC pauses or memory churn to cause audio glitches.

## Project layout

```
src/
  main.rs      entry, shared types
  config.rs    persisted settings
  dsp.rs       the loudness analyzer + lift/clamp controller (platform-neutral)
  audio.rs     engine supervisor, actuator
  audio/win.rs     Windows: WASAPI loopback + endpoint volume
  audio/mac.rs     macOS: ScreenCaptureKit tap + CoreAudio volume
  audio/pulse.rs   Linux: PulseAudio monitor + sink volume
  ui.rs        egui window: toggle, spectrum visualizer, sliders
  volume.rs    tray icon (tray-icon / ksni)
  icon.rs      runtime-generated icon (no image-decoding deps)
tools/gen_icon.py   regenerates assets/icon.png + .ico
```
