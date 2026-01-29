use smithay::{
    backend::udev::{UdevBackend, UdevEvent},
    reexports::{
        calloop::{EventLoop, Interest, Mode, PostAction},
        wayland_server::{Display, DisplayHandle, Client, backend::ClientData},
    },
    wayland::{
        compositor::{CompositorState, CompositorHandler, CompositorClientState},
        socket::ListeningSocketSource,
    },
};
use tracing::info;
use std::{time::Duration, sync::Arc, path::PathBuf};

pub struct FloraState {
    pub display_handle: DisplayHandle,
    pub compositor_state: CompositorState,
    pub should_stop: bool,
    pub drm_devices: Vec<PathBuf>,
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
        // If a DRM device is found but not yet initialized, we can initialize it here
        // (For now we only log, full rendering implementation will follow in the next step)
        if !state.drm_devices.is_empty() {
             let device = state.drm_devices.pop().unwrap();
             info!("Attempting to initialize DRM on: {:?}", device);
             // Full DRM initialization will be implemented here
        }

        event_loop.dispatch(Duration::from_millis(16), &mut state)?;
    }

    Ok(())
}
