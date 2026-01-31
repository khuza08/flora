use smithay::{
    utils::{Point, Physical, Logical, SERIAL_COUNTER},
    input::{keyboard::FilterResult},
};
use crate::compositor::state::FloraState;
use crate::compositor::window::{HitRegion, hit_test_window};
use crate::input::FloraInputEvent;
use crate::compositor::render::get_output_scale;

pub fn handle_input_event(state: &mut FloraState, event: FloraInputEvent) {
    let scale = get_output_scale(state);
    match event {
        FloraInputEvent::Keyboard { keycode, pressed, time } => {
            let serial = SERIAL_COUNTER.next_serial();
            let state_enum = if pressed { smithay::backend::input::KeyState::Pressed } else { smithay::backend::input::KeyState::Released };
            if let Some(keyboard) = state.seat.get_keyboard() {
                keyboard.input::<(), _>(state, keycode.into(), state_enum, serial, time, |_, _, _| FilterResult::Forward);
            }
            state.needs_redraw = true;
        }
        FloraInputEvent::PointerMotion { delta, time } => {
            state.pointer_location += delta;
            clamp_pointer(state);
            forward_pointer_to_egui(state, scale);
            update_grab(state);
            forward_pointer_motion(state, time, scale);
            state.needs_redraw = true;
        }
        FloraInputEvent::PointerMotionAbsolute { location, time } => {
            if let Some(output) = state.output.as_ref() {
                if let Some(mode) = output.current_mode() {
                    let size = mode.size;
                    state.pointer_location.x = location.x * size.w as f64;
                    state.pointer_location.y = location.y * size.h as f64;
                }
            }
            clamp_pointer(state);
            forward_pointer_to_egui(state, scale);
            update_grab(state);
            forward_pointer_motion(state, time, scale);
            state.needs_redraw = true;
        }
        FloraInputEvent::PointerButton { button, pressed, time } => {
            handle_pointer_button(state, button, pressed, time, scale);
            state.needs_redraw = true;
        }
    }
}

pub fn clamp_pointer(state: &mut FloraState) {
    if let Some(output) = state.output.as_ref() {
        if let Some(mode) = output.current_mode() {
            let size = mode.size;
            state.pointer_location.x = state.pointer_location.x.max(0.0).min(size.w as f64);
            state.pointer_location.y = state.pointer_location.y.max(0.0).min(size.h as f64);
        }
    }
}

pub fn update_grab(state: &mut FloraState) {
    if let Some((idx, offset)) = state.grab_state {
        if let Some(window) = state.windows.get_mut(idx) {
            window.location = Point::<i32, Physical>::from((
                (state.pointer_location.x - offset.x).round() as i32,
                (state.pointer_location.y - offset.y).round() as i32
            ));
        }
    }
}

pub fn forward_pointer_to_egui(state: &mut FloraState, scale: f64) {
    let p = state.pointer_location.to_logical(scale);
    state.egui_state.handle_pointer_motion((p.x as i32, p.y as i32).into());
}

pub fn forward_pointer_motion(state: &mut FloraState, time: u32, scale: f64) {
    let serial = SERIAL_COUNTER.next_serial();
    let pointer_logical = state.pointer_location.to_logical(scale);

    if let Some(pointer) = state.seat.get_pointer() {
        let under = state.windows.iter().rev().find_map(|w| {
            let surface_size = w.toplevel.current_state().size.unwrap_or((800, 600).into());
            let window_location_logical = Point::<f64, Logical>::from((w.location.x as f64 / scale, w.location.y as f64 / scale));
            
            match hit_test_window(pointer_logical, window_location_logical, surface_size) {
                HitRegion::Client { local_x, local_y } => {
                    Some((w.toplevel.wl_surface().clone(), Point::<f64, Logical>::from((local_x as f64, local_y as f64))))
                }
                _ => None
            }
        });

        pointer.motion(state, under, &smithay::input::pointer::MotionEvent {
            location: pointer_logical,
            serial, time,
        });
    }
}

pub fn handle_pointer_button(state: &mut FloraState, button: u32, pressed: bool, time: u32, scale: f64) {
    let serial = SERIAL_COUNTER.next_serial();
    let state_enum = if pressed { smithay::backend::input::ButtonState::Pressed } else { smithay::backend::input::ButtonState::Released };
    
    let mb = match button {
        0x110 => Some(smithay::backend::input::MouseButton::Left),
        0x111 => Some(smithay::backend::input::MouseButton::Right),
        0x112 => Some(smithay::backend::input::MouseButton::Middle),
        _ => None,
    };
    
    if let Some(mouse_button) = mb {
        state.egui_state.handle_pointer_button(mouse_button, pressed);
    }
    
    if state.egui_state.wants_pointer() {
        state.needs_redraw = true;
        return;
    }
    
    if pressed {
        let pointer_logical = state.pointer_location.to_logical(scale);
        let hit = state.windows.iter().enumerate().rev().find_map(|(i, w)| {
            let surface_size = w.toplevel.current_state().size.unwrap_or((800, 600).into());
            let window_location_logical = Point::<f64, Logical>::from((w.location.x as f64 / scale, w.location.y as f64 / scale));
            
            let region = hit_test_window(pointer_logical, window_location_logical, surface_size);
            if region != HitRegion::None {
                let w_loc_phys = Point::<f64, Physical>::from((w.location.x as f64, w.location.y as f64));
                Some((i, state.pointer_location - w_loc_phys, region))
            } else {
                None
            }
        });

        if let Some((idx, offset, region)) = hit {
            let surface = state.windows[idx].toplevel.wl_surface().clone();
            if let Some(keyboard) = state.seat.get_keyboard() {
                keyboard.set_focus(state, Some(surface), serial);
            }
            
            let win = state.windows.remove(idx);
            state.windows.push(win);
            
            if region == HitRegion::TitleBar {
                state.grab_state = Some((state.windows.len() - 1, offset));
            }
        }

    } else {
        state.grab_state = None;
    }

    if let Some(pointer) = state.seat.get_pointer() {
        pointer.button(state, &smithay::input::pointer::ButtonEvent {
            button, state: state_enum, serial, time,
        });
    }
}
