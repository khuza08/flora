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

Flora supports two backends for different workflows:

### 🖥️ Development Mode (Winit/Nested)
Runs as a window inside your current Wayland or X11 compositor (e.g., Hyprland, GNOME). **No root required.**

```bash
cargo run
```

### 🎮 Production Mode (DRM/Native)
Runs natively from a TTY. Requires switching to a console (e.g., `Ctrl+Alt+F3`) and root privileges for DRM/input access.

```bash
# Using sudo to allow access to /dev/dri and /dev/input
sudo -E cargo run --no-default-features
```

## Vision
To create a high-performance Wayland desktop experience that brings the polish and aesthetic of macOS to Linux, focusing on smooth animations, frosted glass effects, and a cohesive user interface.
