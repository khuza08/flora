use smithay::{
    reexports::wayland_server::{DisplayHandle, protocol::wl_surface::WlSurface},
    utils::{Point, Physical, Rectangle},
    wayland::{
        compositor::{CompositorState, CompositorHandler},
        shell::xdg::{XdgShellState},
        shm::{ShmState, ShmHandler},
        selection::{data_device::DataDeviceState, primary_selection::PrimarySelectionState},
        text_input::TextInputManagerState,
    },
    input::{SeatState, SeatHandler, Seat, pointer::CursorImageStatus},
    wayland::text_input::TextInputSeat,
    output::Output,
    backend::{
        drm::{DrmDevice, DrmDeviceFd, compositor::DrmCompositor},
        allocator::gbm::GbmAllocator,
        drm::exporter::gbm::GbmFramebufferExporter,
        egl::EGLDisplay,
        renderer::glow::GlowRenderer,
    },
};

pub use smithay::wayland::compositor::CompositorClientState;

#[cfg(feature = "winit")]
use smithay::backend::winit::WinitGraphicsBackend;

use smithay_egui::EguiState;

pub use smithay::reexports::wayland_server::backend::ClientData;

use std::path::PathBuf;
use crate::compositor::Window;

pub struct FloraClientData {
    pub compositor_state: CompositorClientState,
}

impl ClientData for FloraClientData {}

pub enum BackendData {
    Drm {
        gbm: smithay::backend::allocator::gbm::GbmDevice<DrmDeviceFd>,
        egl: EGLDisplay,
        compositor: DrmCompositor<GbmAllocator<DrmDeviceFd>, GbmFramebufferExporter<DrmDeviceFd>, (), DrmDeviceFd>,
        device: DrmDevice,
    },
    #[cfg(feature = "winit")]
    Winit {
        backend: WinitGraphicsBackend<GlowRenderer>,
        damage_tracker: smithay::backend::renderer::damage::OutputDamageTracker,
    },
    None,
}

pub struct FloraState {
    pub display_handle: DisplayHandle,
    pub compositor_state: CompositorState,
    pub xdg_shell_state: XdgShellState,
    pub shm_state: ShmState,
    pub data_device_state: DataDeviceState,
    pub primary_selection_state: PrimarySelectionState,
    pub xdg_decoration_state: smithay::wayland::shell::xdg::decoration::XdgDecorationState,
    pub text_input_manager_state: TextInputManagerState,
    pub seat_state: SeatState<Self>,
    pub seat: Seat<Self>,
    pub output: Option<Output>,
    pub should_stop: bool,
    pub drm_devices: Vec<PathBuf>,
    pub renderer: Option<GlowRenderer>,
    pub backend_data: BackendData,
    pub windows: Vec<Window>,
    pub pointer_location: Point<f64, Physical>,
    pub grab_state: Option<(usize, Point<f64, Physical>)>,
    pub socket_name: std::ffi::OsString,
    pub needs_redraw: bool,
    pub egui_state: EguiState,
    pub start_time: std::time::Instant,
    // Cursor state
    pub cursor_surface: Option<WlSurface>,
    pub cursor_hotspot: Point<i32, Physical>,
    // Smart damage tracking
    pub last_pointer_location: Point<f64, Physical>,
}

impl FloraState {
    pub fn new(display_handle: &DisplayHandle) -> Self {
        let mut seat_state = SeatState::new();
        let mut seat = seat_state.new_wl_seat(display_handle, "seat0");
        
        seat.add_keyboard(Default::default(), 200, 25).expect("Failed to add keyboard to seat");
        seat.add_pointer();

        Self {
            display_handle: display_handle.clone(),
            compositor_state: CompositorState::new::<Self>(display_handle),
            xdg_shell_state: XdgShellState::new::<Self>(display_handle),
            shm_state: ShmState::new::<Self>(display_handle, vec![]),
            data_device_state: DataDeviceState::new::<Self>(display_handle),
            primary_selection_state: PrimarySelectionState::new::<Self>(display_handle),
            xdg_decoration_state: smithay::wayland::shell::xdg::decoration::XdgDecorationState::new::<Self>(display_handle),
            text_input_manager_state: TextInputManagerState::new::<Self>(display_handle),
            seat_state,
            seat,

            output: None,
            should_stop: false,
            drm_devices: Vec::new(),
            renderer: None,
            backend_data: BackendData::None,
            windows: Vec::new(),
            pointer_location: (0.0, 0.0).into(),
            grab_state: None,
            socket_name: "".into(),
            needs_redraw: false,
            egui_state: EguiState::new(Rectangle::new((0, 0).into(), (1280, 800).into())),
            start_time: std::time::Instant::now(),
            cursor_surface: None,
            cursor_hotspot: (0, 0).into(),
            last_pointer_location: (0.0, 0.0).into(),
        }
    }
    
    /// Send frame callbacks to all clients without GPU rendering.
    /// This allows apps like btop/htop to update while maintaining 0% GPU idle.
    pub fn send_frame_callbacks(&self, display: &std::rc::Rc<std::cell::RefCell<smithay::reexports::wayland_server::Display<FloraState>>>) {
        use smithay::wayland::compositor::{SurfaceAttributes, with_states};
        use smithay::reexports::wayland_server::protocol::wl_callback::WlCallback;
        
        let time = self.start_time.elapsed().as_millis() as u32;
        
        for window in &self.windows {
            with_states(window.toplevel.wl_surface(), |states| {
                let mut attributes = states.cached_state.get::<SurfaceAttributes>();
                let current = attributes.current();
                
                // Send signal to client that frame is "done" (can update now)
                for callback in current.frame_callbacks.drain(..) {
                    let callback: WlCallback = callback;
                    callback.done(time);
                }
            });
        }
        
        // Ensure callbacks are actually sent to client sockets
        let _ = display.borrow_mut().flush_clients();
    }
}

// Delegate implementations
impl SeatHandler for FloraState {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;
    fn seat_state(&mut self) -> &mut SeatState<Self> { &mut self.seat_state }
    fn focus_changed(&mut self, _seat: &Seat<Self>, focused: Option<&WlSurface>) {
        let text_input = self.seat.text_input();
        text_input.set_focus(focused.cloned());
        if focused.is_some() {
            text_input.enter();
        } else {
            text_input.leave();
        }
    }
    fn cursor_image(&mut self, _seat: &Seat<Self>, image: CursorImageStatus) {
        match image {
            CursorImageStatus::Surface(surface) => {
                self.cursor_surface = Some(surface);
            }
            CursorImageStatus::Hidden => {
                self.cursor_surface = None;
            }
            _ => {}
        }
        self.needs_redraw = true;
    }
}

impl CompositorHandler for FloraState {
    fn compositor_state(&mut self) -> &mut CompositorState { &mut self.compositor_state }
    fn client_compositor_state<'a>(&self, client: &'a smithay::reexports::wayland_server::Client) -> &'a smithay::wayland::compositor::CompositorClientState {
        &client.get_data::<FloraClientData>().unwrap().compositor_state
    }
    fn commit(&mut self, surface: &WlSurface) {
        use smithay::backend::renderer::utils::on_commit_buffer_handler;
        on_commit_buffer_handler::<Self>(surface);
        self.needs_redraw = true;
    }
}

impl ShmHandler for FloraState {
    fn shm_state(&self) -> &ShmState { &self.shm_state }
}

impl smithay::wayland::buffer::BufferHandler for FloraState {
    fn buffer_destroyed(&mut self, _buffer: &smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer) {}
}

impl smithay::wayland::selection::SelectionHandler for FloraState {
    type SelectionUserData = ();
}

impl smithay::wayland::selection::data_device::DataDeviceHandler for FloraState {
    fn data_device_state(&self) -> &DataDeviceState { &self.data_device_state }
}

impl smithay::wayland::selection::data_device::ClientDndGrabHandler for FloraState {}
impl smithay::wayland::selection::data_device::ServerDndGrabHandler for FloraState {}

impl smithay::wayland::selection::primary_selection::PrimarySelectionHandler for FloraState {
    fn primary_selection_state(&self) -> &PrimarySelectionState { &self.primary_selection_state }
}

impl smithay::wayland::output::OutputHandler for FloraState {}

smithay::delegate_seat!(FloraState);
smithay::delegate_compositor!(FloraState);
smithay::delegate_shm!(FloraState);
smithay::delegate_data_device!(FloraState);
smithay::delegate_primary_selection!(FloraState);
smithay::delegate_text_input_manager!(FloraState);
smithay::delegate_output!(FloraState);
