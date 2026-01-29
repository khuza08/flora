use smithay::{
    backend::{
        udev::{UdevBackend, UdevEvent},
        drm::{DrmDevice, DrmDeviceFd, compositor::DrmCompositor},
        allocator::gbm::{GbmDevice, GbmAllocator},
        drm::exporter::gbm::GbmFramebufferExporter,
        egl::{EGLDisplay, EGLContext},
        renderer::glow::GlowRenderer,
    },
    reexports::{
        calloop::{EventLoop, Interest, Mode, PostAction},
        wayland_server::{Display, DisplayHandle, Client, backend::ClientData},
        drm::control::{connector, Device as _},
    },
    utils::DeviceFd,
    wayland::{
        compositor::{CompositorState, CompositorHandler, CompositorClientState},
        socket::ListeningSocketSource,
    },
};
use tracing::info;
use std::{time::Duration, sync::Arc, path::PathBuf, fs::OpenOptions};

pub struct FloraState {
    pub display_handle: DisplayHandle,
    pub compositor_state: CompositorState,
    pub should_stop: bool,
    pub drm_devices: Vec<PathBuf>,
    pub renderer: Option<GlowRenderer>,
    // Backend storage to keep them alive
    pub _gbm_device: Option<GbmDevice<DrmDeviceFd>>,
    pub _egl_display: Option<EGLDisplay>,
    // The compositor that handles rendering to a specific CRT/Connector
    pub compositor: Option<DrmCompositor<GbmAllocator<DrmDeviceFd>, GbmFramebufferExporter<DrmDeviceFd>, (), DrmDeviceFd>>,
}

pub struct FloraClientData {
    pub compositor_state: CompositorClientState,
}

impl ClientData for FloraClientData {}

impl FloraState {
    pub fn new(dh: &DisplayHandle) -> Self {
        let compositor_state = CompositorState::new::<Self>(dh);

        Self {
            display_handle: dh.clone(),
            compositor_state,
            should_stop: false,
            drm_devices: Vec::new(),
            renderer: None,
            _gbm_device: None,
            _egl_display: None,
            compositor: None,
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

    // Callback when a client commits a new surface
    fn commit(&mut self, _surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface) {}
}

// Delegate macro to connect FloraState with Smithay
smithay::delegate_compositor!(FloraState);

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

    // Insert Wayland Display into event loop
    handle.insert_source(
        smithay::reexports::calloop::generic::Generic::new(display, Interest::READ, Mode::Level),
        |_, display, state| {
            unsafe {
                display.get_mut().dispatch_clients(state).map(|_| PostAction::Continue)
            }
        },
    ).map_err(|_e| anyhow::anyhow!("Failed to insert display source"))?;

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
            let (drm_device, _notifier) = DrmDevice::new(fd.clone(), false)
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
            let crtc = res_handles.filter_crtcs(conn_info.possible_encoders().iter().flat_map(|e| {
                drm_device.get_encoder(*e).ok().map(|ei| ei.possible_crtcs())
            }).fold(0, |acc, x| acc | x)).iter().next()
                .ok_or_else(|| anyhow::anyhow!("No suitable CRTC found for connector!"))?;
            
            info!("Using CRTC: {:?}", crtc);

            // 5. Create Allocator and Exporter
            let allocator = GbmAllocator::new(gbm.clone(), smithay::backend::allocator::gbm::GbmBufferFlags::RENDERING | smithay::backend::allocator::gbm::GbmBufferFlags::SCANOUT);
            let exporter = GbmFramebufferExporter::new(gbm.clone(), drm_device.node_id());

            // 6. Create DrmCompositor
            let compositor = DrmCompositor::new(
                &dh,
                drm_device,
                allocator,
                exporter,
                *crtc,
                mode,
                connector,
                None,
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
               
               // Use DrmCompositor to render a frame
               let _ = compositor.render_frame(renderer, &[], color, smithay::backend::drm::compositor::FrameFlags::empty());
               let _ = compositor.commit_frame();
            }
        }

        event_loop.dispatch(Duration::from_millis(16), &mut state)?;
    }

    Ok(())
}
