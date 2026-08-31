# ROBS - Rust OBS Studio

A complete rewrite of OBS Studio in Rust, designed for modern streaming workflows with multi-destination streaming, unified chat aggregation, and a dockable interface.

ROBS is **cross-platform**: Windows is the fully-featured reference platform, macOS builds and runs with camera/mic capture and full encoding/streaming support, and Linux builds with PulseAudio audio-input placeholders.

## Features

### Core Streaming
- **Multi-destination streaming** - Stream to Twitch, YouTube, Facebook, and other platforms simultaneously
- **RTMP output** with automatic reconnection and configurable retry logic
- **File recording** with support for MP4, MKV, FLV, and MOV containers
- **x264 encoder** with full preset support (ultrafast through veryslow) and CBR/VBR/CRF rate control modes
- **Multi-track audio** with per-source volume control and mixing

### User Interface
- **Dockable panel system** with resizable, toggleable panels
- **Preview window** with live indicator during streaming
- **Sources panel** for managing capture sources with visibility toggles
- **Scenes panel** for quick scene switching
- **Audio mixer** with per-channel volume sliders, mute buttons, and real-time level meters
- **Unified chat window** aggregating messages from multiple platforms
- **Real-time stats** showing duration, bitrate, frame rate, and dropped frames
- **Comprehensive settings** with tabs for General, Video, Audio, Hotkeys, Streaming, and Outputs

### Chat Integration
- **Twitch IRC** integration for real-time chat messages
- **YouTube Live Chat** support
- **Unified message display** with platform-specific color coding
- **Per-platform filtering** to view chat from specific sources

### Resilience & Telemetry
- **Blackbox dual recording** - an always-on background encode of the active scene, so footage is never lost to an unpressed Record button; crash-safe Matroska segments with `.part` marker recovery
- **Anomaly clip capture** - rolling pre-roll buffer that saves short clips on demand
- **PDF event-log reports** exportable from the UI

### Profiles & Settings
- **Profile system** with save/load/duplicate functionality
- **TOML-based serialization** for human-readable configuration
- **Video configuration** including resolution, FPS, and downscale filter
- **Audio configuration** with sample rate and channel settings
- **Streaming configuration** with server, bitrate, encoder, and keyframe settings

## Architecture

ROBS is organized as a Rust workspace with modular crates:

| Crate | Purpose |
|-------|---------|
| `robs-core` | Core types, traits, pipeline, event system, error handling |
| `robs-video` | Video processing pipeline and frame handling |
| `robs-audio` | Audio sources, mixing, and processing |
| `robs-encoding` | Encoder implementations (x264 with extensible trait system) |
| `robs-outputs` | RTMP streaming, file recording, multi-destination output |
| `robs-sources` | Capture sources (window, monitor, game, test pattern) |
| `robs-ui` | egui-based graphical user interface |
| `robs-plugins` | Plugin architecture with dynamic library loading |
| `robs-profiles` | Profile management and settings persistence |
| `robs-chat` | Multi-platform chat aggregation (Twitch, YouTube) |
| `robs` | Main application binary |

## Platform Support

| Capability | Windows | macOS | Linux |
|-----------|---------|-------|-------|
| Monitor / window capture | ✅ DXGI + gdigrab | ❌ planned (ScreenCaptureKit) | ❌ planned (PipeWire) |
| Webcam capture | ✅ DirectShow | ✅ AVFoundation | ❓ untested (v4l2) |
| Audio capture (mic + system) | ✅ DirectShow | ✅ AVFoundation | ❓ pulse placeholder |
| Recording / encoding (x264, NVENC) | ✅ | ✅ libx264 (NVENC auto-detects unavailable) | ✅ |
| RTMP streaming | ✅ | ✅ | ✅ |
| Blackbox / anomaly / snapshots | ✅ | ✅ | ✅ |

## Building

### Prerequisites

- Rust 1.75+ via rustup (`stable` — the workspace `rust-toolchain.toml` handles the toolchain; the Windows MSVC target is declared in `targets`)
- FFmpeg 6+ available on your `PATH` (the capture and encoding pipelines shell out to the `ffmpeg` command)

### FFmpeg Requirement

ROBS requires a recent version of FFmpeg to be installed on your system. The capture and encoding pipelines depend on FFmpeg being available as a system command. Ensure `ffmpeg` is accessible from your command line before running ROBS.

### Setup

Windows (MSVC toolchain):

```powershell
rustup toolchain install stable-x86_64-pc-windows-msvc
rustup default stable-x86_64-pc-windows-msvc
```

macOS (Homebrew):

```bash
brew install ffmpeg
```

Linux (Debian/Ubuntu):

```bash
sudo apt install ffmpeg
```

### Build

```bash
cargo build --release
```

On Windows the compiled binary will be at `target\x86_64-pc-windows-msvc\release\robs.exe`; on macOS/Linux at `target/release/robs`.

### Run

```bash
cargo run
```

## Current Status

This is a functional project with a working UI, capture, encoding, streaming, and recording pipeline on Windows. The following major components are implemented:

- ✅ Complete UI with all panels (Sources, Scenes, Preview, Audio Mixer, Chat, Stats, Settings)
- ✅ Multi-destination RTMP streaming with automatic reconnection
- ✅ Recording start/stop with timestamped filenames (MP4, MKV, FLV, MOV)
- ✅ FFmpeg H.264 software encoder with full preset support
- ✅ NVIDIA NVENC hardware encoder with auto-detection
- ✅ AAC audio encoder with bitrate control
- ✅ Encoder factory with availability detection (FFmpeg, NVENC, AAC)
- ✅ Capture sources: monitor/window (DXGI + gdigrab), webcam, test pattern
- ✅ Audio capture (DirectShow system audio + mic) and audio mixer with meters
- ✅ Blackbox always-on dual recording with crash recovery
- ✅ Anomaly clip capture with rolling pre-roll
- ✅ Profile management system with TOML serialization
- ✅ Chat aggregation framework
- ✅ Plugin loading architecture

### Platform Notes

- **Windows** is the primary development target; everything above works.
- **macOS** builds and runs since the cross-platform refactor: webcam/mic capture (AVFoundation), recording, streaming, blackbox/anomaly, and snapshots all work. Monitor/window capture is gated off until a native ScreenCaptureKit backend lands — those source types report a clear error in the UI. NVENC auto-detects as unavailable, so encoding falls back to libx264. FFmpeg ≥ 8 is expected (device enumeration matches its AVFoundation listing format).
- **Linux** compiles with a PulseAudio audio-input placeholder; not yet tested.

### Not Yet Implemented (High Priority)

1. **macOS screen/window capture** - ScreenCaptureKit backend to replace the gated DXGI/gdigrab paths
2. **Linux capture backends** - PipeWire (screen) and v4l2 (webcam) validation
3. **Game capture** - process-specific hooking

### Technical Status

The project builds and runs with:
- Windows (MSVC) as the fully-featured target
- macOS (aarch64-apple-darwin) build + test suite passing
- Cross-platform workspace configuration (`rust-toolchain.toml`, `.cargo/config.toml` are platform-neutral; Windows-only flags are scoped to the Windows target)
- FFmpeg dependency detection on startup
- NVENC hardware acceleration detection with graceful fallback
- `cargo test -p robs-ui -p robs-outputs` test suite (54 tests) passing on Windows and macOS

## Design Goals

- **Memory safety** through Rust's ownership system
- **Concurrency** with async/await and lock-free data structures where possible
- **Extensibility** through trait-based plugin architecture
- **Cross-platform** by construction - platform code is isolated behind `#[cfg]` gates with per-platform FFmpeg input backends
- **Performance** with LTO and optimized release builds

## License

GPL-3.0
