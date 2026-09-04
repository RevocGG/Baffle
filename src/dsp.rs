//! Tiny, allocation-free analysis engine.
//!
//! Pipeline per ~30 ms hop of mono loopback audio:
//!   audio -> 5-band one-pole filter bank (wideband, tone-safe)
//!         -> per-band RMS levels -> weighted loudness (dBFS-ish)
//!         -> slow anchor ("the level the user is watching at", silence-frozen)
//!         -> two-goal controller:
//!              lift:  quiet dialogue is pulled up towards the user's target
//!              clamp: sudden loud bangs are pulled down towards it
//!         -> multiplicative band gains (we never touch the audio stream,
//!            only the endpoint volume — zero added latency, zero risk)
//!
//! Per-sample cost: 5 one-pole filters + 5 envelope smoothers. That's it.

pub const BANDS: usize = 5;

/// Analysis hop in milliseconds.
pub const HOP_MS: f32 = 30.0;

/// Lowpass corner frequencies (Hz). Band i = content between CORNERS[i] and
/// CORNERS[i+1], computed as LP(CORNERS[i+1]) − LP(CORNERS[i]) (waveforms).
const CORNERS: [f32; BANDS + 1] = [20.0, 60.0, 250.0, 1000.0, 4000.0, 12000.0];
const LP_COUNT: usize = BANDS + 1;
/// Perceptual weights (dialogue band dominates loudness).
const BAND_WEIGHTS: [f32; BANDS] = [0.5, 0.8, 1.0, 0.9, 0.6];

/// Controller bounds.
const MAX_LIFT_DB: f32 = 18.0;
const MAX_CLAMP_DB: f32 = 21.0;
/// Fast loudness envelope: quick attack to catch bangs, moderate release.
const FAST_ATTACK_S: f32 = 0.08;
const FAST_RELEASE_S: f32 = 2.0;
/// Anchor averaging window: "the level the user has been watching at".
const ANCHOR_TAU_S: f32 = 20.0;
/// A band counts as a "bang" when the fast loudness exceeds the anchor AND the
/// target by these amounts (dB).
const BANG_OVER_ANCHOR_DB: f32 = 12.0;
const BANG_OVER_TARGET_DB: f32 = 6.0;
/// Max total gain movement per hop (~±2 dB / 30 ms ≈ 66 dB/s ceiling).
const MAX_STEP_DB_PER_HOP: f32 = 2.0;
/// Signal-presence gate: below this fast loudness we treat it as silence and
/// never lift (silence is not quiet dialogue).
const GATE_LOW_DB: f32 = -60.0;
const GATE_HIGH_DB: f32 = -45.0;
/// Band level floor in dB (for display and math).
const FLOOR_DB: f32 = -120.0;

#[inline]
fn db_to_lin(db: f32) -> f32 {
    (10.0f32).powf(db / 20.0)
}
#[inline]
fn lin_to_db(x: f32) -> f32 {
    20.0 * x.max(1e-12).log10()
}

pub struct Analyzer {
    ch: usize,
    hop: usize,
    n: usize,
    // Independent one-pole lowpass states at CORNERS[0..LP_COUNT]
    lp: [f32; LP_COUNT],
    lp_coef: [f32; LP_COUNT],
    // Smoothed mean-square per band (30 ms-ish release, fast attack)
    band_ms: [f32; BANDS],
    band_a: [f32; BANDS],
    // Fast loudness envelope (dB domain)
    fast: f32,
    // Anchor (power domain), seeded on first real signal
    anchor_ms: f32,
    anchor_seeded: bool,
    // Bang envelope 0..1
    bang: f32,
    // Output gains per band (linear)
    pub gains: [f32; BANDS],
    // Last band levels in dB (telemetry)
    band_db_cache: [f32; BANDS],
    // Current total action in dB (for telemetry + rate limiting)
    action_db: f32,
    // User settings
    target_db: f32,
    strength: f32,
}

impl Analyzer {
    pub fn new(sr: u32, ch: u16, target_db: f32, strength: f32) -> Self {
        let sr = sr as f32;
        let mut lp_coef = [0.0f32; LP_COUNT];
        for (i, c) in lp_coef.iter_mut().enumerate() {
            let fc = CORNERS[i].clamp(10.0, sr * 0.45);
            // one-pole lowpass: y += a*(x-y); a = 1 - exp(-2π fc / sr)
            *c = 1.0 - (-std::f32::consts::TAU * fc / sr).exp();
        }
        // Per-band envelope coefficients: fast attack (~5 ms), ~60 ms release.
        let a_attack = 1.0 - (-sr / 200.0).exp();
        let a_release = 1.0 - (-sr / 15.0).exp();
        let mut band_a = [0.0f32; BANDS];
        for a in band_a.iter_mut() {
            *a = a_attack.min(a_release);
        }
        Self {
            ch: (ch as usize).max(1),
            hop: ((sr * HOP_MS / 1000.0) as usize).max(64),
            n: 0,
            lp: [0.0; LP_COUNT],
            lp_coef,
            band_ms: [0.0; BANDS],
            band_a,
            fast: FLOOR_DB,
            anchor_ms: 1e-12,
            anchor_seeded: false,
            bang: 0.0,
            gains: [1.0; BANDS],
            band_db_cache: [FLOOR_DB; BANDS],
            action_db: 0.0,
            target_db,
            strength: strength.clamp(0.0, 1.5),
        }
    }

    pub fn set_target(&mut self, db: f32) {
        self.target_db = db;
    }
    pub fn set_strength(&mut self, s: f32) {
        self.strength = s.clamp(0.0, 1.5);
    }

    /// Feed interleaved f32 samples. Telemetry is refreshed after each call.
    pub fn process(&mut self, interleaved: &[f32], tel: &mut super::Telemetry) {
        let ch = self.ch;
        let frames = interleaved.len() / ch;
        for f in 0..frames {
            // Plain mean downmix — preserves waveform (no rectification).
            let mut m = 0.0f32;
            for s in &interleaved[f * ch..f * ch + ch] {
                m += s;
            }
            self.push_sample(m / ch as f32);
        }
        self.write_telemetry(tel);
    }

    #[inline]
    fn push_sample(&mut self, x: f32) {
        // Independent one-pole lowpasses of x at each corner frequency.
        for i in 0..LP_COUNT {
            self.lp[i] += self.lp_coef[i] * (x - self.lp[i]);
        }
        // Band i = LP(hi) − LP(lo) as WAVEFORMS (true bandpass by subtraction),
        // squared and smoothed. Bands are wide (one-pole skirts) — perfect for
        // loudness weighting; the overlap means no dead zones.
        for i in 0..BANDS {
            let bw = self.lp[i + 1] - self.lp[i];
            let p = bw * bw;
            self.band_ms[i] += self.band_a[i] * (p - self.band_ms[i]);
        }

        self.n += 1;
        if self.n >= self.hop {
            self.n = 0;
            self.hop_done();
        }
    }

    fn hop_done(&mut self) {
        let dt = HOP_MS / 1000.0;

        // Per-band levels (dB RMS).
        let mut band_db = [0.0f32; BANDS];
        for i in 0..BANDS {
            let db = lin_to_db(self.band_ms[i].sqrt()).max(FLOOR_DB);
            band_db[i] = db;
        }
        // Weighted loudness over ACTIVE bands only (within 35 dB of the
        // loudest) — dead bands must not drag the estimate towards −∞.
        let max_band = band_db.iter().copied().fold(f32::MIN, f32::max);
        let mut loud_sum = 0.0;
        let mut w_sum = 0.0;
        for i in 0..BANDS {
            if band_db[i] > max_band - 35.0 {
                loud_sum += BAND_WEIGHTS[i] * band_db[i];
                w_sum += BAND_WEIGHTS[i];
            }
        }
        let loud = if w_sum > 0.0 {
            (loud_sum / w_sum).max(FLOOR_DB)
        } else {
            FLOOR_DB
        };

        // Fast loudness envelope with attack/release.
        let tc = if loud > self.fast { FAST_ATTACK_S } else { FAST_RELEASE_S };
        let a = (-dt / tc).exp();
        self.fast = loud + a * (self.fast - loud);

        // Seed the anchor on the first real signal; freeze during silence.
        if !self.anchor_seeded && self.fast > GATE_LOW_DB {
            let lin = db_to_lin(self.fast);
            self.anchor_ms = lin * lin;
            self.anchor_seeded = true;
        }
        if self.anchor_seeded && self.fast > GATE_LOW_DB {
            let a2 = 1.0 - (-dt / ANCHOR_TAU_S).exp();
            let lin = db_to_lin(self.fast);
            self.anchor_ms += a2 * (lin * lin - self.anchor_ms);
        }
        let anchor_db = lin_to_db(self.anchor_ms.sqrt());

        // Bang detector: needs a seeded anchor AND actual loudness above target.
        let excess = self.fast - anchor_db;
        let bang_target = if self.anchor_seeded
            && excess > BANG_OVER_ANCHOR_DB
            && self.fast > self.target_db + BANG_OVER_TARGET_DB
        {
            ((excess - BANG_OVER_ANCHOR_DB) / 9.0).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let tc_b = if bang_target > self.bang { 0.05 } else { 6.0 };
        let ab = (-dt / tc_b).exp();
        self.bang = bang_target + ab * (self.bang - bang_target);

        // ---- Two-goal controller ----
        let target = self.target_db;
        let k = self.strength;

        // Presence gate 0..1: full lift only on real (quiet) content.
        let t_gate = ((self.fast - GATE_LOW_DB) / (GATE_HIGH_DB - GATE_LOW_DB)).clamp(0.0, 1.0);
        let presence = t_gate * t_gate * (3.0 - 2.0 * t_gate);

        // Goal A: lift quiet passages towards the target.
        let deficit = (target - self.fast).max(0.0);
        let mut lift_db = (deficit * 0.85).min(MAX_LIFT_DB) * k * presence;
        lift_db *= 1.0 - self.bang; // never fight an explosion

        // Goal B: clamp loud bangs back towards the target.
        let over = (self.fast - target).max(0.0);
        let mut clamp_db = -(over * 0.85).min(MAX_CLAMP_DB) * k;
        clamp_db *= self.bang; // only genuine bangs, not loud-but-intended scenes

        // Gentle long-term bias: whole movie much louder/quieter than target
        // (gated by presence — never during silence).
        let bias_db = ((target - anchor_db) * 0.2).clamp(-4.0, 4.0) * k * presence;

        let want_db = lift_db + clamp_db + bias_db;
        // Rate limit: keep movements smooth and inaudible as "movement".
        let max_step = MAX_STEP_DB_PER_HOP;
        self.action_db = want_db.clamp(self.action_db - max_step, self.action_db + max_step);

        // Distribution across bands:
        //  - lifting favours the speech band (mid/presence),
        //  - clamping favours lows + air (explosions are bass-heavy).
        let total = self.action_db;
        let mut gains = [1.0f32; BANDS];
        for i in 0..BANDS {
            let speech = i == 2 || i == 3;
            let shape = if total >= 0.0 {
                if speech { 1.2 } else { 0.75 }
            } else if speech {
                0.85
            } else {
                1.15
            };
            let g_db = (total * shape).clamp(-MAX_CLAMP_DB, MAX_LIFT_DB);
            gains[i] = db_to_lin(g_db);
        }
        self.gains = gains;
        self.band_db_cache = band_db;
    }

    fn write_telemetry(&self, tel: &mut super::Telemetry) {
        tel.band_gains = self.gains;
        tel.band_levels_db = self.band_db_cache;
        tel.loudness = self.fast;
        tel.anchor = lin_to_db(self.anchor_ms.sqrt());
        tel.applied = db_to_lin(self.action_db).clamp(0.0, 4.0);
        tel.action_db = self.action_db;
        tel.ducking = self.bang > 0.35;
        tel.lifting = self.action_db > 0.5;
    }
}
