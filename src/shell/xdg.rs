use smithay::{
    wayland::shell::xdg::{XdgShellHandler, XdgShellState, ToplevelSurface, PopupSurface, PositionerState},
    reexports::wayland_server::protocol::wl_surface::WlSurface,
};
use crate::compositor::state::FloraState;
use crate::compositor::Window;

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
            bar_id: smithay::backend::renderer::element::Id::new(),
            bar_commit_counter: smithay::backend::renderer::utils::CommitCounter::default(),
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
