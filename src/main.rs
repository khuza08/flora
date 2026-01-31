use smithay::{
    backend::{
        udev::{UdevBackend, UdevEvent},
        drm::{DrmDevice, DrmDeviceFd, compositor::DrmCompositor},
        allocator::{gbm::{GbmDevice, GbmAllocator}, Fourcc},
        drm::exporter::gbm::GbmFramebufferExporter,
        egl::{EGLDisplay, EGLContext},
        renderer::{
            glow::GlowRenderer,
            ImportDma,
            element::surface::{render_elements_from_surface_tree, WaylandSurfaceRenderElement},
            element::Kind,
            utils::on_commit_buffer_handler,
        },
    },
    output::OutputModeSource,
    reexports::{
        calloop::{EventLoop},
        wayland_server::{Display, DisplayHandle, Client, backend::ClientData},
        drm::control::{connector, Device as _},
    },
    utils::{DeviceFd, Transform, Size, Scale, Physical, Point},
    wayland::{
        compositor::{CompositorState, CompositorHandler, CompositorClientState, with_surface_tree_downward, TraversalAction, SurfaceAttributes},
        socket::ListeningSocketSource,
        shell::xdg::{
            XdgShellState, XdgShellHandler, ToplevelSurface, PopupSurface, PositionerState,
            decoration::{XdgDecorationState, XdgDecorationHandler},
        },
        shm::{ShmState, ShmHandler},
        buffer::BufferHandler,
        selection::{
            SelectionHandler,
            data_device::{DataDeviceState, DataDeviceHandler, ClientDndGrabHandler, ServerDndGrabHandler},
            primary_selection::{PrimarySelectionState, PrimarySelectionHandler},
        },
        output::OutputHandler,
        text_input::{TextInputManagerState, TextInputSeat},
    },
};

use smithay::reexports::input::{
    Event as LibinputEvent, 
    event::{
        EventTrait, 
        keyboard::{KeyboardEventTrait, KeyState}, 
        pointer::{PointerEventTrait, ButtonState, PointerEvent},
        device::{DeviceEvent},
    },
};
use smithay::{
    output::{Output, PhysicalProperties, Subpixel, Mode as OutputMode},
    input::{SeatState, SeatHandler, Seat},
    input::keyboard::FilterResult,
    utils::SERIAL_COUNTER,
};
use tracing::{info, warn, error};
use std::{time::Duration, sync::Arc, path::PathBuf, fs::OpenOptions, os::unix::io::{AsRawFd, FromRawFd}};
use libc;

pub struct Window {
    pub toplevel: ToplevelSurface,
    pub location: Point<i32, Physical>,
}

pub struct FloraState {
    pub display_handle: DisplayHandle,
    pub compositor_state: CompositorState,
    pub xdg_shell_state: XdgShellState,
    pub shm_state: ShmState,
    pub data_device_state: DataDeviceState,
    pub primary_selection_state: PrimarySelectionState,
    pub xdg_decoration_state: XdgDecorationState,
    pub text_input_manager_state: TextInputManagerState,
    pub seat_state: SeatState<Self>,
    pub seat: Seat<Self>,
    pub output: Option<Output>,
    pub should_stop: bool,
    pub drm_devices: Vec<PathBuf>,
    pub renderer: Option<GlowRenderer>,
    // Backend storage to keep them alive
    pub _gbm_device: Option<GbmDevice<DrmDeviceFd>>,
    pub _egl_display: Option<EGLDisplay>,
    // The compositor that handles rendering to a specific CRT/Connector
    pub compositor: Option<DrmCompositor<GbmAllocator<DrmDeviceFd>, GbmFramebufferExporter<DrmDeviceFd>, (), DrmDeviceFd>>,
    // Windows for rendering and interaction
    pub windows: Vec<Window>,
    pub pointer_location: Point<f64, Physical>,
    pub grab_state: Option<(usize, Point<f64, Physical>)>,
    pub input_context: Option<smithay::reexports::input::Libinput>,
    pub socket_name: std::ffi::OsString,
    pub needs_redraw: bool,
    pub _drm_device: Option<DrmDevice>,
}

use smithay::delegate_seat;
delegate_seat!(FloraState);

impl SeatHandler for FloraState {
    type KeyboardFocus = smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
    type PointerFocus = smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
    type TouchFocus = smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }

    fn focus_changed(&mut self, seat: &Seat<Self>, focused: Option<&Self::KeyboardFocus>) {
        info!("Focus: Changed to {:?}", focused);
        let serial = SERIAL_COUNTER.next_serial();
        if let Some(keyboard) = seat.get_keyboard() {
            keyboard.set_focus(self, focused.cloned(), serial);
        }

        // In Smithay 0.7.0, we must manually notify the TextInputHandle of focus changes
        let ti = seat.text_input();
        ti.set_focus(focused.cloned());
        if focused.is_some() {
            ti.enter();
        } else {
            ti.leave();
        }
    }
    fn cursor_image(&mut self, _seat: &Seat<Self>, _image: smithay::input::pointer::CursorImageStatus) {}
}

// Data device (clipboard) support
impl SelectionHandler for FloraState {
    type SelectionUserData = ();
}

impl ClientDndGrabHandler for FloraState {}
impl ServerDndGrabHandler for FloraState {}

impl DataDeviceHandler for FloraState {
    fn data_device_state(&self) -> &DataDeviceState {
        &self.data_device_state
    }
}

use smithay::delegate_data_device;
delegate_data_device!(FloraState);

// Output (monitor) global support
impl OutputHandler for FloraState {}

use smithay::delegate_output;
delegate_output!(FloraState);

// Primary selection (copy with middle mouse button) support
impl PrimarySelectionHandler for FloraState {
    fn primary_selection_state(&self) -> &PrimarySelectionState {
        &self.primary_selection_state
    }
}

use smithay::delegate_primary_selection;
delegate_primary_selection!(FloraState);

// XDG decoration (server-side window decorations) support
impl XdgDecorationHandler for FloraState {
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        use smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode;
        // Prefer client-side decorations by default
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(Mode::ClientSide);
        });
        toplevel.send_configure();
    }

    fn request_mode(&mut self, toplevel: ToplevelSurface, mode: smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode) {
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(mode);
        });
        toplevel.send_configure();
    }

    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        use smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode;
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(Mode::ClientSide);
        });
        toplevel.send_configure();
    }
}

use smithay::delegate_xdg_decoration;
delegate_xdg_decoration!(FloraState);

use smithay::delegate_text_input_manager;
delegate_text_input_manager!(FloraState);

pub struct FloraClientData {
    pub compositor_state: CompositorClientState,
}

impl ClientData for FloraClientData {}

impl FloraState {
    pub fn new(dh: &DisplayHandle) -> Self {
        // Take control of TTY if possible to prevent leakage and EBUSY
        unsafe {
            for path in ["/dev/tty0", "/dev/tty"] {
                if let Ok(file) = OpenOptions::new().read(true).write(true).open(path) {
                    let fd = file.as_raw_fd();
                    const KDSETMODE: libc::c_ulong = 0x4B3A;
                    const KD_GRAPHICS: libc::c_ulong = 0x01;
                    if libc::ioctl(fd, KDSETMODE, KD_GRAPHICS) == 0 {
                        info!("Flora: TTY switched to Graphics mode successfully on {}", path);
                        break;
                    } else {
                        warn!("Flora: Failed to set TTY graphics mode on {}: {}", path, std::io::Error::last_os_error());
                    }
                }
            }
        }

        let compositor_state = CompositorState::new::<Self>(dh);
        let xdg_shell_state = XdgShellState::new::<Self>(dh);
        let shm_state = ShmState::new::<Self>(dh, vec![]);
        let data_device_state = DataDeviceState::new::<Self>(dh);
        let primary_selection_state = PrimarySelectionState::new::<Self>(dh);
        let xdg_decoration_state = XdgDecorationState::new::<Self>(dh);
        let text_input_manager_state = TextInputManagerState::new::<Self>(dh);
        let mut seat_state = SeatState::new();
        let mut seat = seat_state.new_wl_seat(dh, "seat0");
        
        // Add initial capabilities
        seat.add_keyboard(Default::default(), 200, 25).ok();
        seat.add_pointer();
        
        // Create output and register as global for clients
        let output = Output::new(
            "Virtual-1".to_string(),
            PhysicalProperties {
                size: (500, 300).into(),
                subpixel: Subpixel::Unknown,
                make: "Flora".to_string(),
                model: "Virtual Display".to_string(),
            },
        );
        output.create_global::<Self>(dh);
        // Set initial mode with reasonable defaults (will be updated when DRM initializes)
        output.change_current_state(
            Some(OutputMode { size: (1280, 800).into(), refresh: 60000 }),
            Some(Transform::Normal),
            Some(smithay::output::Scale::Integer(1)),
            Some((0, 0).into()),
        );
        output.set_preferred(OutputMode { size: (1280, 800).into(), refresh: 60000 });

        Self {
            display_handle: dh.clone(),
            compositor_state,
            xdg_shell_state,
            shm_state,
            data_device_state,
            primary_selection_state,
            xdg_decoration_state,
            text_input_manager_state,
            seat_state,
            seat,
            output: Some(output),
            should_stop: false,
            drm_devices: Vec::new(),
            renderer: None,
            _gbm_device: None,
            _egl_display: None,
            compositor: None,
            windows: Vec::new(),
            pointer_location: Point::from((0.0, 0.0)),
            grab_state: None,
            input_context: None,
            socket_name: std::ffi::OsString::new(),
            needs_redraw: true,
            _drm_device: None,
        }
    }
}

// Basic Smithay trait implementation
impl CompositorHandler for FloraState {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client.get_data::<FloraClientData>().unwrap().compositor_state
    }

    // Callback when a client commits a new surface buffer
    fn commit(&mut self, surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface) {
        info!("Compositor: Commit received for surface {:?}", surface);
        // Register the buffer with Smithay's renderer infrastructure
        on_commit_buffer_handler::<Self>(surface);
        self.needs_redraw = true;
    }
}

// Delegate macro to connect FloraState with Smithay
smithay::delegate_compositor!(FloraState);

// XdgShell Handler - enables window creation
impl XdgShellHandler for FloraState {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        info!("XDG Shell: New toplevel surface created!");
        // Configure the toplevel with a reasonable default size
        surface.with_pending_state(|state| {
            state.size = Some((800, 600).into());
        });
        surface.send_configure();

        let wl_surface = surface.wl_surface().clone();
        self.windows.push(Window {
            toplevel: surface,
            location: (100, 100).into(),
        });

        // Set keyboard focus to the new window
        let serial = SERIAL_COUNTER.next_serial();
        if let Some(keyboard) = self.seat.get_keyboard() {
            keyboard.set_focus(self, Some(wl_surface.clone()), serial);
        }

        // Notify client about the output
        if let Some(output) = self.output.as_ref() {
            output.enter(&wl_surface);
        }

        self.needs_redraw = true;
    }
    fn new_popup(&mut self, surface: PopupSurface, _positioner: PositionerState) {
        surface.send_configure().ok();
    }

    fn grab(&mut self, _surface: PopupSurface, _seat: smithay::reexports::wayland_server::protocol::wl_seat::WlSeat, _serial: smithay::utils::Serial) {}
    fn reposition_request(&mut self, _surface: PopupSurface, _positioner: PositionerState, _token: u32) {}
}

smithay::delegate_xdg_shell!(FloraState);

// Shm Handler - enables shared memory buffers
impl ShmHandler for FloraState {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}

smithay::delegate_shm!(FloraState);

// Buffer Handler - notifies when buffers are destroyed
impl BufferHandler for FloraState {
    fn buffer_destroyed(&mut self, _buffer: &smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer) {}
}


struct FloraLibinputInterface;

impl smithay::reexports::input::LibinputInterface for FloraLibinputInterface {
    fn open_restricted(&mut self, path: &std::path::Path, flags: i32) -> Result<std::os::unix::io::OwnedFd, i32> {
        use std::os::unix::ffi::OsStrExt;
        let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        let fd = unsafe { libc::open(c_path.as_ptr(), flags) };
        if fd < 0 {
            let err = std::io::Error::last_os_error().raw_os_error().unwrap_or(libc::EIO);
            warn!("Libinput: Failed to open {:?}: {}", path, err);
            Err(err)
        } else {
            info!("Libinput: Successfully opened {:?} (fd: {}, flags: {:x})", path, fd, flags);
            Ok(unsafe { std::os::unix::io::OwnedFd::from_raw_fd(fd) })
        }
    }

    fn close_restricted(&mut self, _fd: std::os::unix::io::OwnedFd) {
        // OwnedFd closes itself when dropped, so no action needed here
    }
}


#[derive(Debug, Clone)]
enum FloraInputEvent {
    Keyboard {
        keycode: u32,
        pressed: bool,
        time: u32,
    },
    PointerMotion {
        delta: Point<f64, Physical>,
        time: u32,
    },
    PointerButton {
        button: u32,
        pressed: bool,
        time: u32,
    },
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().init();
    info!("Flora: Starting macOS-like Compositor...");
    
    // ... rest of main ...

    // ... (rest of the setup until Loop)

    // 1. Setup Event Loop
    let mut event_loop: EventLoop<FloraState> = EventLoop::try_new()?;
    let handle = event_loop.handle();
    
    // 2. Setup Wayland Display (wrapped for shared mutable access)
    use std::rc::Rc;
    use std::cell::RefCell;
    use std::os::unix::io::AsFd;
    
    // Create display first, extract poll_fd for event source, then wrap
    let mut display_raw = Display::new()?;
    let poll_fd = display_raw.backend().poll_fd().as_fd().try_clone_to_owned()
        .map_err(|e| anyhow::anyhow!("Failed to clone display poll_fd: {:?}", e))?;
    let dh = display_raw.handle();
    let display = Rc::new(RefCell::new(display_raw));
    
    // 3. Initialize State
    let mut state = FloraState::new(&dh);
    
    // 4. Setup Wayland Socket
    let source = ListeningSocketSource::new_auto()?;
    let socket_name = source.socket_name().to_os_string();
    let xdg_runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/run/user/1000".to_string());
    info!("Flora active! Socket Name: {:?}, XDG_RUNTIME_DIR: {}", socket_name, xdg_runtime);
    info!("Full socket path: {}/{}", xdg_runtime, socket_name.to_string_lossy());
    state.socket_name = socket_name.clone();

    handle.insert_source(source, |client_stream, _, state| {
        info!("Socket callback TRIGGERED! Client attempting to connect...");
        let client_data = FloraClientData {
            compositor_state: CompositorClientState::default(),
        };
        let mut dh = state.display_handle.clone();
        if let Err(e) = dh.insert_client(client_stream, Arc::new(client_data)) {
            warn!("Failed to insert client: {:?}", e);
        } else {
            info!("New client connected to Wayland socket!");
        }
        info!("Socket callback COMPLETED - returning from callback");
    }).map_err(|_e| anyhow::anyhow!("Failed to insert socket source"))?;

    // 4b. Setup Display Event Source
    // This source monitors the Wayland display's poll_fd and dispatches client requests
    // when they arrive. This is critical for responsiveness.
    use smithay::reexports::calloop::generic::Generic;
    use smithay::reexports::calloop::{Interest, Mode, PostAction};
    
    let display_clone = display.clone();
    handle.insert_source(
        Generic::new(poll_fd, Interest::READ, Mode::Level),
        move |_event, _metadata, state| {
            // Using borrow_mut because we are in the same thread and should be the only one dispatching
            let mut disp = display_clone.borrow_mut();
            if let Err(e) = disp.dispatch_clients(state) {
                error!("Display source: dispatch_clients error: {:?}", e);
            }
            let _ = disp.flush_clients();
            Ok(PostAction::Continue)
        }
    ).map_err(|_e| anyhow::anyhow!("Failed to insert display event source"))?;

    // 5. Initialize Udev Backend (to detect displays in VM)
    let udev = UdevBackend::new("seat0")?;
    
    // Scan for existing devices since Added events only trigger for new hotplugged devices
    for (_device_id, dev_path) in udev.device_list() {
        info!("Existing device detected: {:?}", dev_path);
        if dev_path.to_string_lossy().contains("card") || dev_path.to_string_lossy().contains("render") {
            state.drm_devices.push(dev_path.to_path_buf());
        }
    }

    handle.insert_source(udev, |event, _, state| {
        match event {
            UdevEvent::Added { device_id: _, path: dev_path } => {
                info!("New device detected: {:?}", dev_path);
                // Save if this is a DRM device (graphics card)
                if dev_path.to_string_lossy().contains("card") || dev_path.to_string_lossy().contains("render") {
                    state.drm_devices.push(dev_path);
                }
            },
            UdevEvent::Changed { device_id: _ } => info!("Device changed"),
            UdevEvent::Removed { device_id: _ } => info!("Device removed"),
        }
    }).map_err(|_e| anyhow::anyhow!("Failed to insert udev source"))?;

    // 6. Run Loop - dispatch clients manually each iteration
    info!("Flora Loop started. Initializing graphics first...");
    let mut input_initialized = false;

    // 6. Main Loop (Event-driven & Responsive)
    info!("Flora: Entering main event loop...");
    while !state.should_stop {
        // A. Graphics Initialization (if not done)
        if state.renderer.is_none() && !state.drm_devices.is_empty() {
            let device_path = state.drm_devices.pop().unwrap();
            info!("Attempting to initialize DRM on: {:?}", device_path);

            if let Ok(file) = OpenOptions::new().read(true).write(true).append(true).open(&device_path) {
                let fd = DrmDeviceFd::new(DeviceFd::from(std::os::unix::io::OwnedFd::from(file)));
                if let Ok((mut drm_device, _notifier)) = DrmDevice::new(fd.clone(), false) {
                    let gbm = GbmDevice::new(fd).expect("Gbm init failed");
                    let egl_display = unsafe { EGLDisplay::new(gbm.clone()) }.expect("EGL Display failed");
                    let egl_context = EGLContext::new(&egl_display).expect("EGL Context failed");
                    let renderer = unsafe { GlowRenderer::new(egl_context) }.expect("Glow init failed");

                    let res_handles = drm_device.resource_handles().expect("DRM handles failed");
                    let connector = res_handles.connectors().iter().find_map(|conn| {
                        let info = drm_device.get_connector(*conn, false).ok()?;
                        (info.state() == connector::State::Connected).then_some(*conn)
                    }).expect("No connector");

                    let conn_info = drm_device.get_connector(connector, false).expect("Conn info failed");
                    let mode = conn_info.modes()[0];
                    let crtc = conn_info.encoders().iter().find_map(|&enc| {
                        let info = drm_device.get_encoder(enc).ok()?;
                        res_handles.filter_crtcs(info.possible_crtcs()).iter().next().copied()
                    }).expect("No CRTC");

                    let surface = drm_device.create_surface(crtc, mode, &[connector]).expect("Surface failed");
                    let allocator = GbmAllocator::new(gbm.clone(), smithay::backend::allocator::gbm::GbmBufferFlags::RENDERING | smithay::backend::allocator::gbm::GbmBufferFlags::SCANOUT);
                    let exporter = GbmFramebufferExporter::new(gbm.clone(), None);

                    let (w, h) = mode.size();
                    let output_mode_source = OutputModeSource::Static {
                        size: Size::from((w as i32, h as i32)),
                        scale: Scale::from(1.0),
                        transform: Transform::Normal,
                    };

                    let compositor = DrmCompositor::new(
                        output_mode_source, surface, None, allocator, exporter,
                        vec![Fourcc::Xrgb8888, Fourcc::Argb8888], renderer.dmabuf_formats(),
                        Size::from((64, 64)), Some(gbm.clone()),
                    ).expect("Compositor failed");

                    use smithay::backend::drm::DrmEvent;
                    handle.insert_source(_notifier, |event, _metadata, state| {
                        match event {
                            DrmEvent::VBlank(_crtc) => {
                                state.needs_redraw = true;
                            }
                            DrmEvent::Error(err) => error!("DRM Event Error: {:?}", err),
                        }
                    }).expect("Failed to insert DRM notifier");

                    state._egl_display = Some(egl_display);
                    state._gbm_device = Some(gbm);
                    state.renderer = Some(renderer);
                    state.compositor = Some(compositor);
                    state._drm_device = Some(drm_device);
                    state.needs_redraw = true;
                    info!("Screen output initialized successfully!");
                }
            }
        }

        // B. Input Initialization (Once graphics is ready)
        if !input_initialized && state.renderer.is_some() {
            let (input_sender, input_receiver) = smithay::reexports::calloop::channel::channel::<FloraInputEvent>();
            
            // Zero-Latency Input: Insert receiver as a calloop source
            handle.insert_source(input_receiver, |event, _, state| {
                if let smithay::reexports::calloop::channel::Event::Msg(input_event) = event {
                    info!("Main Loop: Input event triggered from channel");
                    match input_event {
                        FloraInputEvent::Keyboard { keycode, pressed, time } => {
                            info!("🎹 Keyboard event received! Key={} Pressed={}", keycode, pressed);
                            let serial = SERIAL_COUNTER.next_serial();
                            let state_enum = if pressed { smithay::backend::input::KeyState::Pressed } else { smithay::backend::input::KeyState::Released };
                            if let Some(keyboard) = state.seat.get_keyboard() {
                                // Add +8 offset for evdev -> XKB keycode mapping
                                keyboard.input::<(), _>(state, (keycode + 8).into(), state_enum, serial, time, |_, _, keysym| {
                                    info!("🎹 Keysym: {:?}", keysym);
                                    FilterResult::Forward
                                });
                            }
                        }
                        FloraInputEvent::PointerMotion { delta, time } => {
                            state.pointer_location += delta;
                            state.pointer_location.x = state.pointer_location.x.max(0.0).min(1280.0);
                            state.pointer_location.y = state.pointer_location.y.max(0.0).min(800.0);
                            
                            if let Some((idx, offset)) = state.grab_state {
                                if let Some(window) = state.windows.get_mut(idx) {
                                    window.location = Point::<i32, Physical>::from((
                                        (state.pointer_location.x - offset.x).round() as i32,
                                        (state.pointer_location.y - offset.y).round() as i32
                                    ));
                                }
                            }

                            let serial = SERIAL_COUNTER.next_serial();
                            if let Some(pointer) = state.seat.get_pointer() {
                                let under = state.windows.iter().rev().find_map(|w| {
                                    let px = state.pointer_location.x.round() as i32;
                                    let py = state.pointer_location.y.round() as i32;
                                    let local_x = px - w.location.x;
                                    let local_y = py - w.location.y;
                                    // Very simple hit test: 800x600 window size
                                    if local_x >= 0 && local_x <= 800 && local_y >= 0 && local_y <= 600 {
                                        Some((w.toplevel.wl_surface().clone(), Point::<f64, smithay::utils::Logical>::from((local_x as f64, local_y as f64))))
                                    } else {
                                        None
                                    }
                                });

                                pointer.motion(state, under, &smithay::input::pointer::MotionEvent {
                                    location: state.pointer_location.to_logical(1.0),
                                    serial, time,
                                });
                            }
                            state.needs_redraw = true;
                        }
                        FloraInputEvent::PointerButton { button, pressed, time } => {
                            let serial = SERIAL_COUNTER.next_serial();
                            let state_enum = if pressed { smithay::backend::input::ButtonState::Pressed } else { smithay::backend::input::ButtonState::Released };
                            
                            if pressed {
                                // Hit test for dragging and focus
                                let hit = state.windows.iter().enumerate().rev().find_map(|(i, w)| {
                                    let px = state.pointer_location.x.round() as i32;
                                    let py = state.pointer_location.y.round() as i32;
                                    let local_x = px - w.location.x;
                                    let local_y = py - w.location.y;
                                    if local_x >= 0 && local_x <= 800 && local_y >= 0 && local_y <= 600 {
                                        let w_loc_f = Point::<f64, Physical>::from((w.location.x as f64, w.location.y as f64));
                                        Some((i, state.pointer_location - w_loc_f))
                                    } else {
                                        None
                                    }
                                });

                                if let Some((idx, offset)) = hit {
                                    let surface = state.windows[idx].toplevel.wl_surface().clone();
                                    if let Some(keyboard) = state.seat.get_keyboard() {
                                        keyboard.set_focus(state, Some(surface), serial);
                                    }
                                    // Move to front
                                    let win = state.windows.remove(idx);
                                    state.windows.push(win);
                                    state.grab_state = Some((state.windows.len() - 1, offset));
                                }
                            } else {
                                state.grab_state = None;
                            }

                            if let Some(pointer) = state.seat.get_pointer() {
                                pointer.button(state, &smithay::input::pointer::ButtonEvent {
                                    button, state: state_enum, serial, time,
                                });
                            }
                            state.needs_redraw = true;
                        }
                    }
                }
            }).expect("Failed to insert input source");

            // Background Thread
            std::thread::spawn(move || {
                let mut libinput = smithay::reexports::input::Libinput::new_from_path(FloraLibinputInterface);
                info!("Input Thread: Started and scanning /dev/input/event* for unique devices...");
                if let Ok(entries) = std::fs::read_dir("/dev/input/") {
                    for entry in entries.flatten() {
                        let name = entry.file_name().to_string_lossy().into_owned();
                        if name.starts_with("event") {
                            let path = format!("/dev/input/{}", name);
                            info!("Input Thread: Adding device: {}", path);
                            libinput.path_add_device(&path);
                        }
                    }
                }
                loop {
                    if let Err(e) = libinput.dispatch() {
                        error!("Input Thread: Libinput dispatch error: {:?}", e);
                    }
                    for event in &mut libinput {
                        // Catch-all log to see if ANY data reaches us
                        info!("Input Thread: EVENT RECEIVED: {:?}", event);
                        
                        match event {
                            LibinputEvent::Device(DeviceEvent::Added(d)) => {
                                info!("Input Thread: Device Added: {:?}", d.device().name());
                            }
                            LibinputEvent::Keyboard(kb) => {
                                info!("Input Thread: Raw Keyboard: key={} state={:?}", kb.key(), kb.key_state());
                                let _ = input_sender.send(FloraInputEvent::Keyboard { keycode: kb.key(), pressed: kb.key_state() == KeyState::Pressed, time: kb.time() as u32 });
                            }
                            LibinputEvent::Pointer(ptr) => {
                                match ptr {
                                    PointerEvent::Motion(m) => {
                                        info!("Input Thread: Raw Motion: dx={} dy={}", m.dx(), m.dy());
                                        let _ = input_sender.send(FloraInputEvent::PointerMotion { delta: (m.dx(), m.dy()).into(), time: m.time() as u32 });
                                    }
                                    PointerEvent::Button(b) => {
                                        info!("Input Thread: Raw Button: button={} state={:?}", b.button(), b.button_state());
                                        let _ = input_sender.send(FloraInputEvent::PointerButton { button: b.button(), pressed: b.button_state() == ButtonState::Pressed, time: b.time() as u32 });
                                    }
                                    _ => {}
                                }
                            }
                            _ => {}
                        }
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
            });
            input_initialized = true;
        }

        // C. Dispatch Events
        // Use a 16ms timeout for roughly 60Hz loop if no events arrive
        if let Err(e) = event_loop.dispatch(Duration::from_millis(16), &mut state) {
            error!("Event loop error: {:?}", e);
            break;
        }

        // D. Dirty Rendering
        if state.needs_redraw {
            if let Some(compositor) = state.compositor.as_mut() {
                if let Some(renderer) = state.renderer.as_mut() {
                    let color = [0.2, 0.2, 0.2, 1.0];
                    let mut elements: Vec<WaylandSurfaceRenderElement<GlowRenderer>> = Vec::new();
                    for window in &state.windows {
                        elements.extend(render_elements_from_surface_tree(renderer, window.toplevel.wl_surface(), window.location, 1.0, 1.0, Kind::Unspecified));
                    }
                    
                    // Render the frame
                    if let Err(e) = compositor.render_frame::<GlowRenderer, WaylandSurfaceRenderElement<GlowRenderer>>(renderer, &elements, color, smithay::backend::drm::compositor::FrameFlags::empty()) {
                        error!("Rendering: render_frame failed: {:?}", e);
                    }
                    
                    // IMPORTANT: Process frame callbacks BEFORE commit_frame or right after to ensure clients know we finished a frame
                    let time = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u32;
                    for window in &state.windows {
                        with_surface_tree_downward(window.toplevel.wl_surface(), (), |_, _, _| TraversalAction::DoChildren(()), |_, states, _| {
                            let mut guard = states.cached_state.get::<SurfaceAttributes>();
                            for callback in guard.current().frame_callbacks.drain(..) { callback.done(time); }
                        }, |_, _, _| true);
                    }

                    // Commit to GPU
                    if let Err(e) = compositor.commit_frame() {
                        error!("Rendering: commit_frame failed: {:?}", e);
                    }
                }
            }
            state.needs_redraw = false;
        }

        // E. Flush Wayland Display happens in the event source callback
    }

    Ok(())
}
