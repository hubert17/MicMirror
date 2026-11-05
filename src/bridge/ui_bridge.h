#ifndef UI_BRIDGE_H
#define UI_BRIDGE_H

#include <stdbool.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct {
    void (* _Nonnull on_toggle_active)(bool active);
    void (* _Nonnull on_buffer_preset_changed)(uint32_t index);
    void (* _Nonnull on_input_volume_changed)(float volume);
    void (* _Nonnull on_reverb_changed)(float wet);
    void (* _Nonnull on_poll_devices)(void);
    void (* _Nonnull on_quit)(void);
} AppCallbacks;

// Initialize the native AppKit menu bar UI
void micmirror_ui_init(
    AppCallbacks callbacks,
    uint32_t initial_preset_idx,
    float initial_input_vol,
    float initial_reverb_wet,
    bool initial_active
);

// Update status from audio engine (e.g. headphone plugged/unplugged)
void micmirror_ui_set_headphone_state(bool connected, const char* _Nullable device_name);

// Update toggle switch state from audio engine
void micmirror_ui_set_active(bool active);

// Run the NSApplication main run loop
void micmirror_ui_run(void);

#ifdef __cplusplus
}
#endif

#endif // UI_BRIDGE_H
