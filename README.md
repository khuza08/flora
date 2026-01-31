# Flora Compositor
A modern, macOS-inspired(Future) Wayland compositor built with [Smithay](https://smithay.github.io/smithay/).

<img width="512" alt="flora 26.0.1 is live" src="https://github.com/user-attachments/assets/bcf28f0a-3a36-4451-9af4-90d13c53ef3a" />

## Current State
Flora is in early development but already supports:
- **Low-Latency Rendering**: Direct DRM/GBM/EGL pipeline with Glow/GLES.
- **Responsive Input**: Optimized background input thread with proactive Wayland socket flushing.
- **Universal Pointer Support**: Handles both relative (mouse) and absolute (tablet/VM) pointer events.
- **Window Management**: Basic XDG shell support with window tracking and grab-to-move.
- **Clean Diagnostics**: Intelligent log suppression for non-critical setup errors.

## Running Flora
Requires root privileges for DRM access:
```bash
sudo -E ./target/debug/flora
```

## Vision
To create a high-performance Wayland desktop experience that brings the polish and aesthetic of macOS to Linux, focusing on smooth animations, frosted glass effects, and a cohesive user interface.
