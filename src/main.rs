use smithay::{
    backend::{
        udev::{UdevBackend, UdevEvent},
        drm::{DrmDevice, DrmDeviceFd, compositor::DrmCompositor},
        allocator::{gbm::{GbmDevice, GbmAllocator}, Fourcc},
        drm::exporter::gbm::GbmFramebufferExporter,
        egl::{EGLDisplay, EGLContext},
        renderer::{
            glow::GlowRenderer,
            ImportDma, ImportAll,
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
        compositor::{CompositorState, CompositorHandler, CompositorClientState},
        socket::ListeningSocketSource,
        shell::xdg::{XdgShellState, XdgShellHandler, ToplevelSurface, PopupSurface, PositionerState},
        shm::{ShmState, ShmHandler},
        buffer::BufferHandler,
    },
    input::{SeatState, SeatHandler, Seat},
    backend::input::{
        InputEvent, Event,
        KeyboardKeyEvent, PointerMotionEvent, PointerButtonEvent, PointerAxisEvent,
    },
    backend::libinput::LibinputInputBackend,
};
use tracing::info;
use std::{time::Duration, sync::Arc, path::PathBuf, fs::OpenOptions};

pub struct FloraState {
    pub display_handle: DisplayHandle,
    pub compositor_state: CompositorState,
    pub xdg_shell_state: XdgShellState,
    pub shm_state: ShmState,
    pub seat_state: SeatState<Self>,
    pub seat: Seat<Self>,
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

pub struct FloraClientData {
    pub compositor_state: CompositorClientState,
}

impl ClientData for FloraClientData {}

impl FloraState {
    pub fn new(dh: &DisplayHandle) -> Self {
        let compositor_state = CompositorState::new::<Self>(dh);
        let xdg_shell_state = XdgShellState::new::<Self>(dh);
        let shm_state = ShmState::new::<Self>(dh, vec![]);
        let mut seat_state = SeatState::new();
        let seat = seat_state.new_seat("seat0");

        Self {
            display_handle: dh.clone(),
            compositor_state,
            xdg_shell_state,
            shm_state,
            seat_state,
            seat,
            should_stop: false,
            drm_devices: Vec::new(),
            renderer: None,
            _gbm_device: None,
            _egl_display: None,
            compositor: None,
            toplevels: Vec::new(),
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
        // Configure the toplevel with a reasonable default size
        surface.with_pending_state(|state| {
            state.size = Some((800, 600).into());
        });
        surface.send_configure();
        self.toplevels.push(surface);
        info!("New toplevel surface created");
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
        OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map(|f| f.into())
            .map_err(|err| err.raw_os_error().unwrap_or(1))
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
    
    // 2. Setup Wayland Display
    let display = Display::new()?;
    let dh = display.handle();
    
    // 3. Initialize State
    let mut state = FloraState::new(&dh);
    
    // 4. Setup Wayland Socket
    let source = ListeningSocketSource::new_auto()?;
    let socket_name = source.socket_name().to_os_string();
    info!("Flora active! Socket Name: {:?}", socket_name);

    handle.insert_source(source, |client_stream, _, state| {
        let client_data = FloraClientData {
            compositor_state: CompositorClientState::default(),
        };
        let _ = state.display_handle.insert_client(client_stream, Arc::new(client_data));
        info!("New client connected!");
    }).map_err(|_e| anyhow::anyhow!("Failed to insert socket source"))?;

    // 5. Initialize Udev Backend (to detect displays in VM)
    // Use "seat0" as it is the standard on Arch Linux
    let udev = UdevBackend::new("seat0")?;
    
    // Scan for existing devices since Added events only trigger for new hotplugged devices
    for (_device_id, path) in udev.device_list() {
        info!("Existing device detected: {:?}", path);
        if path.to_string_lossy().contains("card") || path.to_string_lossy().contains("render") {
            state.drm_devices.push(path.to_path_buf());
        }
    }

    handle.insert_source(udev, |event, _, state| {
        match event {
            UdevEvent::Added { device_id: _, path } => {
                info!("New device detected: {:?}", path);
                // Save if this is a DRM device (graphics card)
                if path.to_string_lossy().contains("card") || path.to_string_lossy().contains("render") {
                    state.drm_devices.push(path);
                }
            },
            UdevEvent::Changed { device_id: _ } => info!("Device changed"),
            UdevEvent::Removed { device_id: _ } => info!("Device removed"),
        }
    }).map_err(|_e| anyhow::anyhow!("Failed to insert udev source"))?;

    handle.insert_source(
        smithay::reexports::calloop::generic::Generic::new(display, Interest::READ, Mode::Level),
        |_, display, state| {
            unsafe {
                display.get_mut().dispatch_clients(state).map(|_| PostAction::Continue)
            }
        },
    ).map_err(|_e| anyhow::anyhow!("Failed to insert display source"))?;

    // 6. Initialize Libinput (DISABLED - blocks on QEMU without input devices)
    // TODO: Re-enable when running on real hardware or with input device passthrough
    state.seat.add_keyboard(Default::default(), 200, 25)
        .map_err(|_| anyhow::anyhow!("Failed to add keyboard to seat"))?;
    state.seat.add_pointer();
    info!("Input devices registered (libinput disabled for QEMU compatibility)");


    // 6. Run Loop
    info!("Flora Loop started. Waiting for graphics hardware...");
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
                   elements.extend(surface_elements);
               }
               
                // Use DrmCompositor to render a frame with surface elements
                let _ = compositor.render_frame::<GlowRenderer, WaylandSurfaceRenderElement<GlowRenderer>>(
                    renderer,
                    &elements,
                    color,
                    smithay::backend::drm::compositor::FrameFlags::empty(),
                );
                let _ = compositor.commit_frame();
            }
        }

        event_loop.dispatch(Duration::from_millis(16), &mut state)?;
    }

    Ok(())
}
