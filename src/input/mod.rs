pub mod handler;
pub mod libinput;

use smithay::utils::{Point, Physical};

#[derive(Debug, Clone)]
pub enum FloraInputEvent {
    Keyboard {
        keycode: u32,
        pressed: bool,
        time: u32,
    },
    PointerMotion {
        delta: Point<f64, Physical>,
        time: u32,
    },
    PointerMotionAbsolute {
        location: Point<f64, Physical>,
        time: u32,
    },
    PointerButton {
        button: u32,
        pressed: bool,
        time: u32,
    },
}
