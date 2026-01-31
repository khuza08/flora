use smithay::{
    reexports::wayland_server::{DisplayHandle, protocol::wl_surface::WlSurface},
    utils::{Point, Physical},
    wayland::{
        compositor::{CompositorState, CompositorHandler},
        shell::xdg::{XdgShellState, XdgShellHandler, ToplevelSurface, PopupSurface, PositionerState},
        shm::{ShmState, ShmHandler},
        selection::{data_device::DataDeviceState, primary_selection::PrimarySelectionState},
        text_input::TextInputManagerState,
    },
    input::{SeatState, SeatHandler, Seat},
    output::Output,
    backend::{
        drm::{DrmDevice, DrmDeviceFd, compositor::DrmCompositor},
        allocator::gbm::GbmAllocator,
        drm::exporter::gbm::GbmFramebufferExporter,
        egl::EGLDisplay,
        renderer::{
            glow::GlowRenderer,
            element::memory::MemoryRenderBuffer,
        },
    },
};

use crate::decorations::{create_circle_buffer, RED_BUTTON_COLOR, YELLOW_BUTTON_COLOR, GREEN_BUTTON_COLOR};

pub use smithay::reexports::wayland_server::backend::ClientData;
pub use smithay::wayland::compositor::CompositorClientState;

use std::path::PathBuf;

pub struct FloraClientData {
    pub compositor_state: CompositorClientState,
}

impl ClientData for FloraClientData {}

pub struct Window {
    pub toplevel: ToplevelSurface,
    pub location: Point<i32, Physical>,
    pub title_bar_height: i32,
    pub bar_id: smithay::backend::renderer::element::Id,
    pub red_id: smithay::backend::renderer::element::Id,
    pub yellow_id: smithay::backend::renderer::element::Id,
    pub green_id: smithay::backend::renderer::element::Id,
}


pub const TITLE_BAR_HEIGHT: i32 = 30;
pub const BUTTON_SIZE: i32 = 12;
pub const BUTTON_SPACING: i32 = 8;
pub const MARGIN: i32 = 10;



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
    // Backend storage to keep them alive
    pub _gbm_device: Option<smithay::backend::allocator::gbm::GbmDevice<DrmDeviceFd>>,
    pub _egl_display: Option<EGLDisplay>,
    // The compositor that handles rendering to a specific CRT/Connector
    pub compositor: Option<DrmCompositor<GbmAllocator<DrmDeviceFd>, GbmFramebufferExporter<DrmDeviceFd>, (), DrmDeviceFd>>,
    // Windows for rendering and interaction
    pub windows: Vec<Window>,
    pub pointer_location: Point<f64, Physical>,
    pub grab_state: Option<(usize, Point<f64, Physical>)>,
    pub socket_name: std::ffi::OsString,
    pub needs_redraw: bool,
    pub _drm_device: Option<DrmDevice>,
    // Pre-generated circle button textures
    pub red_button_buffer: MemoryRenderBuffer,
    pub yellow_button_buffer: MemoryRenderBuffer,
    pub green_button_buffer: MemoryRenderBuffer,
}

impl FloraState {
    pub fn new(display_handle: &DisplayHandle) -> Self {
        let mut seat_state = SeatState::new();
        let mut seat = seat_state.new_wl_seat(display_handle, "seat0");
        
        // Add keyboard and pointer capabilities to the seat
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
            _gbm_device: None,
            _egl_display: None,
            compositor: None,
            windows: Vec::new(),
            pointer_location: (0.0, 0.0).into(),
            grab_state: None,
            socket_name: "".into(),
            needs_redraw: false,
            _drm_device: None,
            // Create circle button textures
            red_button_buffer: create_circle_buffer(BUTTON_SIZE, RED_BUTTON_COLOR),
            yellow_button_buffer: create_circle_buffer(BUTTON_SIZE, YELLOW_BUTTON_COLOR),
            green_button_buffer: create_circle_buffer(BUTTON_SIZE, GREEN_BUTTON_COLOR),
        }
    }
}

// Delegate implementations

impl SeatHandler for FloraState {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;
    fn seat_state(&mut self) -> &mut SeatState<Self> { &mut self.seat_state }
    fn focus_changed(&mut self, _seat: &Seat<Self>, focused: Option<&WlSurface>) {
        use smithay::wayland::text_input::TextInputSeat;
        
        // Update text input focus to track keyboard focus
        let text_input = self.seat.text_input();
        text_input.set_focus(focused.cloned());
        if focused.is_some() {
            text_input.enter();
        } else {
            text_input.leave();
        }
    }
    fn cursor_image(&mut self, _seat: &Seat<Self>, _image: smithay::input::pointer::CursorImageStatus) {}
}

smithay::delegate_seat!(FloraState);

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

smithay::delegate_compositor!(FloraState);

impl XdgShellHandler for FloraState {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState { &mut self.xdg_shell_state }
    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        surface.with_pending_state(|state| {
            state.size = Some((800, 600).into());
        });
        surface.send_configure();
        let wl_surface = surface.wl_surface().clone();
        self.windows.push(Window { 
            toplevel: surface, 
            location: (100, 100).into(),
            title_bar_height: TITLE_BAR_HEIGHT,
            bar_id: smithay::backend::renderer::element::Id::new(),
            red_id: smithay::backend::renderer::element::Id::new(),
            yellow_id: smithay::backend::renderer::element::Id::new(),
            green_id: smithay::backend::renderer::element::Id::new(),
        });
        if let Some(keyboard) = self.seat.get_keyboard() {
            keyboard.set_focus(self, Some(wl_surface.clone()), smithay::utils::SERIAL_COUNTER.next_serial());
        }
        if let Some(output) = self.output.as_ref() { output.enter(&wl_surface); }
        self.needs_redraw = true;
    }
    fn new_popup(&mut self, surface: PopupSurface, _positioner: PositionerState) { surface.send_configure().ok(); }
    fn grab(&mut self, _surface: PopupSurface, _seat: smithay::reexports::wayland_server::protocol::wl_seat::WlSeat, _serial: smithay::utils::Serial) {}
    fn reposition_request(&mut self, _surface: PopupSurface, _positioner: PositionerState, _token: u32) {}
    fn ack_configure(&mut self, _surface: WlSurface, _configure: smithay::wayland::shell::xdg::Configure) {}
}

smithay::delegate_xdg_shell!(FloraState);

impl ShmHandler for FloraState {
    fn shm_state(&self) -> &ShmState { &self.shm_state }
}

smithay::delegate_shm!(FloraState);

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

impl smithay::wayland::shell::xdg::decoration::XdgDecorationHandler for FloraState {
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode::ServerSide);
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
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode::ServerSide);
        });
        toplevel.send_configure();
    }
}

smithay::delegate_xdg_decoration!(FloraState);

impl smithay::wayland::output::OutputHandler for FloraState {}

smithay::delegate_data_device!(FloraState);
smithay::delegate_primary_selection!(FloraState);
smithay::delegate_text_input_manager!(FloraState);
smithay::delegate_output!(FloraState);
