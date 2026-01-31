use smithay::{
    wayland::shell::xdg::{ToplevelSurface, decoration::XdgDecorationHandler},
    reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1,
};
use crate::compositor::state::FloraState;

impl XdgDecorationHandler for FloraState {
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(zxdg_toplevel_decoration_v1::Mode::ServerSide);
        });
        toplevel.send_configure();
    }
    fn request_mode(&mut self, toplevel: ToplevelSurface, mode: zxdg_toplevel_decoration_v1::Mode) {
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(mode);
        });
        toplevel.send_configure();
    }
    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(zxdg_toplevel_decoration_v1::Mode::ServerSide);
        });
        toplevel.send_configure();
    }
}

smithay::delegate_xdg_decoration!(FloraState);
