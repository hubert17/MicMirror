use crate::reverb::Freeverb;
use anyhow::{anyhow, Context, Result};
use coreaudio_sys as sys;
use cpal::traits::{DeviceTrait, HostTrait};
use cpal::Device;
use log::{error, info, warn};
use rtrb::{Consumer, Producer, RingBuffer};
use std::ffi::CStr;
use std::mem;
use std::os::raw::{c_char, c_void};
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};

pub const RING_BUFFER_CAPACITY_FRAMES: usize = 2048;
const MAX_FRAMES_PER_SLICE: u32 = 4096;

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

enum AudioCommand {
    SetBufferPreset(usize),
    RefreshDevices,
    Shutdown,
}

pub type HeadphoneChangeCallback = Box<dyn Fn(&HeadphoneState) + Send + 'static>;

pub struct AudioEngine {
    tx: Option<Sender<AudioCommand>>,
    control_thread: Option<JoinHandle<()>>,
    is_active: Arc<AtomicBool>,
    safety_lockout: Arc<AtomicBool>,
    input_volume_bits: Arc<AtomicU32>, // f32 stored as u32 bits (0.0 to 1.5)
    reverb_wet_bits: Arc<AtomicU32>,   // f32 stored as u32 bits (0.0 to 1.0)
    refresh_pending: Arc<AtomicBool>,
    initial_device_info: DeviceInfo,
}

impl AudioEngine {
    pub fn new(
        initial_preset_index: usize,
        on_headphone_change: HeadphoneChangeCallback,
    ) -> Result<Self> {
        let is_active = Arc::new(AtomicBool::new(true));
        let safety_lockout = Arc::new(AtomicBool::new(false));
        let input_volume_bits = Arc::new(AtomicU32::new(0.75f32.to_bits())); // Default 75% volume
        let reverb_wet_bits = Arc::new(AtomicU32::new(0.0f32.to_bits()));    // Default 0% (off)
        let refresh_pending = Arc::new(AtomicBool::new(false));

        let host = cpal::default_host();
        let (input_device, input_name) = find_builtin_microphone(&host)?;
        let (_output_device, output_name, headphone_state) = find_headphone_output(&host)?;

        let is_safe = matches!(headphone_state, HeadphoneState::Connected(_));
        if !is_safe {
            warn!("Safety Lockout: 3.5mm headphones not detected. Auto-muting to prevent acoustic feedback.");
            safety_lockout.store(true, Ordering::SeqCst);
        }

        let initial_device_info = DeviceInfo {
            input_name: input_name.clone(),
            output_name: output_name.clone(),
            sample_rate: 48000,
            headphone_state: headphone_state.clone(),
        };

        let preset_idx = initial_preset_index.min(BUFFER_PRESETS.len() - 1);

        let mut worker = AudioWorker {
            input_unit: None,
            output_unit: None,
            is_active: is_active.clone(),
            safety_lockout: safety_lockout.clone(),
            input_volume_bits: input_volume_bits.clone(),
            reverb_wet_bits: reverb_wet_bits.clone(),
            current_preset_index: preset_idx,
            current_device_info: initial_device_info.clone(),
            on_headphone_change,
        };

        if is_safe {
            let (in_f, out_f) = BUFFER_PRESETS[preset_idx];
            worker.init_streams(&input_device, &input_name, &output_name, in_f, out_f)?;
        }

        let (tx, rx) = mpsc::channel::<AudioCommand>();
        let pending_clone = refresh_pending.clone();
        let control_thread = thread::Builder::new()
            .name("MicMirror-AudioControl".to_string())
            .spawn(move || {
                while let Ok(cmd) = rx.recv() {
                    match cmd {
                        AudioCommand::SetBufferPreset(idx) => {
                            if let Err(e) = worker.set_buffer_preset(idx) {
                                error!("Failed to update CoreAudio buffer preset: {}", e);
                            }
                        }
                        AudioCommand::RefreshDevices => {
                            pending_clone.store(false, Ordering::SeqCst);
                            if let Err(e) = worker.refresh_devices() {
                                error!("Failed to refresh audio devices: {}", e);
                            }
                        }
                        AudioCommand::Shutdown => {
                            worker.stop_streams();
                            break;
                        }
                    }
                }
            })
            .context("Failed to spawn AudioControl thread")?;

        Ok(Self {
            tx: Some(tx),
            control_thread: Some(control_thread),
            is_active,
            safety_lockout,
            input_volume_bits,
            reverb_wet_bits,
            refresh_pending,
            initial_device_info,
        })
    }

    pub fn set_buffer_preset(&self, preset_index: usize) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(AudioCommand::SetBufferPreset(preset_index));
        }
    }

    pub fn set_input_volume(&self, volume: f32) {
        let clamped = volume.clamp(0.0, 1.5);
        self.input_volume_bits.store(clamped.to_bits(), Ordering::Relaxed);
        log::debug!("Input volume set to: {:.0}%", clamped * 100.0);
    }

    pub fn get_input_volume(&self) -> f32 {
        f32::from_bits(self.input_volume_bits.load(Ordering::Relaxed))
    }

    pub fn set_reverb_wet(&self, wet: f32) {
        let clamped = wet.clamp(0.0, 1.0);
        self.reverb_wet_bits.store(clamped.to_bits(), Ordering::Relaxed);
        log::debug!("Reverb wet set to: {:.0}%", clamped * 100.0);
    }

    pub fn get_reverb_wet(&self) -> f32 {
        f32::from_bits(self.reverb_wet_bits.load(Ordering::Relaxed))
    }

    pub fn set_active(&self, active: bool) {
        self.is_active.store(active, Ordering::SeqCst);
        info!("Sidetone active set to: {}", active);
    }

    pub fn is_active(&self) -> bool {
        self.is_active.load(Ordering::Relaxed)
    }

    pub fn device_info(&self) -> &DeviceInfo {
        &self.initial_device_info
    }

    pub fn request_refresh_devices(&self) {
        if self
            .refresh_pending
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            if let Some(tx) = &self.tx {
                let _ = tx.send(AudioCommand::RefreshDevices);
            }
        }
    }

    pub fn shutdown(&mut self) {
        info!("Shutting down AudioEngine streams...");
        self.is_active.store(false, Ordering::SeqCst);
        self.safety_lockout.store(true, Ordering::SeqCst);
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(AudioCommand::Shutdown);
        }
        if let Some(handle) = self.control_thread.take() {
            let _ = handle.join();
        }
        info!("AudioEngine streams stopped.");
    }
}

struct AudioWorker {
    input_unit: Option<HalInputUnit>,
    output_unit: Option<HalOutputUnit>,
    is_active: Arc<AtomicBool>,
    safety_lockout: Arc<AtomicBool>,
    input_volume_bits: Arc<AtomicU32>,
    reverb_wet_bits: Arc<AtomicU32>,
    current_preset_index: usize,
    current_device_info: DeviceInfo,
    on_headphone_change: HeadphoneChangeCallback,
}

impl AudioWorker {
    fn stop_streams(&mut self) {
        self.input_unit = None;
        self.output_unit = None;
    }

    fn init_streams(
        &mut self,
        input_device: &Device,
        input_name: &str,
        output_name: &str,
        in_frames: u32,
        out_frames: u32,
    ) -> Result<()> {
        let (producer, consumer) = RingBuffer::<f32>::new(RING_BUFFER_CAPACITY_FRAMES);

        let default_in_config = input_device
            .default_input_config()
            .context("Failed to get default input config")?;
        let in_channels = default_in_config.channels();

        let device_rate = default_in_config.sample_rate().0;
        if device_rate != 48000 {
            warn!(
                "Expected 48kHz sample rate but device reports {}Hz — audio may not work correctly",
                device_rate
            );
        }

        let input_id = find_audio_device_id_by_name(input_name, true)
            .ok_or_else(|| anyhow!("CoreAudio device ID not found for input '{}'", input_name))?;
        let output_id = find_audio_device_id_by_name(output_name, false)
            .ok_or_else(|| anyhow!("CoreAudio device ID not found for output '{}'", output_name))?;

        info!(
            "Starting CoreAudio HAL units: In='{}' ({}ch, {}f), Out='{}' (2ch, {}f) @ 48kHz",
            input_name, in_channels, in_frames, output_name, out_frames,
        );

        let input_unit = HalInputUnit::start(
            input_id,
            in_channels as u32,
            48000.0,
            in_frames,
            producer,
            self.input_volume_bits.clone(),
            self.reverb_wet_bits.clone(),
        )?;

        let output_unit = HalOutputUnit::start(
            output_id,
            2,
            48000.0,
            out_frames,
            consumer,
            self.is_active.clone(),
            self.safety_lockout.clone(),
        )?;

        self.input_unit = Some(input_unit);
        self.output_unit = Some(output_unit);
        Ok(())
    }

    fn set_buffer_preset(&mut self, preset_index: usize) -> Result<()> {
        let idx = preset_index.min(BUFFER_PRESETS.len() - 1);
        if self.current_preset_index == idx && self.input_unit.is_some() {
            return Ok(());
        }

        self.current_preset_index = idx;
        let (in_f, out_f) = BUFFER_PRESETS[idx];
        info!("Setting buffer preset {}: In {}f / Out {}f", idx, in_f, out_f);

        self.stop_streams();

        if !self.safety_lockout.load(Ordering::SeqCst) {
            let host = cpal::default_host();
            let (input_device, input_name) = find_builtin_microphone(&host)?;
            let (_, output_name, _) = find_headphone_output(&host)?;
            self.init_streams(&input_device, &input_name, &output_name, in_f, out_f)?;
        }

        Ok(())
    }

    fn refresh_devices(&mut self) -> Result<()> {
        let host = cpal::default_host();
        let (input_device, input_name) = find_builtin_microphone(&host)?;
        let (_, output_name, headphone_state) = find_headphone_output(&host)?;

        let is_safe = matches!(headphone_state, HeadphoneState::Connected(_));
        let prev_safe = !self.safety_lockout.load(Ordering::SeqCst);
        self.safety_lockout.store(!is_safe, Ordering::SeqCst);

        self.current_device_info = DeviceInfo {
            input_name: input_name.clone(),
            output_name: output_name.clone(),
            sample_rate: 48000,
            headphone_state: headphone_state.clone(),
        };

        if is_safe {
            if self.input_unit.is_none() || self.output_unit.is_none() {
                let (in_f, out_f) = BUFFER_PRESETS[self.current_preset_index];
                self.init_streams(&input_device, &input_name, &output_name, in_f, out_f)?;
            }
        } else {
            self.stop_streams();
        }

        if prev_safe != is_safe {
            (self.on_headphone_change)(&headphone_state);
        }

        Ok(())
    }
}

// ============================================================================
// Lock-free, deadlock-free CoreAudio AUHAL Units (no AudioUnitGetProperty in IOProc)
// ============================================================================

struct InputCallbackState {
    unit: sys::AudioUnit,
    channels: usize,
    scratch: Vec<f32>,
    producer: Producer<f32>,
    volume_bits: Arc<AtomicU32>,
    reverb_wet_bits: Arc<AtomicU32>,
    reverb: Freeverb,
}

struct HalInputUnit {
    unit: sys::AudioUnit,
    state_ptr: *mut InputCallbackState,
}

unsafe impl Send for HalInputUnit {}

impl HalInputUnit {
    fn start(
        device_id: sys::AudioDeviceID,
        channels: u32,
        sample_rate: f64,
        buffer_frames: u32,
        producer: Producer<f32>,
        volume_bits: Arc<AtomicU32>,
        reverb_wet_bits: Arc<AtomicU32>,
    ) -> Result<Self> {
        let unit = create_hal_audio_unit()?;

        unsafe {
            let enable_io: u32 = 1;
            let disable_io: u32 = 0;
            check_os_status(
                sys::AudioUnitSetProperty(
                    unit,
                    sys::kAudioOutputUnitProperty_EnableIO,
                    sys::kAudioUnitScope_Input,
                    1,
                    &enable_io as *const u32 as *const c_void,
                    mem::size_of::<u32>() as u32,
                ),
                "EnableIO(Input)",
            )?;
            check_os_status(
                sys::AudioUnitSetProperty(
                    unit,
                    sys::kAudioOutputUnitProperty_EnableIO,
                    sys::kAudioUnitScope_Output,
                    0,
                    &disable_io as *const u32 as *const c_void,
                    mem::size_of::<u32>() as u32,
                ),
                "EnableIO(Output=0)",
            )?;
            check_os_status(
                sys::AudioUnitSetProperty(
                    unit,
                    sys::kAudioOutputUnitProperty_CurrentDevice,
                    sys::kAudioUnitScope_Global,
                    0,
                    &device_id as *const sys::AudioDeviceID as *const c_void,
                    mem::size_of::<sys::AudioDeviceID>() as u32,
                ),
                "CurrentDevice(Input)",
            )?;

            set_device_buffer_frame_size(device_id, sys::kAudioObjectPropertyScopeInput, buffer_frames);

            let asbd = make_f32_interleaved_asbd(sample_rate, channels);
            check_os_status(
                sys::AudioUnitSetProperty(
                    unit,
                    sys::kAudioUnitProperty_StreamFormat,
                    sys::kAudioUnitScope_Output,
                    1,
                    &asbd as *const sys::AudioStreamBasicDescription as *const c_void,
                    mem::size_of::<sys::AudioStreamBasicDescription>() as u32,
                ),
                "StreamFormat(Input)",
            )?;

            let max_frames = MAX_FRAMES_PER_SLICE;
            check_os_status(
                sys::AudioUnitSetProperty(
                    unit,
                    sys::kAudioUnitProperty_MaximumFramesPerSlice,
                    sys::kAudioUnitScope_Global,
                    0,
                    &max_frames as *const u32 as *const c_void,
                    mem::size_of::<u32>() as u32,
                ),
                "MaximumFramesPerSlice(Input)",
            )?;

            let state = Box::new(InputCallbackState {
                unit,
                channels: channels as usize,
                scratch: vec![0.0f32; (MAX_FRAMES_PER_SLICE as usize) * (channels as usize)],
                producer,
                volume_bits,
                reverb_wet_bits,
                reverb: Freeverb::new(),
            });
            let state_ptr = Box::into_raw(state);

            let cb = sys::AURenderCallbackStruct {
                inputProc: Some(input_render_callback),
                inputProcRefCon: state_ptr as *mut c_void,
            };
            if let Err(e) = check_os_status(
                sys::AudioUnitSetProperty(
                    unit,
                    sys::kAudioOutputUnitProperty_SetInputCallback,
                    sys::kAudioUnitScope_Global,
                    0,
                    &cb as *const sys::AURenderCallbackStruct as *const c_void,
                    mem::size_of::<sys::AURenderCallbackStruct>() as u32,
                ),
                "SetInputCallback",
            ) {
                drop(Box::from_raw(state_ptr));
                sys::AudioComponentInstanceDispose(unit);
                return Err(e);
            }

            if let Err(e) = check_os_status(sys::AudioUnitInitialize(unit), "AudioUnitInitialize(Input)") {
                drop(Box::from_raw(state_ptr));
                sys::AudioComponentInstanceDispose(unit);
                return Err(e);
            }

            if let Err(e) = check_os_status(sys::AudioOutputUnitStart(unit), "AudioOutputUnitStart(Input)") {
                sys::AudioUnitUninitialize(unit);
                drop(Box::from_raw(state_ptr));
                sys::AudioComponentInstanceDispose(unit);
                return Err(e);
            }

            Ok(Self { unit, state_ptr })
        }
    }
}

impl Drop for HalInputUnit {
    fn drop(&mut self) {
        unsafe {
            sys::AudioOutputUnitStop(self.unit);
            sys::AudioUnitUninitialize(self.unit);
            sys::AudioComponentInstanceDispose(self.unit);
            drop(Box::from_raw(self.state_ptr));
        }
    }
}

unsafe extern "C" fn input_render_callback(
    in_ref_con: *mut c_void,
    io_action_flags: *mut sys::AudioUnitRenderActionFlags,
    in_time_stamp: *const sys::AudioTimeStamp,
    in_bus_number: sys::UInt32,
    in_number_frames: sys::UInt32,
    _io_data: *mut sys::AudioBufferList,
) -> sys::OSStatus {
    let state = &mut *(in_ref_con as *mut InputCallbackState);
    if in_number_frames == 0 || in_number_frames > MAX_FRAMES_PER_SLICE {
        return 0;
    }

    let sample_count = (in_number_frames as usize) * state.channels;
    let byte_size = (sample_count * mem::size_of::<f32>()) as u32;

    let mut buffer_list = sys::AudioBufferList {
        mNumberBuffers: 1,
        mBuffers: [sys::AudioBuffer {
            mNumberChannels: state.channels as u32,
            mDataByteSize: byte_size,
            mData: state.scratch.as_mut_ptr() as *mut c_void,
        }],
    };

    let status = sys::AudioUnitRender(
        state.unit,
        io_action_flags,
        in_time_stamp,
        in_bus_number,
        in_number_frames,
        &mut buffer_list,
    );
    if status != 0 {
        return status;
    }

    let volume = f32::from_bits(state.volume_bits.load(Ordering::Relaxed));
    let wet = f32::from_bits(state.reverb_wet_bits.load(Ordering::Relaxed));
    state.reverb.set_reverb_amount(wet);

    let data = &state.scratch[..sample_count];
    let channels = state.channels;

    for chunk in data.chunks_exact(channels) {
        let raw_mono: f32 = if channels == 1 {
            chunk[0]
        } else if channels == 2 {
            (chunk[0] + chunk[1]) * 0.5
        } else {
            chunk.iter().sum::<f32>() / (channels as f32)
        };

        let scaled_sample = raw_mono * volume;
        let (out_l, out_r) = state.reverb.process(scaled_sample);

        let _ = state.producer.push(out_l);
        let _ = state.producer.push(out_r);
    }

    0
}

struct OutputCallbackState {
    channels: usize,
    consumer: Consumer<f32>,
    is_active: Arc<AtomicBool>,
    safety_lockout: Arc<AtomicBool>,
}

struct HalOutputUnit {
    unit: sys::AudioUnit,
    state_ptr: *mut OutputCallbackState,
}

unsafe impl Send for HalOutputUnit {}

impl HalOutputUnit {
    fn start(
        device_id: sys::AudioDeviceID,
        channels: u32,
        sample_rate: f64,
        buffer_frames: u32,
        consumer: Consumer<f32>,
        is_active: Arc<AtomicBool>,
        safety_lockout: Arc<AtomicBool>,
    ) -> Result<Self> {
        let unit = create_hal_audio_unit()?;

        unsafe {
            check_os_status(
                sys::AudioUnitSetProperty(
                    unit,
                    sys::kAudioOutputUnitProperty_CurrentDevice,
                    sys::kAudioUnitScope_Global,
                    0,
                    &device_id as *const sys::AudioDeviceID as *const c_void,
                    mem::size_of::<sys::AudioDeviceID>() as u32,
                ),
                "CurrentDevice(Output)",
            )?;

            set_device_buffer_frame_size(device_id, sys::kAudioObjectPropertyScopeOutput, buffer_frames);

            let asbd = make_f32_interleaved_asbd(sample_rate, channels);
            check_os_status(
                sys::AudioUnitSetProperty(
                    unit,
                    sys::kAudioUnitProperty_StreamFormat,
                    sys::kAudioUnitScope_Input,
                    0,
                    &asbd as *const sys::AudioStreamBasicDescription as *const c_void,
                    mem::size_of::<sys::AudioStreamBasicDescription>() as u32,
                ),
                "StreamFormat(Output)",
            )?;

            let max_frames = MAX_FRAMES_PER_SLICE;
            check_os_status(
                sys::AudioUnitSetProperty(
                    unit,
                    sys::kAudioUnitProperty_MaximumFramesPerSlice,
                    sys::kAudioUnitScope_Global,
                    0,
                    &max_frames as *const u32 as *const c_void,
                    mem::size_of::<u32>() as u32,
                ),
                "MaximumFramesPerSlice(Output)",
            )?;

            let state = Box::new(OutputCallbackState {
                channels: channels as usize,
                consumer,
                is_active,
                safety_lockout,
            });
            let state_ptr = Box::into_raw(state);

            let cb = sys::AURenderCallbackStruct {
                inputProc: Some(output_render_callback),
                inputProcRefCon: state_ptr as *mut c_void,
            };
            if let Err(e) = check_os_status(
                sys::AudioUnitSetProperty(
                    unit,
                    sys::kAudioUnitProperty_SetRenderCallback,
                    sys::kAudioUnitScope_Input,
                    0,
                    &cb as *const sys::AURenderCallbackStruct as *const c_void,
                    mem::size_of::<sys::AURenderCallbackStruct>() as u32,
                ),
                "SetRenderCallback",
            ) {
                drop(Box::from_raw(state_ptr));
                sys::AudioComponentInstanceDispose(unit);
                return Err(e);
            }

            if let Err(e) = check_os_status(sys::AudioUnitInitialize(unit), "AudioUnitInitialize(Output)") {
                drop(Box::from_raw(state_ptr));
                sys::AudioComponentInstanceDispose(unit);
                return Err(e);
            }

            if let Err(e) = check_os_status(sys::AudioOutputUnitStart(unit), "AudioOutputUnitStart(Output)") {
                sys::AudioUnitUninitialize(unit);
                drop(Box::from_raw(state_ptr));
                sys::AudioComponentInstanceDispose(unit);
                return Err(e);
            }

            Ok(Self { unit, state_ptr })
        }
    }
}

impl Drop for HalOutputUnit {
    fn drop(&mut self) {
        unsafe {
            sys::AudioOutputUnitStop(self.unit);
            sys::AudioUnitUninitialize(self.unit);
            sys::AudioComponentInstanceDispose(self.unit);
            drop(Box::from_raw(self.state_ptr));
        }
    }
}

unsafe extern "C" fn output_render_callback(
    in_ref_con: *mut c_void,
    _io_action_flags: *mut sys::AudioUnitRenderActionFlags,
    _in_time_stamp: *const sys::AudioTimeStamp,
    _in_bus_number: sys::UInt32,
    in_number_frames: sys::UInt32,
    io_data: *mut sys::AudioBufferList,
) -> sys::OSStatus {
    let state = &mut *(in_ref_con as *mut OutputCallbackState);
    if io_data.is_null() || (*io_data).mNumberBuffers == 0 {
        return 0;
    }

    let buf = &mut (*io_data).mBuffers[0];
    if buf.mData.is_null() {
        return 0;
    }

    let sample_count = (in_number_frames as usize) * state.channels;
    let data = std::slice::from_raw_parts_mut(buf.mData as *mut f32, sample_count);

    let active = state.is_active.load(Ordering::Relaxed);
    let locked = state.safety_lockout.load(Ordering::Relaxed);

    if !active || locked {
        data.fill(0.0);
        while state.consumer.pop().is_ok() {}
        return 0;
    }

    let channels = state.channels;
    for frame in data.chunks_exact_mut(channels) {
        if channels >= 2 {
            frame[0] = state.consumer.pop().unwrap_or(0.0);
            frame[1] = state.consumer.pop().unwrap_or(0.0);
            for ch in &mut frame[2..] {
                *ch = 0.0;
            }
        } else if channels == 1 {
            let left = state.consumer.pop().unwrap_or(0.0);
            let right = state.consumer.pop().unwrap_or(0.0);
            frame[0] = (left + right) * 0.5;
        }
    }

    0
}

// ============================================================================
// CoreAudio Helper Functions
// ============================================================================

fn create_hal_audio_unit() -> Result<sys::AudioUnit> {
    let desc = sys::AudioComponentDescription {
        componentType: sys::kAudioUnitType_Output,
        componentSubType: sys::kAudioUnitSubType_HALOutput,
        componentManufacturer: sys::kAudioUnitManufacturer_Apple,
        componentFlags: 0,
        componentFlagsMask: 0,
    };

    unsafe {
        let comp = sys::AudioComponentFindNext(ptr::null_mut(), &desc);
        if comp.is_null() {
            return Err(anyhow!("Failed to find Apple HALOutput AudioComponent"));
        }
        let mut unit: sys::AudioUnit = ptr::null_mut();
        check_os_status(sys::AudioComponentInstanceNew(comp, &mut unit), "AudioComponentInstanceNew")?;
        Ok(unit)
    }
}

fn make_f32_interleaved_asbd(sample_rate: f64, channels: u32) -> sys::AudioStreamBasicDescription {
    let bytes_per_frame = 4 * channels;
    sys::AudioStreamBasicDescription {
        mSampleRate: sample_rate,
        mFormatID: sys::kAudioFormatLinearPCM,
        mFormatFlags: sys::kAudioFormatFlagIsFloat | sys::kAudioFormatFlagIsPacked,
        mBytesPerPacket: bytes_per_frame,
        mFramesPerPacket: 1,
        mBytesPerFrame: bytes_per_frame,
        mChannelsPerFrame: channels,
        mBitsPerChannel: 32,
        mReserved: 0,
    }
}

fn set_device_buffer_frame_size(
    device_id: sys::AudioDeviceID,
    scope: sys::AudioObjectPropertyScope,
    frames: u32,
) {
    let addr = sys::AudioObjectPropertyAddress {
        mSelector: sys::kAudioDevicePropertyBufferFrameSize,
        mScope: scope,
        mElement: sys::kAudioObjectPropertyElementMaster,
    };
    unsafe {
        let status = sys::AudioObjectSetPropertyData(
            device_id,
            &addr,
            0,
            ptr::null(),
            mem::size_of::<u32>() as u32,
            &frames as *const u32 as *const c_void,
        );
        if status != 0 {
            warn!("Could not set buffer frame size to {} (OSStatus {})", frames, status);
        }
    }
}

fn check_os_status(status: sys::OSStatus, op: &str) -> Result<()> {
    if status == 0 {
        Ok(())
    } else {
        Err(anyhow!("CoreAudio {} failed with OSStatus {}", op, status))
    }
}

fn find_audio_device_id_by_name(target_name: &str, is_input: bool) -> Option<sys::AudioDeviceID> {
    let addr = sys::AudioObjectPropertyAddress {
        mSelector: sys::kAudioHardwarePropertyDevices,
        mScope: sys::kAudioObjectPropertyScopeGlobal,
        mElement: sys::kAudioObjectPropertyElementMaster,
    };

    unsafe {
        let mut size: u32 = 0;
        if sys::AudioObjectGetPropertyDataSize(sys::kAudioObjectSystemObject, &addr, 0, ptr::null(), &mut size) != 0 {
            return None;
        }
        let count = (size as usize) / mem::size_of::<sys::AudioDeviceID>();
        let mut ids = vec![0 as sys::AudioDeviceID; count];
        if sys::AudioObjectGetPropertyData(
            sys::kAudioObjectSystemObject,
            &addr,
            0,
            ptr::null(),
            &mut size,
            ids.as_mut_ptr() as *mut c_void,
        ) != 0
        {
            return None;
        }

        let scope = if is_input {
            sys::kAudioObjectPropertyScopeInput
        } else {
            sys::kAudioObjectPropertyScopeOutput
        };

        for id in ids {
            if !device_has_streams_in_scope(id, scope) {
                continue;
            }
            if let Some(name) = get_device_name(id) {
                if name == target_name {
                    return Some(id);
                }
            }
        }
    }
    None
}

unsafe fn device_has_streams_in_scope(id: sys::AudioDeviceID, scope: sys::AudioObjectPropertyScope) -> bool {
    let addr = sys::AudioObjectPropertyAddress {
        mSelector: sys::kAudioDevicePropertyStreamConfiguration,
        mScope: scope,
        mElement: sys::kAudioObjectPropertyElementMaster,
    };
    let mut size: u32 = 0;
    if sys::AudioObjectGetPropertyDataSize(id, &addr, 0, ptr::null(), &mut size) != 0 || size == 0 {
        return false;
    }
    let mut buf = vec![0u8; size as usize];
    if sys::AudioObjectGetPropertyData(id, &addr, 0, ptr::null(), &mut size, buf.as_mut_ptr() as *mut c_void) != 0 {
        return false;
    }
    let list = &*(buf.as_ptr() as *const sys::AudioBufferList);
    list.mNumberBuffers > 0
}

unsafe fn get_device_name(id: sys::AudioDeviceID) -> Option<String> {
    let addr = sys::AudioObjectPropertyAddress {
        mSelector: sys::kAudioDevicePropertyDeviceNameCFString,
        mScope: sys::kAudioObjectPropertyScopeGlobal,
        mElement: sys::kAudioObjectPropertyElementMaster,
    };
    let mut cf_str: sys::CFStringRef = ptr::null();
    let mut size = mem::size_of::<sys::CFStringRef>() as u32;
    if sys::AudioObjectGetPropertyData(id, &addr, 0, ptr::null(), &mut size, &mut cf_str as *mut _ as *mut c_void) != 0
        || cf_str.is_null()
    {
        return None;
    }
    let mut buf = [0 as c_char; 256];
    let ok = sys::CFStringGetCString(
        cf_str,
        buf.as_mut_ptr(),
        buf.len() as sys::CFIndex,
        sys::kCFStringEncodingUTF8,
    );
    sys::CFRelease(cf_str as *const c_void);
    if ok != 0 {
        Some(CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned())
    } else {
        None
    }
}

// ============================================================================
// Device Discovery (via CPAL)
// ============================================================================

fn find_builtin_microphone(host: &cpal::Host) -> Result<(Device, String)> {
    let devices = host.input_devices().context("Failed to query input devices")?;
    let mut headset_mic: Option<(Device, String)> = None;
    let mut internal_mic: Option<(Device, String)> = None;

    for device in devices {
        if let Ok(name) = device.name() {
            let lower = name.to_lowercase();
            if lower.contains("external microphone") || lower.contains("headset") {
                headset_mic = Some((device, name));
                break;
            }
            if lower.contains("macbook") || lower.contains("built-in") || lower.contains("internal") {
                internal_mic = Some((device, name));
            }
        }
    }

    if let Some(mic) = headset_mic {
        return Ok(mic);
    }
    if let Some(mic) = internal_mic {
        return Ok(mic);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_list_all_devices() {
        let host = cpal::default_host();
        println!("\n=== CPAL INPUT DEVICES ===");
        if let Ok(devices) = host.input_devices() {
            for dev in devices {
                println!("Input: {:?}", dev.name());
            }
        }
        println!("=== CPAL OUTPUT DEVICES ===");
        if let Ok(devices) = host.output_devices() {
            for dev in devices {
                println!("Output: {:?}", dev.name());
            }
        }
    }
}
