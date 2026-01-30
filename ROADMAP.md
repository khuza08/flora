# Flora Compositor - Development Roadmap

## Current Status ✅
Flora is now a functional Wayland compositor capable of:
- DRM/EGL/GLES rendering pipeline
- Accepting Wayland client connections
- Creating and rendering toplevel surfaces (windows)
- Basic XDG shell support

## Phase 1: Core Input Handling (High Priority)
- [ ] **Keyboard Input**
  - Forward keyboard events to focused client
  - Handle key repeat
  - Implement keyboard focus tracking

- [ ] **Pointer/Mouse Input**
  - Forward mouse events to client under cursor
  - Implement pointer focus tracking
  - Handle cursor image updates from clients

## Phase 2: Window Management (High Priority)
- [ ] **Window Positioning**
  - Track window positions
  - Support window move operations
  - Handle client resize requests

- [ ] **Window Focus**
  - Click-to-focus support
  - Visual focus indicators
  - Focus change notifications

- [ ] **Multiple Windows**
  - Window stacking/z-order
  - Window list management
  - Close window handling

## Phase 3: Missing Wayland Protocols (Medium Priority)
- [ ] **Primary Selection** - Copy/paste between windows
- [ ] **XDG Decoration** - Server-side window decorations
- [ ] **Cursor Shape** - Server-side cursor themes
- [ ] **Compositor Region** - Damage tracking optimization

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
- [ ] Clean up unused imports/dead code
- [ ] Modularize main.rs into separate files
- [ ] Add error handling and recovery
- [ ] Implement proper logging levels

## Resources
- [Smithay Documentation](https://smithay.github.io/smithay/)
- [Wayland Protocol Reference](https://wayland.freedesktop.org/docs/html/)
- [wlroots Tinywl Example](https://gitlab.freedesktop.org/wlroots/wlroots/-/tree/master/tinywl)
