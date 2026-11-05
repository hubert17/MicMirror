#import <AppKit/AppKit.h>
#include "ui_bridge.h"

// Buffer presets: (Input frames, Output frames, Description)
typedef struct {
    uint32_t in_frames;
    uint32_t out_frames;
    const char *tag;
} BufferPreset;

static const BufferPreset BUFFER_PRESETS[7] = {
    {16,  64,  "Ultra Low"},
    {24,  96,  "Very Low"},
    {32, 128,  "SoX Baseline"}, // Default matching sox --input-buffer 32 --buffer 128
    {48, 128,  "Balanced In"},
    {64, 128,  "Safe Low"},
    {128, 256, "Medium"},
    {256, 512, "Stability"}
};

// Subclassed NSSwitch that persistently forces standard Apple Green when ON/Active
// even across menu open/close and window re-attachments
@interface GreenSwitch : NSSwitch
@end

@implementation GreenSwitch

- (void)updateGreenTrack {
    @try {
        [self setValue:[NSColor systemGreenColor] forKey:@"trackColor"];
    } @catch (id ex) {}
}

- (void)viewDidMoveToWindow {
    [super viewDidMoveToWindow];
    [self updateGreenTrack];
}

- (void)updateLayer {
    [super updateLayer];
    [self updateGreenTrack];
}

- (void)drawRect:(NSRect)dirtyRect {
    [self updateGreenTrack];
    [super drawRect:dirtyRect];
}

- (void)setState:(NSControlStateValue)state {
    [super setState:state];
    [self updateGreenTrack];
}

- (void)layout {
    [super layout];
    [self updateGreenTrack];
}

@end

@interface MicMirrorMenuController : NSObject <NSMenuDelegate>
@property (nonatomic, strong) NSStatusItem *statusItem;
@property (nonatomic, strong) GreenSwitch *toggleSwitch;
@property (nonatomic, strong) NSSlider *bufferSlider;
@property (nonatomic, strong) NSTextField *bufferLabel;
@property (nonatomic, strong) NSSlider *inputVolumeSlider;
@property (nonatomic, strong) NSTextField *inputVolumeLabel;
@property (nonatomic, strong) NSSlider *reverbSlider;
@property (nonatomic, strong) NSTextField *reverbLabel;
@property (nonatomic, strong) NSTextField *statusLabel;
@property (nonatomic, strong) NSTimer *pollTimer;
@property (nonatomic, assign) AppCallbacks callbacks;
@property (nonatomic, assign) BOOL isLocked;
@property (nonatomic, assign) BOOL isActive;
@property (nonatomic, copy) NSString *currentDeviceDisplayName;
@end

@implementation MicMirrorMenuController

- (instancetype)initWithCallbacks:(AppCallbacks)callbacks
               initialPresetIndex:(uint32_t)presetIdx
                  initialInputVol:(float)initialVol
                 initialReverbWet:(float)initialWet
                    initialActive:(BOOL)initialActive {
    self = [super init];
    if (self) {
        _callbacks = callbacks;
        _isLocked = NO;
        _isActive = initialActive;
        _currentDeviceDisplayName = @"3.5mm Headphone Jack";

        [self setupUIWithPresetIndex:presetIdx initialVol:initialVol initialWet:initialWet active:initialActive];

        // Periodic timer on Main RunLoop to poll headphone connection state (1 Hz)
        _pollTimer = [NSTimer scheduledTimerWithTimeInterval:1.0
                                                      target:self
                                                    selector:@selector(pollTimerFired:)
                                                    userInfo:nil
                                                     repeats:YES];
    }
    return self;
}

- (void)pollTimerFired:(NSTimer *)timer {
    if (self.callbacks.on_poll_devices) {
        self.callbacks.on_poll_devices();
    }
}

- (NSString *)formatReverbLabel:(float)val {
    int pct = (int)(val * 100.0f);
    if (pct <= 0) {
        return @"Vocal Reverb: Off";
    } else if (pct < 25) {
        return [NSString stringWithFormat:@"Vocal Reverb: %d%% [Small Room]", pct];
    } else if (pct < 50) {
        return [NSString stringWithFormat:@"Vocal Reverb: %d%% [Studio]", pct];
    } else if (pct < 75) {
        return [NSString stringWithFormat:@"Vocal Reverb: %d%% [Large Hall]", pct];
    } else if (pct < 95) {
        return [NSString stringWithFormat:@"Vocal Reverb: %d%% [Concert Hall]", pct];
    } else {
        return [NSString stringWithFormat:@"Vocal Reverb: %d%% [Cathedral]", pct];
    }
}

// Display only input frame and short description, removing ms delay
- (NSString *)formatBufferLabel:(uint32_t)index {
    if (index > 6) index = 2;
    BufferPreset p = BUFFER_PRESETS[index];
    return [NSString stringWithFormat:@"Buffer: %u frames [%s]", p.in_frames, p.tag];
}

- (void)setupUIWithPresetIndex:(uint32_t)presetIdx initialVol:(float)initialVol initialWet:(float)initialWet active:(BOOL)initialActive {
    // 1. Status Bar Item
    self.statusItem = [[NSStatusBar systemStatusBar] statusItemWithLength:NSSquareStatusItemLength];
    NSStatusBarButton *button = self.statusItem.button;
    if (button) {
        NSImage *icon = [NSImage imageWithSystemSymbolName:@"mic.fill" accessibilityDescription:@"MicMirror"];
        if (icon) {
            [icon setTemplate:YES];
            button.image = icon;
        } else {
            button.title = @"🎤";
        }
    }

    // 2. Menu and Custom Card Container
    NSMenu *menu = [[NSMenu alloc] init];
    menu.delegate = self;
    menu.minimumWidth = 280.0;

    CGFloat viewWidth = 280.0;
    CGFloat viewHeight = 224.0;
    NSView *cardView = [[NSView alloc] initWithFrame:NSMakeRect(0, 0, viewWidth, viewHeight)];

    // --- Header Section (Row 1) ---
    NSImageView *iconView = [[NSImageView alloc] initWithFrame:NSMakeRect(14, viewHeight - 38, 24, 24)];
    NSImage *appIcon = [NSImage imageWithSystemSymbolName:@"waveform.circle.fill" accessibilityDescription:@"Logo"];
    if (appIcon) {
        [appIcon setTemplate:YES];
        iconView.image = appIcon;
    }
    [cardView addSubview:iconView];

    NSTextField *titleLabel = [[NSTextField alloc] initWithFrame:NSMakeRect(46, viewHeight - 27, 140, 18)];
    titleLabel.stringValue = @"MicMirror";
    titleLabel.font = [NSFont systemFontOfSize:13.5 weight:NSFontWeightSemibold];
    titleLabel.textColor = [NSColor labelColor];
    titleLabel.bezeled = NO;
    titleLabel.drawsBackground = NO;
    titleLabel.editable = NO;
    titleLabel.selectable = NO;
    [cardView addSubview:titleLabel];

    NSTextField *subLabel = [[NSTextField alloc] initWithFrame:NSMakeRect(46, viewHeight - 42, 140, 14)];
    subLabel.stringValue = @"Zero-Latency Sidetone";
    subLabel.font = [NSFont systemFontOfSize:10.5 weight:NSFontWeightRegular];
    subLabel.textColor = [NSColor secondaryLabelColor];
    subLabel.bezeled = NO;
    subLabel.drawsBackground = NO;
    subLabel.editable = NO;
    subLabel.selectable = NO;
    [cardView addSubview:subLabel];

    // GreenSwitch: Persistent Apple Green switch
    self.toggleSwitch = [[GreenSwitch alloc] initWithFrame:NSMakeRect(viewWidth - 52, viewHeight - 38, 38, 24)];
    self.toggleSwitch.state = initialActive ? NSControlStateValueOn : NSControlStateValueOff;
    [self.toggleSwitch updateGreenTrack];
    self.toggleSwitch.target = self;
    self.toggleSwitch.action = @selector(switchToggled:);
    [cardView addSubview:self.toggleSwitch];

    // Separator 1
    NSBox *sep1 = [[NSBox alloc] initWithFrame:NSMakeRect(14, viewHeight - 50, viewWidth - 28, 1)];
    sep1.boxType = NSBoxSeparator;
    [cardView addSubview:sep1];

    // --- Buffer Size Section (Row 2) ---
    self.bufferLabel = [[NSTextField alloc] initWithFrame:NSMakeRect(14, viewHeight - 72, viewWidth - 28, 16)];
    self.bufferLabel.stringValue = [self formatBufferLabel:presetIdx];
    self.bufferLabel.font = [NSFont monospacedDigitSystemFontOfSize:11.5 weight:NSFontWeightMedium];
    self.bufferLabel.textColor = [NSColor labelColor];
    self.bufferLabel.bezeled = NO;
    self.bufferLabel.drawsBackground = NO;
    self.bufferLabel.editable = NO;
    self.bufferLabel.selectable = NO;
    [cardView addSubview:self.bufferLabel];

    self.bufferSlider = [[NSSlider alloc] initWithFrame:NSMakeRect(14, viewHeight - 94, viewWidth - 28, 20)];
    self.bufferSlider.minValue = 0.0;
    self.bufferSlider.maxValue = 6.0;
    self.bufferSlider.numberOfTickMarks = 7;
    self.bufferSlider.allowsTickMarkValuesOnly = YES;
    self.bufferSlider.intValue = presetIdx;
    self.bufferSlider.continuous = YES;
    self.bufferSlider.target = self;
    self.bufferSlider.action = @selector(bufferSliderChanged:);
    [cardView addSubview:self.bufferSlider];

    // Separator 2
    NSBox *sep2 = [[NSBox alloc] initWithFrame:NSMakeRect(14, viewHeight - 104, viewWidth - 28, 1)];
    sep2.boxType = NSBoxSeparator;
    [cardView addSubview:sep2];

    // --- Input Volume Section (Row 3) ---
    int volPercent = (int)(initialVol * 100.0f);
    self.inputVolumeLabel = [[NSTextField alloc] initWithFrame:NSMakeRect(14, viewHeight - 124, viewWidth - 28, 16)];
    self.inputVolumeLabel.stringValue = [NSString stringWithFormat:@"Input volume: %d%%", volPercent];
    self.inputVolumeLabel.font = [NSFont systemFontOfSize:11.5 weight:NSFontWeightRegular];
    self.inputVolumeLabel.textColor = [NSColor labelColor];
    self.inputVolumeLabel.bezeled = NO;
    self.inputVolumeLabel.drawsBackground = NO;
    self.inputVolumeLabel.editable = NO;
    self.inputVolumeLabel.selectable = NO;
    [cardView addSubview:self.inputVolumeLabel];

    // Left mic icon (small)
    NSImageView *micLow = [[NSImageView alloc] initWithFrame:NSMakeRect(14, viewHeight - 144, 14, 14)];
    NSImage *imgLow = [NSImage imageWithSystemSymbolName:@"mic" accessibilityDescription:nil];
    if (imgLow) { [imgLow setTemplate:YES]; micLow.image = imgLow; }
    [cardView addSubview:micLow];

    // Input volume slider
    self.inputVolumeSlider = [[NSSlider alloc] initWithFrame:NSMakeRect(32, viewHeight - 146, viewWidth - 66, 18)];
    self.inputVolumeSlider.minValue = 0.0;
    self.inputVolumeSlider.maxValue = 1.0;
    self.inputVolumeSlider.floatValue = initialVol;
    self.inputVolumeSlider.continuous = YES;
    self.inputVolumeSlider.target = self;
    self.inputVolumeSlider.action = @selector(volumeSliderChanged:);
    [cardView addSubview:self.inputVolumeSlider];

    // Right mic icon (full)
    NSImageView *micHigh = [[NSImageView alloc] initWithFrame:NSMakeRect(viewWidth - 28, viewHeight - 144, 14, 14)];
    NSImage *imgHigh = [NSImage imageWithSystemSymbolName:@"mic.fill" accessibilityDescription:nil];
    if (imgHigh) { [imgHigh setTemplate:YES]; micHigh.image = imgHigh; }
    [cardView addSubview:micHigh];

    // Separator 3
    NSBox *sep3 = [[NSBox alloc] initWithFrame:NSMakeRect(14, viewHeight - 156, viewWidth - 28, 1)];
    sep3.boxType = NSBoxSeparator;
    [cardView addSubview:sep3];

    // --- Vocal Reverb Section (Row 4) ---
    self.reverbLabel = [[NSTextField alloc] initWithFrame:NSMakeRect(14, viewHeight - 176, viewWidth - 28, 16)];
    self.reverbLabel.stringValue = [self formatReverbLabel:initialWet];
    self.reverbLabel.font = [NSFont systemFontOfSize:11.5 weight:NSFontWeightRegular];
    self.reverbLabel.textColor = [NSColor labelColor];
    self.reverbLabel.bezeled = NO;
    self.reverbLabel.drawsBackground = NO;
    self.reverbLabel.editable = NO;
    self.reverbLabel.selectable = NO;
    [cardView addSubview:self.reverbLabel];

    // Left spark/reverb icon
    NSImageView *verbLow = [[NSImageView alloc] initWithFrame:NSMakeRect(14, viewHeight - 196, 14, 14)];
    NSImage *imgVerbLow = [NSImage imageWithSystemSymbolName:@"sparkles" accessibilityDescription:nil];
    if (imgVerbLow) { [imgVerbLow setTemplate:YES]; verbLow.image = imgVerbLow; }
    [cardView addSubview:verbLow];

    // Reverb slider (0.0 to 1.0)
    self.reverbSlider = [[NSSlider alloc] initWithFrame:NSMakeRect(32, viewHeight - 198, viewWidth - 46, 18)];
    self.reverbSlider.minValue = 0.0;
    self.reverbSlider.maxValue = 1.0;
    self.reverbSlider.floatValue = initialWet;
    self.reverbSlider.continuous = YES;
    self.reverbSlider.target = self;
    self.reverbSlider.action = @selector(reverbSliderChanged:);
    [cardView addSubview:self.reverbSlider];

    // --- Status Section (Row 5) ---
    self.statusLabel = [[NSTextField alloc] initWithFrame:NSMakeRect(14, 6, viewWidth - 28, 16)];
    self.statusLabel.stringValue = @"● 3.5mm Headphone Jack Connected";
    self.statusLabel.font = [NSFont systemFontOfSize:11.0 weight:NSFontWeightMedium];
    self.statusLabel.textColor = [NSColor systemGreenColor];
    self.statusLabel.bezeled = NO;
    self.statusLabel.drawsBackground = NO;
    self.statusLabel.editable = NO;
    self.statusLabel.selectable = NO;
    [cardView addSubview:self.statusLabel];

    // Add to menu
    NSMenuItem *cardMenuItem = [[NSMenuItem alloc] init];
    cardMenuItem.view = cardView;
    [menu addItem:cardMenuItem];

    [menu addItem:[NSMenuItem separatorItem]];

    // Quit Menu Item
    NSMenuItem *quitItem = [[NSMenuItem alloc] initWithTitle:@"Quit MicMirror"
                                                      action:@selector(quitClicked:)
                                               keyEquivalent:@"q"];
    quitItem.target = self;
    [menu addItem:quitItem];

    self.statusItem.menu = menu;
}

// NSMenuDelegate: Called every time menu is about to open
- (void)menuWillOpen:(NSMenu *)menu {
    // Re-assert switch state and green track color on menu appearance
    if (self.isLocked) {
        self.toggleSwitch.state = NSControlStateValueOff;
        self.toggleSwitch.enabled = NO;
    } else {
        self.toggleSwitch.state = self.isActive ? NSControlStateValueOn : NSControlStateValueOff;
        self.toggleSwitch.enabled = YES;
    }
    [self.toggleSwitch updateGreenTrack];
}

- (void)switchToggled:(NSSwitch *)sender {
    if (self.isLocked) {
        sender.state = NSControlStateValueOff;
        return;
    }
    BOOL active = (sender.state == NSControlStateValueOn);
    self.isActive = active;
    [self.toggleSwitch updateGreenTrack];

    if (self.callbacks.on_toggle_active) {
        self.callbacks.on_toggle_active(active);
    }
}

- (void)bufferSliderChanged:(NSSlider *)sender {
    int idx = sender.intValue;
    if (idx < 0) idx = 0;
    if (idx > 6) idx = 6;

    self.bufferLabel.stringValue = [self formatBufferLabel:idx];

    if (self.callbacks.on_buffer_preset_changed) {
        self.callbacks.on_buffer_preset_changed(idx);
    }
}

- (void)volumeSliderChanged:(NSSlider *)sender {
    float val = sender.floatValue;
    int pct = (int)(val * 100.0f);
    self.inputVolumeLabel.stringValue = [NSString stringWithFormat:@"Input volume: %d%%", pct];

    if (self.callbacks.on_input_volume_changed) {
        self.callbacks.on_input_volume_changed(val);
    }
}

- (void)reverbSliderChanged:(NSSlider *)sender {
    float val = sender.floatValue;
    self.reverbLabel.stringValue = [self formatReverbLabel:val];

    if (self.callbacks.on_reverb_changed) {
        self.callbacks.on_reverb_changed(val);
    }
}

- (void)quitClicked:(id)sender {
    if (self.callbacks.on_quit) {
        self.callbacks.on_quit();
    }
    [NSApp terminate:nil];
}

- (void)updateHeadphoneState:(BOOL)connected deviceName:(const char *)name {
    NSString *dName = (name && strlen(name) > 0) ? [NSString stringWithUTF8String:name] : @"3.5mm Headphone Jack";
    dispatch_async(dispatch_get_main_queue(), ^{
        self.isLocked = !connected;
        self.currentDeviceDisplayName = dName;
        if (connected) {
            self.statusLabel.stringValue = [NSString stringWithFormat:@"● %@", dName];
            self.statusLabel.textColor = [NSColor systemGreenColor];
            self.toggleSwitch.enabled = YES;
            self.toggleSwitch.state = self.isActive ? NSControlStateValueOn : NSControlStateValueOff;
        } else {
            self.statusLabel.stringValue = @"⚠️ Auto-Muted (Plug in 3.5mm)";
            self.statusLabel.textColor = [NSColor systemRedColor];
            self.toggleSwitch.state = NSControlStateValueOff;
            self.toggleSwitch.enabled = NO;
        }
        [self.toggleSwitch updateGreenTrack];
    });
}

- (void)updateActive:(BOOL)active {
    dispatch_async(dispatch_get_main_queue(), ^{
        self.isActive = active;
        self.toggleSwitch.state = active ? NSControlStateValueOn : NSControlStateValueOff;
        [self.toggleSwitch updateGreenTrack];
    });
}

@end

static MicMirrorMenuController *g_menuController = nil;

void micmirror_ui_init(
    AppCallbacks callbacks,
    uint32_t initial_preset_idx,
    float initial_input_vol,
    float initial_reverb_wet,
    bool initial_active
) {
    [NSApplication sharedApplication];
    [NSApp setActivationPolicy:NSApplicationActivationPolicyAccessory];

    g_menuController = [[MicMirrorMenuController alloc] initWithCallbacks:callbacks
                                                       initialPresetIndex:initial_preset_idx
                                                          initialInputVol:initial_input_vol
                                                         initialReverbWet:initial_reverb_wet
                                                            initialActive:initial_active];
}

void micmirror_ui_set_headphone_state(bool connected, const char* _Nullable device_name) {
    if (g_menuController) {
        [g_menuController updateHeadphoneState:connected deviceName:device_name];
    }
}

void micmirror_ui_set_active(bool active) {
    if (g_menuController) {
        [g_menuController updateActive:active];
    }
}

void micmirror_ui_run(void) {
    [NSApp run];
}
