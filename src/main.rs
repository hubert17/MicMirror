#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
compile_error!("MicMirror is exclusively engineered for Apple Silicon macOS (aarch64-apple-darwin).");

mod audio;

use anyhow::Result;
use audio::{AudioEngine, HeadphoneState, DEFAULT_PRESET_INDEX};
use log::{error, info};
use std::ffi::CString;

#[repr(C)]
pub struct AppCallbacks {
    pub on_toggle_active: extern "C" fn(bool),
    pub on_buffer_preset_changed: extern "C" fn(u32),
    pub on_input_volume_changed: extern "C" fn(f32),
    pub on_poll_devices: extern "C" fn(),
    pub on_quit: extern "C" fn(),
}

extern "C" {
    fn micmirror_ui_init(
        callbacks: AppCallbacks,
        initial_preset_idx: u32,
        initial_input_vol: f32,
        initial_active: bool,
    );
    fn micmirror_ui_set_headphone_state(connected: bool, device_name: *const std::os::raw::c_char);
    fn micmirror_ui_run();
}

static mut GLOBAL_ENGINE: *mut AudioEngine = std::ptr::null_mut();

extern "C" fn callback_toggle_active(active: bool) {
    info!("UI: Toggle active -> {}", active);
    unsafe {
        if !GLOBAL_ENGINE.is_null() {
            (*GLOBAL_ENGINE).set_active(active);
        }
    }
}

extern "C" fn callback_buffer_preset_changed(preset_idx: u32) {
    info!("UI: Buffer preset selected -> index {}", preset_idx);
    unsafe {
        if !GLOBAL_ENGINE.is_null() {
            if let Err(e) = (*GLOBAL_ENGINE).set_buffer_preset(preset_idx as usize) {
                error!("Failed to update CoreAudio buffer preset: {}", e);
            }
        }
    }
}

extern "C" fn callback_input_volume_changed(volume: f32) {
    unsafe {
        if !GLOBAL_ENGINE.is_null() {
            (*GLOBAL_ENGINE).set_input_volume(volume);
        }
    }
}

extern "C" fn callback_poll_devices() {
    unsafe {
        if !GLOBAL_ENGINE.is_null() {
            let eng = &mut *GLOBAL_ENGINE;

            // Re-check hardware connection
            if let Ok(changed) = eng.refresh_devices() {
                if changed {
                    let info = eng.device_info();
                    let connected = matches!(info.headphone_state, HeadphoneState::Connected(_));
                    let c_name = match &info.headphone_state {
                        HeadphoneState::Connected(name) => Some(CString::new(name.replace('\0', "")).unwrap_or_default()),
                        _ => None,
                    };
                    micmirror_ui_set_headphone_state(
                        connected,
                        c_name.as_ref().map_or(std::ptr::null(), |s| s.as_ptr()),
                    );
                }
            }
        }
    }
}

extern "C" fn callback_quit() {
    info!("UI: Quit selected");
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    info!("Starting MicMirror - Apple Silicon Ultra-Low Latency Sidetone");

    // Default: SoX Baseline (In 32f / Out 128f)
    let initial_preset = DEFAULT_PRESET_INDEX;
    // Leak the Box to ensure the engine lives for the entire process lifetime.
    // micmirror_ui_run() calls [NSApp run] which never returns.
    let engine = Box::leak(Box::new(match AudioEngine::new(initial_preset) {
        Ok(eng) => eng,
        Err(err) => {
            error!("Failed to initialize audio engine: {:#}", err);
            std::process::exit(1);
        }
    }));

    let is_active = engine.is_active();
    let initial_vol = engine.get_input_volume();
    let initial_info = engine.device_info().clone();
    let is_headphone_connected = matches!(initial_info.headphone_state, HeadphoneState::Connected(_));

    unsafe {
        GLOBAL_ENGINE = engine as *mut AudioEngine;
    }

    let callbacks = AppCallbacks {
        on_toggle_active: callback_toggle_active,
        on_buffer_preset_changed: callback_buffer_preset_changed,
        on_input_volume_changed: callback_input_volume_changed,
        on_poll_devices: callback_poll_devices,
        on_quit: callback_quit,
    };

    // Initialize native Cocoa menu bar UI
    unsafe {
        micmirror_ui_init(
            callbacks,
            initial_preset as u32,
            initial_vol,
            is_active,
        );

        let c_name = match &initial_info.headphone_state {
            HeadphoneState::Connected(name) => Some(CString::new(name.replace('\0', "")).unwrap_or_default()),
            _ => None,
        };
        micmirror_ui_set_headphone_state(
            is_headphone_connected,
            c_name.as_ref().map_or(std::ptr::null(), |s| s.as_ptr()),
        );
    }

    // Enter macOS AppKit main run loop
    unsafe {
        micmirror_ui_run();
    }

    Ok(())
}
