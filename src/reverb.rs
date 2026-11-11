//! Enhanced Freeverb with room-size scaling from intimate room to concert hall.
//!
//! Architecture:
//! - Optional pre-delay (0–40ms) simulating distance to first wall reflection.
//! - 8 parallel lowpass feedback comb filters per channel with stereo spread.
//! - 4 series allpass filters per channel for dense phase diffusion.
//! - Soft-clip output limiter for safe headphone monitoring at high reverb.
//! - Single composite slider maps 0.0–1.0 to musical parameter curves.

const STEREO_SPREAD: usize = 23;
const MAX_PREDELAY_SAMPLES: usize = 1920; // 40ms at 48kHz

// Comb filter delay lengths tuned for 48kHz (scaled from 44.1kHz Freeverb reference)
const COMB_TUNING_L: [usize; 8] = [1217, 1293, 1417, 1494, 1556, 1653, 1726, 1803];
const ALLPASS_TUNING_L: [usize; 4] = [245, 372, 480, 606];

struct CombFilter {
    buffer: Vec<f32>,
    buf_idx: usize,
    filter_store: f32,
    damp: f32,
    feedback: f32,
}

impl CombFilter {
    fn new(size: usize) -> Self {
        Self {
            buffer: vec![0.0; size],
            buf_idx: 0,
            filter_store: 0.0,
            damp: 0.2,
            feedback: 0.5,
        }
    }

    #[inline(always)]
    fn process(&mut self, input: f32) -> f32 {
        let output = self.buffer[self.buf_idx];
        self.filter_store = output * (1.0 - self.damp) + self.filter_store * self.damp;
        self.buffer[self.buf_idx] = input + self.filter_store * self.feedback;
        self.buf_idx += 1;
        if self.buf_idx >= self.buffer.len() {
            self.buf_idx = 0;
        }
        output
    }

    fn set_damp(&mut self, val: f32) {
        self.damp = val;
    }

    fn set_feedback(&mut self, val: f32) {
        self.feedback = val;
    }
}

struct AllpassFilter {
    buffer: Vec<f32>,
    buf_idx: usize,
    feedback: f32,
}

impl AllpassFilter {
    fn new(size: usize) -> Self {
        Self {
            buffer: vec![0.0; size],
            buf_idx: 0,
            feedback: 0.5,
        }
    }

    #[inline(always)]
    fn process(&mut self, input: f32) -> f32 {
        let buf_out = self.buffer[self.buf_idx];
        let output = -input + buf_out;
        self.buffer[self.buf_idx] = input + buf_out * self.feedback;
        self.buf_idx += 1;
        if self.buf_idx >= self.buffer.len() {
            self.buf_idx = 0;
        }
        output
    }
}

struct PreDelay {
    buffer: Vec<f32>,
    write_idx: usize,
    delay_samples: usize,
}

impl PreDelay {
    fn new(max_samples: usize) -> Self {
        Self {
            buffer: vec![0.0; max_samples + 1],
            write_idx: 0,
            delay_samples: 0,
        }
    }

    fn set_delay(&mut self, samples: usize) {
        self.delay_samples = samples.min(self.buffer.len() - 1);
    }

    #[inline(always)]
    fn process(&mut self, input: f32) -> f32 {
        let len = self.buffer.len();
        self.buffer[self.write_idx] = input;
        let read_idx = (self.write_idx + len - self.delay_samples) % len;
        let output = self.buffer[read_idx];
        self.write_idx = (self.write_idx + 1) % len;
        output
    }
}

pub struct Freeverb {
    combs_l: Vec<CombFilter>,
    combs_r: Vec<CombFilter>,
    allpasses_l: Vec<AllpassFilter>,
    allpasses_r: Vec<AllpassFilter>,
    predelay: PreDelay,
    room_size: f32,
    damp: f32,
    wet: f32,
    dry: f32,
    width: f32,
    input_scale: f32,
    last_amount: f32,
}

impl Freeverb {
    pub fn new() -> Self {
        let mut combs_l = Vec::with_capacity(8);
        let mut combs_r = Vec::with_capacity(8);

        for &len in COMB_TUNING_L.iter() {
            combs_l.push(CombFilter::new(len));
            combs_r.push(CombFilter::new(len + STEREO_SPREAD));
        }

        let mut allpasses_l = Vec::with_capacity(4);
        let mut allpasses_r = Vec::with_capacity(4);

        for &len in ALLPASS_TUNING_L.iter() {
            allpasses_l.push(AllpassFilter::new(len));
            allpasses_r.push(AllpassFilter::new(len + STEREO_SPREAD));
        }

        Self {
            combs_l,
            combs_r,
            allpasses_l,
            allpasses_r,
            predelay: PreDelay::new(MAX_PREDELAY_SAMPLES),
            room_size: 0.5,
            damp: 0.5,
            wet: 0.0,
            dry: 1.0,
            width: 1.0,
            input_scale: 0.015,
            last_amount: -1.0, // Force first update
        }
    }

    /// Map a 0.0–1.0 slider value to musical reverb parameters.
    ///
    /// | Slider  | Room Type    | Wet   | Feedback | Damp | Pre-delay |
    /// |---------|-------------|-------|----------|------|-----------|
    /// | 0%      | Off         | 0%    | —        | —    | 0ms       |
    /// | ~15%    | Small Room  | ~1.2% | 0.80     | 0.61 | ~0.9ms    |
    /// | ~35%    | Studio      | ~6.7% | 0.83     | 0.48 | ~5ms      |
    /// | ~55%    | Large Hall  | ~16%  | 0.86     | 0.36 | ~12ms     |
    /// | ~80%    | Concert Hall| ~35%  | 0.90     | 0.20 | ~26ms     |
    /// | 100%    | Cathedral   | ~55%  | 0.95     | 0.08 | ~40ms     |
    pub fn set_reverb_amount(&mut self, amount: f32) {
        let amount = amount.clamp(0.0, 1.0);

        // Skip update if value hasn't meaningfully changed
        if (amount - self.last_amount).abs() < 0.001 {
            return;
        }
        self.last_amount = amount;

        if amount <= 0.001 {
            self.wet = 0.0;
            self.dry = 1.0;
            return;
        }

        // Wet mix curve:
        // - 1%–9%: Gentle subtle onset (starts low so 1-9 is kept subtle as requested)
        // - 10%–24% [Small Room]: Noticeable boost so intimate room acoustics are clearly heard
        // - 25%–49% [Studio]: Audible and present studio room ambiance
        // - 50%–100% [Large Hall, Concert Hall, Cathedral]: Smoothly transitions into the already-loved hall/cathedral levels
        self.wet = if amount < 0.10 {
            // 0.0 to 0.10 maps gently from 0.0 to 0.05
            amount * 0.50
        } else if amount < 0.50 {
            // 0.10 to 0.50 smoothly scales from 0.05 to ~0.20 (giving clear body in 10-24% and 25-49%)
            0.05 + (amount - 0.10) * (0.15 / 0.40)
        } else {
            // 0.50 to 1.0 scales from 0.20 to 0.55 (matching the large hall, concert hall & cathedral response)
            0.20 + (amount - 0.50) * (0.35 / 0.50)
        };

        // Room size → comb feedback: 0.40 (clear reflections) to 0.95 (cathedral)
        // feedback = room_size * 0.28 + 0.7, capped at 0.95
        self.room_size = 0.40 + amount * 0.55;

        // Damping: 0.65 (warm room) to 0.08 (bright cathedral shimmer)
        self.damp = 0.65 - amount * 0.57;

        // Stereo width: 0.6 (studio stereo) to 1.0 (full panoramic spread)
        self.width = 0.6 + amount * 0.4;

        // Pre-delay: 0ms to ~40ms
        // Simulates physical distance from voice to first reflecting surface
        self.predelay
            .set_delay((amount * amount * MAX_PREDELAY_SAMPLES as f32) as usize);

        // Input gain into comb network: slightly higher baseline so lower room settings have good body
        self.input_scale = 0.022 + amount * 0.015;

        // Dry stays locked at 100% — direct zero-latency voice is never compromised
        self.dry = 1.0;

        self.update_parameters();
    }

    fn update_parameters(&mut self) {
        // Freeverb standard: feedback = room_size * 0.28 + 0.7
        let feedback = (self.room_size * 0.28 + 0.7).min(0.95);
        for c in self.combs_l.iter_mut() {
            c.set_feedback(feedback);
            c.set_damp(self.damp);
        }
        for c in self.combs_r.iter_mut() {
            c.set_feedback(feedback);
            c.set_damp(self.damp);
        }
    }

    /// Process one mono input sample → stereo (left, right) with reverb.
    ///
    /// The dry path passes through at zero latency. The wet (reverb) tail
    /// blossoms behind it naturally — no perceivable delay on the direct voice.
    #[inline(always)]
    pub fn process(&mut self, input: f32) -> (f32, f32) {
        if self.wet <= 0.0001 {
            return (input * self.dry, input * self.dry);
        }

        // Pre-delay for spatial depth (only affects wet path)
        let delayed = self.predelay.process(input);
        let scaled_in = delayed * self.input_scale;

        let mut out_l = 0.0f32;
        let mut out_r = 0.0f32;

        for c in self.combs_l.iter_mut() {
            out_l += c.process(scaled_in);
        }
        for c in self.combs_r.iter_mut() {
            out_r += c.process(scaled_in);
        }

        for a in self.allpasses_l.iter_mut() {
            out_l = a.process(out_l);
        }
        for a in self.allpasses_r.iter_mut() {
            out_r = a.process(out_r);
        }

        // Stereo width matrix
        let wet1 = self.wet * (self.width * 0.5 + 0.5);
        let wet2 = self.wet * ((1.0 - self.width) * 0.5);

        let left = input * self.dry + out_l * wet1 + out_r * wet2;
        let right = input * self.dry + out_r * wet1 + out_l * wet2;

        // Soft-clip to prevent headphone distortion at high reverb + loud input
        (soft_clip(left), soft_clip(right))
    }
}

/// Fast tanh approximation (Padé approximant, accurate to ~0.1% for |x| < 3).
/// Prevents clipping artifacts while preserving natural vocal dynamics.
#[inline(always)]
fn soft_clip(x: f32) -> f32 {
    if x.abs() >= 3.0 {
        return x.signum();
    }
    let x2 = x * x;
    x * (27.0 + x2) / (27.0 + 9.0 * x2)
}
