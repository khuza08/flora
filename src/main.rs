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
        calloop::{EventLoop, Interest, Mode, PostAction},
        wayland_server::{Display, DisplayHandle, Client, backend::ClientData},
        drm::control::{connector, Device as _},
        input as smithay_input,
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
    },
    output::{Output, PhysicalProperties, Subpixel, Mode as OutputMode},
    input::{SeatState, SeatHandler, Seat},
    backend::input::{
        InputEvent, Event,
        KeyboardKeyEvent, PointerMotionEvent, PointerButtonEvent,
    },
    backend::libinput::LibinputInputBackend,
    input::keyboard::FilterResult,
    utils::SERIAL_COUNTER,
};
use tracing::{info, warn, error};
use std::{time::Duration, sync::Arc, path::PathBuf, fs::OpenOptions};

pub struct FloraState {
    pub display_handle: DisplayHandle,
    pub compositor_state: CompositorState,
    pub xdg_shell_state: XdgShellState,
    pub shm_state: ShmState,
    pub data_device_state: DataDeviceState,
    pub primary_selection_state: PrimarySelectionState,
    pub xdg_decoration_state: XdgDecorationState,
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
    // Toplevel surfaces for rendering
    pub toplevels: Vec<ToplevelSurface>,
    pub pointer_location: Point<f64, Physical>,
    pub input_context: Option<smithay::reexports::input::Libinput>,
    pub socket_name: std::ffi::OsString,
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

    fn focus_changed(&mut self, _seat: &Seat<Self>, _focused: Option<&Self::KeyboardFocus>) {}
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

pub struct FloraClientData {
    pub compositor_state: CompositorClientState,
}

impl ClientData for FloraClientData {}

impl FloraState {
    pub fn new(dh: &DisplayHandle) -> Self {
        let compositor_state = CompositorState::new::<Self>(dh);
        let xdg_shell_state = XdgShellState::new::<Self>(dh);
        let shm_state = ShmState::new::<Self>(dh, vec![]);
        let data_device_state = DataDeviceState::new::<Self>(dh);
        let primary_selection_state = PrimarySelectionState::new::<Self>(dh);
        let xdg_decoration_state = XdgDecorationState::new::<Self>(dh);
        let mut seat_state = SeatState::new();
        // Use new_wl_seat to register wl_seat as a global for clients to bind
        let seat = seat_state.new_wl_seat(dh, "seat0");
        
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
            seat_state,
            seat,
            output: Some(output),
            should_stop: false,
            drm_devices: Vec::new(),
            renderer: None,
            _gbm_device: None,
            _egl_display: None,
            compositor: None,
            toplevels: Vec::new(),
            pointer_location: Point::from((0.0, 0.0)),
            input_context: None,
            socket_name: std::ffi::OsString::new(),
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
        info!("XdgShell: New toplevel surface created (Client connected)!");
        // Configure the toplevel with a reasonable default size
        surface.with_pending_state(|state| {
            state.size = Some((800, 600).into());
        });
        surface.send_configure();
        
        let wl_surface = surface.wl_surface().clone();
        self.toplevels.push(surface);
        
        // New: Set keyboard focus to the new window
        let serial = SERIAL_COUNTER.next_serial();
        if let Some(keyboard) = self.seat.get_keyboard() {
            keyboard.set_focus(self, Some(wl_surface), serial);
        }
        
        info!("XdgShell: New toplevel surface created and focused.");
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

impl smithay_input::LibinputInterface for FloraLibinputInterface {
    fn open_restricted(&mut self, path: &std::path::Path, _flags: i32) -> Result<std::os::unix::io::OwnedFd, i32> {
        info!("Libinput: Opening {:?}", path);
        let result = OpenOptions::new()
            .read(true)
            .write(true) // Re-enable write access
            .open(path);
        
        match result {
            Ok(file) => {
                info!("Libinput: Successfully opened {:?}", path);
                Ok(file.into())
            }
            Err(err) => {
                warn!("Libinput: Failed to open {:?}: {:?}", path, err);
                Err(err.raw_os_error().unwrap_or(1))
            }
        }
    }

    fn close_restricted(&mut self, fd: std::os::unix::io::OwnedFd) {
        drop(fd);
    }
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().init();
    info!("Flora: Starting macOS-like Compositor...");

    // 1. Setup Event Loop
    let mut event_loop: EventLoop<FloraState> = EventLoop::try_new()?;
    let handle = event_loop.handle();
    
    // 2. Setup Wayland Display (wrapped for shared mutable access)
    use std::rc::Rc;
    use std::cell::RefCell;
    
    let display = Rc::new(RefCell::new(Display::new()?));
    let dh = display.borrow().handle();
    
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
    }).map_err(|_e| anyhow::anyhow!("Failed to insert socket source"))?;

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

    while !state.should_stop {
        if state.renderer.is_none() && !state.drm_devices.is_empty() {
            let device_path = state.drm_devices.pop().unwrap();
            info!("Attempting to initialize DRM on: {:?}", device_path);

            // Open the DRM device
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .append(true)
                .open(&device_path)?;
            
            let fd = DrmDeviceFd::new(DeviceFd::from(std::os::unix::io::OwnedFd::from(file)));
            
            // Initialize DRM Device
            // Use atomic = false (Legacy DRM) for better compatibility in VM environments
            let (mut drm_device, _notifier) = DrmDevice::new(fd.clone(), false)
                .map_err(|e| anyhow::anyhow!("Failed to initialize DRM device: {}", e))?;
            
            // Initialize GBM Device
            let gbm = GbmDevice::new(fd)
                .map_err(|e| anyhow::anyhow!("Failed to initialize GBM device: {}", e))?;
            
            // Initialize EGL and Renderer
            let egl_display = unsafe { EGLDisplay::new(gbm.clone()) }
                .map_err(|e| anyhow::anyhow!("Failed to create EGL display: {}", e))?;
            let egl_context = EGLContext::new(&egl_display)
                .map_err(|e| anyhow::anyhow!("Failed to create EGL context: {}", e))?;
            
            let renderer = unsafe { GlowRenderer::new(egl_context) }
                .map_err(|e| anyhow::anyhow!("Failed to initialize Glow renderer: {}", e))?;
            
            info!("DRM and Renderer initialized successfully on {:?}", device_path);
            
            // --- New: Display Enumeration and Compositor Setup ---
            
            // 1. Get resources (connectors, crtcs, etc.)
            let res_handles = drm_device.resource_handles()
                .map_err(|e| anyhow::anyhow!("Failed to get DRM resource handles: {}", e))?;
            
            // 2. Find a connected connector
            let connector = res_handles.connectors().iter()
                .find_map(|conn_handle| {
                    let info = drm_device.get_connector(*conn_handle, false).ok()?;
                    if info.state() == connector::State::Connected {
                        Some(*conn_handle)
                    } else {
                        None
                    }
                })
                .ok_or_else(|| anyhow::anyhow!("No connected display found!"))?;
            
            let conn_info = drm_device.get_connector(connector, false)?;
            info!("Found connected display: {:?}", conn_info.interface());

            // 3. Get the native mode (resolution)
            let mode = conn_info.modes()[0];
            info!("Using mode: {:?}", mode);

            // 4. Find a CRT for this connector
            let crtc = conn_info.encoders().iter()
                .find_map(|&encoder_handle| {
                    let encoder_info = drm_device.get_encoder(encoder_handle).ok()?;
                    res_handles.filter_crtcs(encoder_info.possible_crtcs()).iter().next().copied()
                })
                .ok_or_else(|| anyhow::anyhow!("No suitable CRTC found for connector!"))?;
            
            info!("Using CRTC: {:?}", crtc);

            // 5. Create DrmSurface
            let surface = drm_device.create_surface(crtc, mode, &[connector])
                .map_err(|e| anyhow::anyhow!("Failed to create DrmSurface: {}", e))?;

            // 6. Create Allocator and Exporter
            let allocator = GbmAllocator::new(gbm.clone(), smithay::backend::allocator::gbm::GbmBufferFlags::RENDERING | smithay::backend::allocator::gbm::GbmBufferFlags::SCANOUT);
            let exporter = GbmFramebufferExporter::new(gbm.clone(), None);

            // 7. Prepare DrmCompositor Arguments
            let (w, h) = mode.size();
            let output_mode_source = OutputModeSource::Static {
                size: Size::from((w as i32, h as i32)),
                scale: Scale::from(1.0),
                transform: Transform::Normal,
            };
            
            let renderer_formats = renderer.dmabuf_formats();

            // 8. Create DrmCompositor (9 arguments for Smithay 0.7.0)
            // Multiple formats for virtual GPU compatibility
            let color_formats = vec![
                Fourcc::Xrgb8888,  // Most compatible - no alpha
                Fourcc::Argb8888,  // ARGB with alpha
                Fourcc::Xbgr8888,  // BGR variant
                Fourcc::Abgr8888,  // ABGR variant
            ];
            
            let compositor = DrmCompositor::new(
                output_mode_source,
                surface,
                None, // planes
                allocator,
                exporter,
                color_formats,
                renderer_formats,
                Size::from((64, 64)), // cursor_size
                Some(gbm.clone()), // gbm
            ).map_err(|e| anyhow::anyhow!("Failed to create DrmCompositor: {}", e))?;

            // Store EVERYTHING to keep it alive
            state._egl_display = Some(egl_display);
            state._gbm_device = Some(gbm);
            state.renderer = Some(renderer);
            state.compositor = Some(compositor);
            
            info!("Screen output initialized successfully!");
        }

        // --- Rendering Loop ---
        if let Some(compositor) = state.compositor.as_mut() {
            if let Some(renderer) = state.renderer.as_mut() {
               // Render a macOS-like gray background
               let color = [0.2, 0.2, 0.2, 1.0]; // #333333ish
               
               // Collect render elements from all toplevel surfaces
               let mut elements: Vec<WaylandSurfaceRenderElement<GlowRenderer>> = Vec::new();
               
               // Debug: log number of toplevels
               if !state.toplevels.is_empty() {
                   info!("Rendering {} toplevels", state.toplevels.len());
               }
               
               for toplevel in &state.toplevels {
                   let surface = toplevel.wl_surface();
                   let location: Point<i32, Physical> = (100, 100).into();
                   let surface_elements: Vec<WaylandSurfaceRenderElement<GlowRenderer>> = 
                       render_elements_from_surface_tree(
                           renderer,
                           surface,
                           location,
                           1.0, // scale
                           1.0, // alpha
                           Kind::Unspecified,
                       );
                   info!("Surface generated {} render elements", surface_elements.len());
                   elements.extend(surface_elements);
               }
               
               if !elements.is_empty() {
                   info!("Total render elements: {}", elements.len());
               }
               
               // Use DrmCompositor to render a frame with surface elements
                let _ = compositor.render_frame::<GlowRenderer, WaylandSurfaceRenderElement<GlowRenderer>>(
                    renderer,
                    &elements,
                    color,
                    smithay::backend::drm::compositor::FrameFlags::empty(),
                );
                let _ = compositor.commit_frame();
                
                // Send frame callbacks to notify clients we're done rendering
                let time = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_millis() as u32;
                    
                for toplevel in &state.toplevels {
                    let surface = toplevel.wl_surface();
                    with_surface_tree_downward(
                        surface,
                        (),
                        |_, _, _| TraversalAction::DoChildren(()),
                        |_, states, _| {
                            let mut guard = states.cached_state.get::<SurfaceAttributes>();
                            let attrs = guard.current();
                            for callback in attrs.frame_callbacks.drain(..) {
                                callback.done(time);
                            }
                        },
                        |_, _, _| true,
                    );
                }
            }
        }

        if !input_initialized && state.renderer.is_some() {
            info!("Graphics ready, initializing input...");
            
            // 6. Initialize Input Protocols
            state.seat.add_keyboard(Default::default(), 200, 25).ok();
            state.seat.add_pointer();

            info!("Input: Initializing Libinput context (Path-based)...");
            let mut libinput_context = smithay::reexports::input::Libinput::new_from_path(FloraLibinputInterface);
            
            // Avoid adding the same event node multiple times via symlinks
            let mut added_nodes = std::collections::HashSet::new();

            info!("Input: Scanning /dev/input/by-path/...");
            if let Ok(entries) = std::fs::read_dir("/dev/input/by-path/") {
                for entry in entries.flatten() {
                    let path = entry.path();
                    let path_str = path.to_string_lossy();
                    
                    // Skip legacy/ACPI that cause hangs
                    if path_str.contains("i8042") || path_str.contains("acpi") {
                        continue;
                    }

                    if path_str.contains("-event-kbd") || path_str.contains("-event-mouse") {
                        // Resolve symlink to see the real eventX node
                        if let Ok(real_path) = std::fs::canonicalize(&path) {
                            if added_nodes.contains(&real_path) {
                                continue;
                            }
                            
                            // Temporary skip for event3 which takes 10 seconds in this VM
                            let real_path_str = real_path.to_string_lossy();
                            if real_path_str.contains("event3") {
                                info!("Input: Skipping SLOW device {:?} (Target: {:?})", path_str, real_path);
                                continue;
                            }

                            info!("Input: Registering device {:?} (Target: {:?})", path_str, real_path);
                            libinput_context.path_add_device(&path_str);
                            added_nodes.insert(real_path);
                            info!("Input: path_add_device returned.");
                        }
                    }
                }
            }
            
            let libinput_backend = LibinputInputBackend::new(libinput_context);
            info!("Input: Libinput backend created. Inserting source...");
            handle.insert_source(libinput_backend, |event, _, state| {
                match event {
                    InputEvent::Keyboard { event } => {
                        let keycode = event.key_code();
                        let key_state = event.state();
                        let serial = SERIAL_COUNTER.next_serial();
                        let time = event.time() as u32;
                        if let Some(keyboard) = state.seat.get_keyboard() {
                            keyboard.input::<(), _>(state, keycode, key_state, serial, time, |_, _, _| FilterResult::Forward);
                        }
                    }
                    InputEvent::PointerMotion { event } => {
                        state.pointer_location += event.delta().to_physical(1.0);
                        state.pointer_location.x = state.pointer_location.x.max(0.0).min(1280.0);
                        state.pointer_location.y = state.pointer_location.y.max(0.0).min(800.0);
                        if let Some(pointer) = state.seat.get_pointer() {
                            use smithay::input::pointer::MotionEvent;
                            pointer.motion(state, None, &MotionEvent {
                                location: state.pointer_location.to_logical(1.0),
                                serial: SERIAL_COUNTER.next_serial(),
                                time: event.time() as u32,
                            });
                        }
                    }
                    InputEvent::PointerButton { event } => {
                        if let Some(pointer) = state.seat.get_pointer() {
                            use smithay::input::pointer::ButtonEvent;
                            pointer.button(state, &ButtonEvent {
                                button: event.button_code(),
                                state: event.state(),
                                serial: SERIAL_COUNTER.next_serial(),
                                time: event.time() as u32,
                            });
                        }
                    }
                    _ => {}
                }
            }).ok();
            
            input_initialized = true;
            info!("Input initialization fully finished.");

            // DISABLED: Auto-spawn foot for testing - use manual foot for debugging
            info!("Flora: Ready for clients! Connect with: WAYLAND_DISPLAY={:?} foot", state.socket_name);
            // use std::process::Command;
            // match Command::new("foot")
            //     .env("WAYLAND_DISPLAY", &state.socket_name)
            //     .env("XDG_RUNTIME_DIR", "/run/user/1000")
            //     .spawn() {
            //     Ok(_) => info!("Flora: foot spawned successfully."),
            //     Err(e) => warn!("Flora: Failed to spawn foot: {:?}", e),
            // }
        }

        // First: Accept new connections and process input events
        event_loop.dispatch(Duration::from_millis(16), &mut state)?;
        
        // Second: Dispatch Wayland protocol messages from connected clients
        match display.try_borrow_mut() {
            Ok(mut disp) => {
                if let Err(e) = disp.dispatch_clients(&mut state) {
                    error!("Flora: dispatch_clients error: {:?}", e);
                }
                if let Err(e) = disp.flush_clients() {
                    error!("Flora: flush_clients error: {:?}", e);
                }
            },
            Err(e) => {
                warn!("Flora: Could not borrow display: {:?}", e);
            }
        }
    }

    Ok(())
}
