//! Freeverb implementation tailored for ultra-low latency vocal monitoring at 48kHz.
//!
//! Architecture:
//! - 8 Lowpass Feedback Comb Filters (LBFC) in parallel per channel with slight stereo spread.
//! - 4 Allpass Filters in series per channel for phase diffusion.
//! - Allocation-free processing in the audio callback.

const STEREO_SPREAD: usize = 23;

// Comb filter delay lengths tuned for 48kHz (scaled from standard 44.1kHz Freeverb tunings: len * 48000 / 44100)
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

pub struct Freeverb {
    combs_l: Vec<CombFilter>,
    combs_r: Vec<CombFilter>,
    allpasses_l: Vec<AllpassFilter>,
    allpasses_r: Vec<AllpassFilter>,
    room_size: f32,
    damp: f32,
    wet: f32,
    dry: f32,
    width: f32,
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

        let mut verb = Self {
            combs_l,
            combs_r,
            allpasses_l,
            allpasses_r,
            room_size: 0.7,
            damp: 0.25,
            wet: 0.0, // Default off until slider turned up
            dry: 1.0,
            width: 1.0,
        };

        verb.update_parameters();
        verb
    }

    pub fn set_wet(&mut self, wet: f32) {
        self.wet = wet.clamp(0.0, 1.0);
        // Maintain constant perceived energy: dry stays solid for singing confidence
        self.dry = 1.0;
    }

    #[allow(dead_code)]
    pub fn set_room_size(&mut self, size: f32) {
        self.room_size = size.clamp(0.0, 1.0);
        self.update_parameters();
    }

    #[allow(dead_code)]
    pub fn set_damp(&mut self, damp: f32) {
        self.damp = damp.clamp(0.0, 1.0);
        self.update_parameters();
    }

    fn update_parameters(&mut self) {
        // Freeverb standard mapping: feedback = room_size * 0.28 + 0.7
        let feedback = self.room_size * 0.28 + 0.7;
        for c in self.combs_l.iter_mut() {
            c.set_feedback(feedback);
            c.set_damp(self.damp);
        }
        for c in self.combs_r.iter_mut() {
            c.set_feedback(feedback);
            c.set_damp(self.damp);
        }
    }

    /// Process one mono input sample and produce (out_left, out_right).
    #[inline(always)]
    pub fn process(&mut self, input: f32) -> (f32, f32) {
        if self.wet <= 0.0001 {
            return (input * self.dry, input * self.dry);
        }

        let scaled_in = input * 0.015; // Comb filter input headroom scaling
        let mut out_l = 0.0;
        let mut out_r = 0.0;

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

        // Stereo width matrixing
        let wet1 = self.wet * (self.width / 2.0 + 0.5);
        let wet2 = self.wet * ((1.0 - self.width) / 2.0);

        let left = input * self.dry + out_l * wet1 + out_r * wet2;
        let right = input * self.dry + out_r * wet1 + out_l * wet2;

        (left, right)
    }
}
