# Flora Compositor - Development Roadmap

## Current Status 
Flora is now a functional Wayland compositor capable of:
- **Dual Backend Support**: Winit (nested/development) and DRM (native/production)
- DRM/EGL/GLES rendering pipeline with smart damage tracking
- Accepting Wayland client connections
- Creating and rendering toplevel surfaces (windows)
- XDG shell support with server-side decorations
- Software cursor rendering with client cursor support
- Optimized frame callbacks (0% GPU idle, clients stay responsive)

## Phase 1: Core Input & Backend maybe complete

- [x] **Dual Backend Architecture**
  - [x] Winit backend for nested development mode
  - [x] DRM backend for native TTY mode
  - [x] Feature flag system (`--features winit`)
  - [x] Runtime backend switching

- [x] **Keyboard Input**
  - [x] Forward keyboard events to focused client
  - [x] Set `needs_redraw = true` for responsive typing
  - [x] Implement keyboard focus tracking
  - [x] Integrate Libinput directly into Calloop (Zero-thread model)
  - [x] Fix modifier state leak from parent compositor
  - [ ] Handle key repeat (Smithay's `KeyboardHandler`)

- [x] **Pointer/Mouse Input**
  - [x] Forward mouse events to client under cursor
  - [x] Support Relative Pointer Motion
  - [x] Support Absolute Pointer Motion (Tablet/VM mode)
  - [x] Implement pointer focus tracking (basic hit test)
  - [x] Software cursor rendering
  - [x] Client cursor image support via `SeatHandler::cursor_image`

- [x] **Rendering Optimization**
  - [x] Smart damage tracking (cursor movement detection)
  - [x] Separate frame callbacks from GPU rendering
  - [x] 0% GPU usage when idle
  - [x] Maintain client responsiveness (btop/htop auto-refresh)

## Phase 2: Window Management MOSTLY COMPLETE

- [] **Window Positioning**
  - [] Track window positions
  - [] Support window move operations (basic grab)
  - [ ] Handle client resize requests

- [] **Window Focus**
  - [] Click-to-focus support
  - [] Surface-to-surface focus switching
  - [ ] Visual focus indicators
  - [ ] Focus change notifications

- [] **Multiple Windows**
  - [] Window stacking/z-order refinement
  - [] Window list management (`Vec<Window>`)
  - [] Close window interaction (Egui title bar)

## Phase 3: Missing Wayland Protocols (Medium Priority)

- [ ] **Primary Selection** - Copy/paste between windows
- [x] **XDG Decoration** - Server-side window decorations support
- [ ] **Cursor Shape** - Server-side cursor themes
- [x] **Compositor Region** - Damage tracking implementation

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
- [x] Modularize `main.rs` into separate files (`compositor/`, `shell/`, `input/`, `backend/`)
- [ ] Add error handling and recovery
- [x] Implement proper logging/tracing base
- [ ] DRM backend verification in TTY environment

## Resources

- [Smithay Documentation](https://smithay.github.io/smithay/)
- [Wayland Protocol Reference](https://wayland.freedesktop.org/docs/html/)
- [wlroots Tinywl Example](https://gitlab.freedesktop.org/wlroots/wlroots/-/tree/master/tinywl)
