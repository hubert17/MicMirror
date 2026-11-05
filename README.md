# MicMirror

**Ultra-low latency real-time microphone monitoring (sidetone) for macOS Apple Silicon.**

MicMirror replicates and refines the zero-perceivable-latency loopback behavior of:
```bash
sox -V0 --multi-threaded --input-buffer 32 --buffer 128 -d -d
```
packaged into a lightweight, native macOS menu bar application with acoustic safety guards and volume controls.

---

## Why MicMirror?

When wearing closed-back headphones or in-ear monitors (IEMs), the **occlusion effect** dampens your perception of your own voice, often causing vocal strain, loud speaking, or speech hesitation. Traditional software passthrough solutions introduce distracting 15–50 ms delays that make natural speaking impossible.

MicMirror delivers **sub-4 ms round-trip hardware monitoring** by pairing Apple Silicon's motherboard-level audio DMA with lock-free real-time audio threads.

---

## Visual Interface

MicMirror lives exclusively in your menu bar (`LSUIElement = true`) with a modern, native AppKit card interface:

```
┌────────────────────────────────────────────────────────┐
│  [🎙️] MicMirror                     [   ON / OFF 🟢 ]    │  <-- Persistent Apple Green
│      Apple Silicon 3.5mm Sidetone                      │
├────────────────────────────────────────────────────────┤
│  Buffer: 32 frames [SoX Baseline]                      │  <-- 7-Tick Snap Slider
│  [ 16 ── 24 ──●── 48 ── 64 ── 128 ── 256 ]             │
├────────────────────────────────────────────────────────┤
│  Input volume: 75%                                     │  <-- Real-time Gain Slider
│  🎙️ [ ─────────────●───────────── ] 🎙️                  │
├────────────────────────────────────────────────────────┤
│  ● External Headphones Connected                       │  <-- Hardware State
│  ───────────────────────────────────────────────────── │
│  Quit MicMirror                                    ⌘Q  │
└────────────────────────────────────────────────────────┘
```

---

## Key Features

- **Apple Silicon Hardware Lock**: Exclusively targets `aarch64-apple-darwin`. Eliminates USB controller polling delays and shared USB bus jitter by interfacing directly with Apple's internal audio controller.
- **Zero Clock Drift**: Built-in microphone array and the 3.5mm headphone DAC are driven by the exact same motherboard crystal oscillator—eliminating sample slippage and drift compensation overhead.
- **Exact SoX Default Parity**: Opens by default with **32 frames input** and **128 frames output** at 48 kHz.
- **Acoustic Feedback Protection (Auto-Mute)**: If 3.5mm headphones are unplugged, the app immediately disengages the audio stream to prevent dangerous high-pitched feedback squeal between the built-in mic and MacBook speakers.
- **Lock-Free, Real-Time Audio**: Powered by [`rtrb`](https://crates.io/crates/rtrb) (wait-free, zero heap allocation, cache-line padded against false sharing).
- **Persistent Apple Green Switch**: Custom Cocoa switch integration that stays vibrant system green whenever monitoring is active.
- **Integrated Input Volume**: Adjust your microphone gain directly inside the menu bar without opening **System Settings → Sound**.

---

## Buffer Latency Scale (48 kHz)

The slider features 7 discrete tick marks to customize your buffer based on stability and hardware tolerance:

| Tick | Preset Name | Buffer Display | Underlying Config | Estimated Delay | Notes |
| :---: | :--- | :--- | :---: | :---: | :--- |
| **0** | Ultra Low | `Buffer: 16 frames [Ultra Low]` | In 16f / Out 64f | ~1.67 ms | Absolute lowest hardware DMA limit |
| **1** | Very Low | `Buffer: 24 frames [Very Low]` | In 24f / Out 96f | ~2.50 ms | Sub-3ms round-trip |
| **2** | **SoX Baseline** | `Buffer: 32 frames [SoX Baseline]` | **In 32f / Out 128f** | **~3.33 ms** | **Default (Exact SoX Parity)** |
| **3** | Balanced In | `Buffer: 48 frames [Balanced In]` | In 48f / Out 128f | ~3.67 ms | 1 ms input frame duration |
| **4** | Safe Low | `Buffer: 64 frames [Safe Low]` | In 64f / Out 128f | ~4.00 ms | Ultra-stable low latency |
| **5** | Medium | `Buffer: 128 frames [Medium]` | In 128f / Out 256f | ~8.00 ms | Moderate buffer |
| **6** | Stability | `Buffer: 256 frames [Stability]` | In 256f / Out 512f | ~16.00 ms | High buffer tolerance |

---

## Architecture

```
┌────────────────────────────────────────────────────────┐
│                   macOS Menu Bar UI                    │
│        (Native AppKit NSStatusItem + Custom NSView)    │
│    [GreenSwitch Toggle] [Buffer Slider] [Input Volume] │
└──────────────────────────┬─────────────────────────────┘
                           │ Lock-Free Atomics (0.000 ms overhead)
                           ▼
┌────────────────────────────────────────────────────────┐
│                 MicMirror Audio Engine                 │
│                                                        │
│  ┌───────────────────────┐   Push (Mono)  ┌─────────┐ │
│  │ CoreAudio Input (HAL) │───────────────►│  rtrb   │ │
│  │ In: 32 frames (0.67ms)│                │  Ring   │ │
│  └───────────────────────┘                │ Buffer  │ │
│                                           │  (SPSC) │ │
│  ┌───────────────────────┐   Pop (Stereo) │         │ │
│  │ CoreAudio Output(HAL) │◄───────────────┤         │ │
│  │ Out:128 frames (2.67ms│                └─────────┘ │
│  └───────────────────────┘                             │
│                                                        │
│  [Auto-Mute Guard] Disables stream if 3.5mm unplugged  │
└────────────────────────────────────────────────────────┘
```

- **Thread Separation**: AppKit controls live on the Cocoa Main Thread. CoreAudio runs on Mach real-time scheduled threads (`THREAD_TIME_CONSTRAINT_POLICY`). UI actions write to atomic primitives; the audio callback never blocks, allocates, or waits for locks.
- **Channel Conversion**: Downmixes microphone input array to mono and duplicates across Left and Right headphone channels for naturally centered voice positioning.

---

## System Requirements

- **Mac**: Apple Silicon (`M1`, `M2`, `M3`, `M4` or later)
- **OS**: macOS 12 (Monterey) or later
- **Output Device**: 3.5mm analog headphones or headset plugged into the headphone jack
- **Build Requirements**: Rust toolchain (`cargo`, `rustc`), Xcode Command Line Tools

---

## Building and Running

### 1. Clone & Build
Run the automated packaging script to build in release mode, bundle the `.app`, attach `Info.plist`, and apply an ad-hoc signature:

```bash
./scripts/build_app.sh
```

### 2. Launch
```bash
open target/release/MicMirror.app
```

> **Note on Permissions**: On first launch, macOS will display a system dialog requesting permission to access the **Microphone**. Click **OK** to allow real-time loopback.

---

## Frequently Asked Questions (FAQ)

#### Why does MicMirror only run on the 3.5mm headphone jack?
1. **Hearing Safety**: If zero-latency monitoring runs through built-in speakers while the built-in mic is live, it creates an immediate acoustic feedback squeal.
2. **True Sub-Millisecond DMA**: Apple Silicon's internal headphone jack connects directly over memory-mapped DMA to the analog codec, bypassing USB controller packet latency.
3. **Bluetooth Latency**: Bluetooth audio codecs (AAC, SBC) impose a physical latency floor of 30–150 ms, which causes distracting auditory disorientation for live speech.

#### How do I quit the application?
Click the menu bar icon and select **Quit MicMirror** or press `⌘Q`.
