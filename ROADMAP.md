# Flora Compositor - Development Roadmap

## Current Status 
Flora is now a functional Wayland compositor capable of:
- DRM/EGL/GLES rendering pipeline
- Accepting Wayland client connections
- Creating and rendering toplevel surfaces (windows)
- Basic XDG shell support

- [x] **Keyboard Input**
  - [x] Forward keyboard events to focused client
  - [x] Set `needs_redraw = true` for responsive typing
  - [x] Implement keyboard focus tracking
  - [x] Integrate Libinput directly into Calloop (Zero-thread model)
  - [ ] Handle key repeat (Smithay's `KeyboardHandler`)

- [x] **Pointer/Mouse Input**
  - [x] Forward mouse events to client under cursor
  - [x] Support Relative Pointer Motion
  - [x] Support Absolute Pointer Motion (Tablet/VM mode)
  - [x] Implement pointer focus tracking (basic hit test)
  - [ ] Handle cursor image updates from clients

## Phase 2: Window Management (High Priority)
- [x] **Window Positioning**
  - [x] Track window positions
  - [x] Support window move operations (basic grab)
  - [ ] Handle client resize requests
- [x] **Window Focus**
  - [x] Click-to-focus support
  - [x] Surface-to-surface focus switching
  - [ ] Visual focus indicators
  - [ ] Focus change notifications

- [x] **Multiple Windows**
  - [x] Window stacking/z-order refinement
  - [x] Window list management (`Vec<Window>`)
  - [x] Close window interaction (Egui title bar)

## Phase 3: Missing Wayland Protocols (Medium Priority)
- [ ] **Primary Selection** - Copy/paste between windows
- [x] **XDG Decoration** - Initial server-side window decorations support
- [ ] **Cursor Shape** - Server-side cursor themes
- [x] **Compositor Region** - Basic damage tracking implementation

## Phase 4: Desktop Features (Lower Priority)
- [ ] **Background/Wallpaper** - Desktop background layer
- [ ] **Panel/Dock** - Task bar implementation
- [ ] **Notifications** - Desktop notification support
- [ ] **Screenshots** - Screen capture protocol

## Phase 5: macOS-like Features (Vision)
- [ ] **Smooth Animations** - Window open/close effects
- [ ] **Blur Effects** - Frosted glass UI elements
- [ ] **Global Menu Bar** - macOS-style menu integration
- [ ] **Mission Control** - Window overview mode
- [ ] **Hot Corners** - Screen edge triggers

## Technical Debt
- [x] Clean up unused imports/dead code
- [x] Modularize `main.rs` into separate files (`compositor/`, `shell/`, `input/`)
- [ ] Add error handling and recovery
- [x] Implement proper logging/tracing base

## Resources
- [Smithay Documentation](https://smithay.github.io/smithay/)
- [Wayland Protocol Reference](https://wayland.freedesktop.org/docs/html/)
- [wlroots Tinywl Example](https://gitlab.freedesktop.org/wlroots/wlroots/-/tree/master/tinywl)
