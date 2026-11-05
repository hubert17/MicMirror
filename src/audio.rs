#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
compile_error!("MicMirror is exclusively engineered for Apple Silicon macOS (aarch64-apple-darwin).");

use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, Device, SampleRate, Stream, StreamConfig};
use log::{error, info, warn};
use rtrb::{Consumer, Producer, RingBuffer};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

pub const RING_BUFFER_CAPACITY_FRAMES: usize = 1024;

// (Input frames, Output frames)
pub const BUFFER_PRESETS: [(u32, u32); 7] = [
    (16, 64),
    (24, 96),
    (32, 128), // Default SoX Baseline: sox -V0 --multi-threaded --input-buffer 32 --buffer 128 -d -d
    (48, 128),
    (64, 128),
    (128, 256),
    (256, 512),
];
pub const DEFAULT_PRESET_INDEX: usize = 2; // (32 in / 128 out)

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeadphoneState {
    Connected(String),
    DisconnectedSpeakersDetected(String),
    NotFound,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct DeviceInfo {
    pub input_name: String,
    pub output_name: String,
    pub sample_rate: u32,
    pub headphone_state: HeadphoneState,
}

pub struct AudioEngine {
    input_stream: Option<Stream>,
    output_stream: Option<Stream>,
    is_active: Arc<AtomicBool>,
    safety_lockout: Arc<AtomicBool>,
    input_volume_bits: Arc<AtomicU32>, // f32 stored as u32 bits (0.0 to 1.5)
    current_device_info: DeviceInfo,
    current_preset_index: usize,
}

impl AudioEngine {
    pub fn new(initial_preset_index: usize) -> Result<Self> {
        let is_active = Arc::new(AtomicBool::new(true));
        let safety_lockout = Arc::new(AtomicBool::new(false));
        let input_volume_bits = Arc::new(AtomicU32::new(0.75f32.to_bits())); // Default 75% volume

        let host = cpal::default_host();
        let (input_device, input_name) = find_builtin_microphone(&host)?;
        let (output_device, output_name, headphone_state) = find_headphone_output(&host)?;

        let is_safe = matches!(headphone_state, HeadphoneState::Connected(_));
        if !is_safe {
            warn!("Safety Lockout: 3.5mm headphones not detected. Auto-muting to prevent acoustic feedback.");
            safety_lockout.store(true, Ordering::SeqCst);
        }

        let device_info = DeviceInfo {
            input_name,
            output_name,
            sample_rate: 48000,
            headphone_state,
        };

        let mut engine = Self {
            input_stream: None,
            output_stream: None,
            is_active,
            safety_lockout,
            input_volume_bits,
            current_device_info: device_info,
            current_preset_index: initial_preset_index.min(BUFFER_PRESETS.len() - 1),
        };

        if is_safe {
            let (in_f, out_f) = BUFFER_PRESETS[engine.current_preset_index];
            engine.init_streams(&input_device, &output_device, in_f, out_f)?;
        }

        Ok(engine)
    }

    fn init_streams(
        &mut self,
        input_device: &Device,
        output_device: &Device,
        in_frames: u32,
        out_frames: u32,
    ) -> Result<()> {
        let (producer, consumer) = RingBuffer::<f32>::new(RING_BUFFER_CAPACITY_FRAMES);

        // Apple Silicon built-in audio natively runs at 48kHz
        let sample_rate = SampleRate(48000);

        let default_in_config = input_device
            .default_input_config()
            .context("Failed to get default input config")?;
        let in_channels = default_in_config.channels();

        let in_config = StreamConfig {
            channels: in_channels,
            sample_rate,
            buffer_size: BufferSize::Fixed(in_frames),
        };

        // Output stream (Stereo = 2 channels)
        let out_channels = 2;
        let out_config = StreamConfig {
            channels: out_channels,
            sample_rate,
            buffer_size: BufferSize::Fixed(out_frames),
        };

        info!(
            "Starting CoreAudio streams: In='{}' ({}ch, {}f), Out='{}' ({}ch, {}f) @ 48kHz",
            input_device.name().unwrap_or_default(),
            in_channels,
            in_frames,
            output_device.name().unwrap_or_default(),
            out_channels,
            out_frames,
        );

        let input_stream = setup_input_stream(
            input_device,
            &in_config,
            producer,
            self.input_volume_bits.clone(),
        )?;
        let output_stream = setup_output_stream(
            output_device,
            &out_config,
            consumer,
            self.is_active.clone(),
            self.safety_lockout.clone(),
        )?;

        input_stream.play().context("Failed to start input stream")?;
        output_stream.play().context("Failed to start output stream")?;

        self.input_stream = Some(input_stream);
        self.output_stream = Some(output_stream);

        Ok(())
    }

    pub fn set_buffer_preset(&mut self, preset_index: usize) -> Result<()> {
        let idx = preset_index.min(BUFFER_PRESETS.len() - 1);
        if self.current_preset_index == idx && self.input_stream.is_some() {
            return Ok(());
        }

        self.current_preset_index = idx;
        let (in_f, out_f) = BUFFER_PRESETS[idx];
        info!("Setting buffer preset {}: In {}f / Out {}f", idx, in_f, out_f);

        self.input_stream = None;
        self.output_stream = None;

        if !self.safety_lockout.load(Ordering::SeqCst) {
            let host = cpal::default_host();
            let (input_device, _) = find_builtin_microphone(&host)?;
            let (output_device, _, _) = find_headphone_output(&host)?;
            self.init_streams(&input_device, &output_device, in_f, out_f)?;
        }

        Ok(())
    }

    pub fn set_input_volume(&self, volume: f32) {
        let clamped = volume.clamp(0.0, 1.5);
        self.input_volume_bits.store(clamped.to_bits(), Ordering::Relaxed);
        info!("Input volume set to: {:.0}%", clamped * 100.0);
    }

    pub fn get_input_volume(&self) -> f32 {
        f32::from_bits(self.input_volume_bits.load(Ordering::Relaxed))
    }

    pub fn set_active(&self, active: bool) {
        self.is_active.store(active, Ordering::SeqCst);
        info!("Sidetone active set to: {}", active);
    }

    pub fn is_active(&self) -> bool {
        self.is_active.load(Ordering::Relaxed)
    }

    pub fn device_info(&self) -> &DeviceInfo {
        &self.current_device_info
    }

    pub fn refresh_devices(&mut self) -> Result<bool> {
        let host = cpal::default_host();
        let (input_device, input_name) = find_builtin_microphone(&host)?;
        let (output_device, output_name, headphone_state) = find_headphone_output(&host)?;

        let is_safe = matches!(headphone_state, HeadphoneState::Connected(_));
        let prev_safe = !self.safety_lockout.load(Ordering::SeqCst);
        self.safety_lockout.store(!is_safe, Ordering::SeqCst);

        self.current_device_info = DeviceInfo {
            input_name,
            output_name,
            sample_rate: 48000,
            headphone_state,
        };

        if is_safe {
            if self.input_stream.is_none() || self.output_stream.is_none() {
                let (in_f, out_f) = BUFFER_PRESETS[self.current_preset_index];
                self.init_streams(&input_device, &output_device, in_f, out_f)?;
            }
        } else {
            self.input_stream = None;
            self.output_stream = None;
        }

        Ok(prev_safe != is_safe)
    }
}

fn find_builtin_microphone(host: &cpal::Host) -> Result<(Device, String)> {
    let devices = host.input_devices().context("Failed to query input devices")?;
    for device in devices {
        if let Ok(name) = device.name() {
            let lower = name.to_lowercase();
            if lower.contains("macbook") || lower.contains("built-in") || lower.contains("internal") || lower.contains("microphone") {
                return Ok((device, name));
            }
        }
    }

    let default_device = host
        .default_input_device()
        .ok_or_else(|| anyhow!("No default input device found"))?;
    let name = default_device.name().unwrap_or_else(|_| "Default Input".to_string());
    Ok((default_device, name))
}

fn find_headphone_output(host: &cpal::Host) -> Result<(Device, String, HeadphoneState)> {
    let devices = host.output_devices().context("Failed to query output devices")?;
    let mut detected_speakers: Option<(Device, String)> = None;

    for device in devices {
        if let Ok(name) = device.name() {
            let lower = name.to_lowercase();
            if lower.contains("headphone") {
                return Ok((device, name.clone(), HeadphoneState::Connected(name)));
            }
            if lower.contains("speakers") || lower.contains("macbook") || lower.contains("built-in") {
                detected_speakers = Some((device, name));
            }
        }
    }

    if let Some((speaker_dev, speaker_name)) = detected_speakers {
        return Ok((
            speaker_dev,
            speaker_name.clone(),
            HeadphoneState::DisconnectedSpeakersDetected(speaker_name),
        ));
    }

    let default_out = host
        .default_output_device()
        .ok_or_else(|| anyhow!("No default output device found"))?;
    let name = default_out.name().unwrap_or_else(|_| "Unknown Output".to_string());
    Ok((default_out, name, HeadphoneState::NotFound))
}

fn setup_input_stream(
    device: &Device,
    config: &StreamConfig,
    mut producer: Producer<f32>,
    volume_bits: Arc<AtomicU32>,
) -> Result<Stream> {
    let channels = config.channels as usize;

    let stream = device.build_input_stream(
        config,
        move |data: &[f32], _: &cpal::InputCallbackInfo| {
            let volume = f32::from_bits(volume_bits.load(Ordering::Relaxed));

            for chunk in data.chunks_exact(channels) {
                let raw_mono: f32 = if channels == 1 {
                    chunk[0]
                } else if channels == 2 {
                    (chunk[0] + chunk[1]) * 0.5
                } else {
                    chunk.iter().sum::<f32>() / (channels as f32)
                };

                // Apply input volume / gain
                let scaled_sample = raw_mono * volume;
                let _ = producer.push(scaled_sample);
            }
        },
        |err| error!("CoreAudio input stream error: {}", err),
        None,
    )?;

    Ok(stream)
}

fn setup_output_stream(
    device: &Device,
    config: &StreamConfig,
    mut consumer: Consumer<f32>,
    is_active: Arc<AtomicBool>,
    safety_lockout: Arc<AtomicBool>,
) -> Result<Stream> {
    let channels = config.channels as usize;

    let stream = device.build_output_stream(
        config,
        move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
            let active = is_active.load(Ordering::Relaxed);
            let locked = safety_lockout.load(Ordering::Relaxed);

            // Silence immediately if turned off or safety lockout triggered
            if !active || locked {
                for sample in data.iter_mut() {
                    *sample = 0.0;
                }
                while consumer.pop().is_ok() {}
                return;
            }

            for frame in data.chunks_exact_mut(channels) {
                match consumer.pop() {
                    Ok(sample) => {
                        for ch in frame.iter_mut() {
                            *ch = sample;
                        }
                    }
                    Err(_) => {
                        for ch in frame.iter_mut() {
                            *ch = 0.0;
                        }
                    }
                }
            }
        },
        |err| error!("CoreAudio output stream error: {}", err),
        None,
    )?;

    Ok(stream)
}
