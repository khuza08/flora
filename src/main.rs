pub mod state;
mod input;
mod backend;

use smithay::{
    backend::{
        udev::{UdevBackend, UdevEvent},
        renderer::{
            glow::GlowRenderer,
            element::{Kind, surface::{render_elements_from_surface_tree, WaylandSurfaceRenderElement}, solid::SolidColorRenderElement, texture::TextureRenderElement},
        },
        drm::DrmEvent,
    },
    reexports::{
        calloop::{EventLoop, Interest, Mode, PostAction, generic::Generic, channel::Event},
        wayland_server::Display,
    },
    utils::{SERIAL_COUNTER, Point, Physical, Rectangle},
    wayland::{
        compositor::{with_surface_tree_downward, TraversalAction, SurfaceAttributes},
    },
    input::keyboard::FilterResult,
};

use smithay::backend::renderer::gles::GlesTexture;

use std::{time::Duration, rc::Rc, cell::RefCell, os::unix::io::{AsRawFd, BorrowedFd}};
use tracing::{info, warn, error};
use anyhow::Result;

use crate::state::{FloraState, FloraClientData, CompositorClientState, TITLE_BAR_HEIGHT};
use crate::input::{FloraInputEvent, spawn_input_thread};
use crate::backend::init_graphics;

smithay::backend::renderer::element::render_elements! {
    pub CustomRenderElement<=GlowRenderer>;
    Surface=WaylandSurfaceRenderElement<GlowRenderer>,
    Solid=SolidColorRenderElement,
    Egui=TextureRenderElement<GlesTexture>,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt().init();
    info!("Flora: Starting macOS-like Compositor...");

    // 1. TTY Graphics Mode setup with restoration on exit
    let tty_fd = setup_tty_graphics()?;
    
    // 2. Setup Event Loop
    let mut event_loop: EventLoop<FloraState> = EventLoop::try_new()?;
    let handle = event_loop.handle();

    // 3. Setup Wayland Display
    let mut display_raw = Display::new()?;
    let poll_fd = display_raw.backend().poll_fd().as_raw_fd();
    let dh = display_raw.handle();
    let display = Rc::new(RefCell::new(display_raw));

    // 4. Initialize State
    let mut state = FloraState::new(&dh);
    
    // 5. Setup Wayland Socket
    let source = smithay::wayland::socket::ListeningSocketSource::new_auto()?;
    state.socket_name = source.socket_name().to_os_string();
    info!("Flora active! Socket Name: {:?}", state.socket_name);

    handle.insert_source(source, |client_stream, _, state| {
        use std::sync::Arc;
        let client_data = FloraClientData {
            compositor_state: CompositorClientState::default(),
        };
        let mut dh = state.display_handle.clone();
        let _ = dh.insert_client(client_stream, Arc::new(client_data));
    }).map_err(|_| anyhow::anyhow!("Failed to insert socket source"))?;

    // 6. Setup Display Event Source
    let display_event_clone = display.clone();
    handle.insert_source(
        Generic::new(unsafe { BorrowedFd::borrow_raw(poll_fd) }, Interest::READ, Mode::Level),
        move |_event, _metadata, state| {
            let mut disp = display_event_clone.borrow_mut();
            let _ = disp.dispatch_clients(state);
            let _ = disp.flush_clients();
            Ok(PostAction::Continue)
        }
    ).map_err(|_| anyhow::anyhow!("Failed to insert display event source"))?;

    // 7. Setup Udev Backend
    let udev = UdevBackend::new("seat0")?;
    for (_device_id, dev_path) in udev.device_list() {
        if dev_path.to_string_lossy().contains("card") || dev_path.to_string_lossy().contains("render") {
            state.drm_devices.push(dev_path.to_path_buf());
        }
    }
    handle.insert_source(udev, |event, _, state| {
        if let UdevEvent::Added { path, .. } = event {
            if path.to_string_lossy().contains("card") || path.to_string_lossy().contains("render") {
                state.drm_devices.push(path);
            }
        }
    }).map_err(|_| anyhow::anyhow!("Failed to insert udev source"))?;

    // 8. Main Loop
    let mut input_initialized = false;
    let (input_sender, input_receiver) = smithay::reexports::calloop::channel::channel::<FloraInputEvent>();

    let display_input_clone = display.clone();
    handle.insert_source(input_receiver, move |event, _, state| {
        if let Event::Msg(input_event) = event {
            handle_input_event(state, input_event);
            let _ = display_input_clone.borrow_mut().flush_clients();
        }
    }).expect("Failed to insert input source");

    info!("Flora: Entering main event loop...");
    while !state.should_stop {
        // A. Graphics Initialization
        if state.renderer.is_none() && !state.drm_devices.is_empty() {
            let device_path = state.drm_devices.pop().unwrap();
            match init_graphics(&device_path) {
                Ok((gbm, egl, renderer, output, compositor, drm_device, notifier)) => {
                    handle.insert_source(notifier, |event, _, state| {
                        match event {
                            DrmEvent::VBlank(_) => state.needs_redraw = true,
                            DrmEvent::Error(err) => error!("DRM Event Error: {:?}", err),
                        }
                    }).expect("Failed to insert DRM notifier");

                    state._gbm_device = Some(gbm);
                    state._egl_display = Some(egl);
                    state.renderer = Some(renderer);
                    
                    // Advertise the output to clients
                    output.create_global::<FloraState>(&state.display_handle);
                    state.output = Some(output);
                    
                    state.compositor = Some(compositor);
                    state._drm_device = Some(drm_device);
                    state.needs_redraw = true;
                    info!("Screen output initialized successfully!");

                    if !input_initialized {
                        spawn_input_thread(input_sender.clone());
                        input_initialized = true;
                    }
                }
                Err(e) => {
                    error!("Failed to initialize graphics for {:?}: {}", device_path, e);
                }
            }
        }

        // B. Dispatch Events
        if let Err(e) = event_loop.dispatch(Duration::from_millis(16), &mut state) {
            error!("Event loop error: {:?}", e);
            break;
        }

        // C. Rendering
        if state.needs_redraw {
            render_frame(&mut state, &display)?;
            state.needs_redraw = false;
        }
    }

    // 9. Shutdown & Cleanup
    info!("Flora: Shutting down...");
    restore_tty(tty_fd);
    Ok(())
}

fn setup_tty_graphics() -> Result<Option<std::fs::File>> {
    unsafe {
        for path in ["/dev/tty0", "/dev/tty"] {
            if let Ok(file) = std::fs::OpenOptions::new().read(true).write(true).open(path) {
                let fd = file.as_raw_fd();
                const KDSETMODE: libc::c_ulong = 0x4B3A;
                const KD_GRAPHICS: libc::c_ulong = 0x01;
                if libc::ioctl(fd, KDSETMODE, KD_GRAPHICS) == 0 {
                    info!("Flora: TTY switched to Graphics mode on {}", path);
                    return Ok(Some(file));
                }
            }
        }
    }
    warn!("Flora: Could not switch TTY to graphics mode. Continuing anyway.");
    Ok(None)
}

fn restore_tty(tty_file: Option<std::fs::File>) {
    if let Some(file) = tty_file {
        unsafe {
            let fd = file.as_raw_fd();
            const KDSETMODE: libc::c_ulong = 0x4B3A;
            const KD_TEXT: libc::c_ulong = 0x00;
            if libc::ioctl(fd, KDSETMODE, KD_TEXT) == 0 {
                info!("Flora: TTY restored to Text mode.");
            } else {
                warn!("Flora: Failed to restore TTY text mode.");
            }
        }
    }
}

fn handle_input_event(state: &mut FloraState, event: FloraInputEvent) {
    match event {
        FloraInputEvent::Keyboard { keycode, pressed, time } => {
            let serial = SERIAL_COUNTER.next_serial();
            let state_enum = if pressed { smithay::backend::input::KeyState::Pressed } else { smithay::backend::input::KeyState::Released };
            if let Some(keyboard) = state.seat.get_keyboard() {
                keyboard.input::<(), _>(state, (keycode + 8).into(), state_enum, serial, time, |_, _, _| FilterResult::Forward);
            }
            state.needs_redraw = true;
        }
        FloraInputEvent::PointerMotion { delta, time } => {
            state.pointer_location += delta;
            clamp_pointer(state);
            let scale = get_output_scale(state);
            forward_pointer_to_egui(state, scale);
            update_grab(state);
            forward_pointer_motion(state, time);
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
            
            let scale = get_output_scale(state);
            forward_pointer_to_egui(state, scale);
            update_grab(state);
            forward_pointer_motion(state, time);
            state.needs_redraw = true;
        }
        FloraInputEvent::PointerButton { button, pressed, time } => {
            handle_pointer_button(state, button, pressed, time);
            state.needs_redraw = true;
        }
    }
}

fn forward_pointer_to_egui(state: &mut FloraState, scale: f64) {
    let p = state.pointer_location.to_logical(scale);
    state.egui_state.handle_pointer_motion((p.x as i32, p.y as i32).into());
}

fn get_output_scale(state: &FloraState) -> f64 {
    state.output.as_ref()
        .map(|o| o.current_scale().fractional_scale())
        .unwrap_or(1.0)
}

/// Hit region for window hit-testing
#[derive(Debug, Clone, Copy, PartialEq)]
enum HitRegion {
    TitleBar,
    Client { local_x: i32, local_y: i32 },
    None,
}

/// Shared hit-test logic for a single window
fn hit_test_window(pointer: Point<f64, Physical>, window_location: Point<i32, Physical>, surface_size: smithay::utils::Size<i32, smithay::utils::Logical>) -> HitRegion {
    let px = pointer.x.round() as i32;
    let py = pointer.y.round() as i32;
    let relative_x = px - window_location.x;
    let relative_y = py - window_location.y;
    
    if relative_x >= 0 && relative_x < surface_size.w {
        if relative_y >= 0 && relative_y < TITLE_BAR_HEIGHT {
            HitRegion::TitleBar
        } else if relative_y >= TITLE_BAR_HEIGHT && relative_y < (TITLE_BAR_HEIGHT + surface_size.h) {
            HitRegion::Client { local_x: relative_x, local_y: relative_y - TITLE_BAR_HEIGHT }
        } else {
            HitRegion::None
        }
    } else {
        HitRegion::None
    }
}

fn clamp_pointer(state: &mut FloraState) {
    if let Some(output) = state.output.as_ref() {
        if let Some(mode) = output.current_mode() {
            let size = mode.size;
            state.pointer_location.x = state.pointer_location.x.max(0.0).min(size.w as f64);
            state.pointer_location.y = state.pointer_location.y.max(0.0).min(size.h as f64);
        }
    }
}

fn update_grab(state: &mut FloraState) {
    if let Some((idx, offset)) = state.grab_state {
        if let Some(window) = state.windows.get_mut(idx) {
            window.location = Point::<i32, Physical>::from((
                (state.pointer_location.x - offset.x).round() as i32,
                (state.pointer_location.y - offset.y).round() as i32
            ));
        }
    }
}

fn forward_pointer_motion(state: &mut FloraState, time: u32) {
    let serial = SERIAL_COUNTER.next_serial();
    let scale = get_output_scale(state);
    
    if let Some(pointer) = state.seat.get_pointer() {
        let under = state.windows.iter().rev().find_map(|w| {
            let surface_size = w.toplevel.current_state().size.unwrap_or((800, 600).into());
            match hit_test_window(state.pointer_location, w.location, surface_size) {
                HitRegion::Client { local_x, local_y } => {
                    Some((w.toplevel.wl_surface().clone(), Point::<f64, smithay::utils::Logical>::from((local_x as f64, local_y as f64))))
                }
                _ => None
            }
        });

        pointer.motion(state, under, &smithay::input::pointer::MotionEvent {
            location: state.pointer_location.to_logical(scale),
            serial, time,
        });
    }
}

fn handle_pointer_button(state: &mut FloraState, button: u32, pressed: bool, time: u32) {
    let serial = SERIAL_COUNTER.next_serial();
    let state_enum = if pressed { smithay::backend::input::ButtonState::Pressed } else { smithay::backend::input::ButtonState::Released };
    
    // Forward to egui only for known buttons
    let mb = match button {
        0x110 => Some(smithay::backend::input::MouseButton::Left),
        0x111 => Some(smithay::backend::input::MouseButton::Right),
        0x112 => Some(smithay::backend::input::MouseButton::Middle),
        _ => None,
    };
    
    if let Some(mouse_button) = mb {
        state.egui_state.handle_pointer_button(mouse_button, pressed);
    }
    
    // Intercept if egui wants it
    if state.egui_state.wants_pointer() {
        state.needs_redraw = true;
        return;
    }
    
    if pressed {
        // Use shared hit-test helper
        let hit = state.windows.iter().enumerate().rev().find_map(|(i, w)| {
            let surface_size = w.toplevel.current_state().size.unwrap_or((800, 600).into());
            let region = hit_test_window(state.pointer_location, w.location, surface_size);
            if region != HitRegion::None {
                let w_loc_f = Point::<f64, Physical>::from((w.location.x as f64, w.location.y as f64));
                Some((i, state.pointer_location - w_loc_f, region))
            } else {
                None
            }
        });

        if let Some((idx, offset, region)) = hit {
            let surface = state.windows[idx].toplevel.wl_surface().clone();
            if let Some(keyboard) = state.seat.get_keyboard() {
                keyboard.set_focus(state, Some(surface), serial);
            }
            
            // Bring window to front
            let win = state.windows.remove(idx);
            state.windows.push(win);
            
            // Start grab if title bar was clicked
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

fn render_frame(state: &mut FloraState, display: &Rc<RefCell<smithay::reexports::wayland_server::Display<FloraState>>>) -> Result<()> {
    if let (Some(compositor), Some(renderer)) = (state.compositor.as_mut(), state.renderer.as_mut()) {
        let color = [0.2, 0.2, 0.2, 1.0];
        let mut elements: Vec<CustomRenderElement> = Vec::new();
        
        // Get output geometry for egui
        let output_size = state.output.as_ref()
            .and_then(|o| o.current_mode())
            .map(|m| m.size)
            .unwrap_or((1280, 800).into());
        
        // Collect window data for egui (to avoid borrow issues)
        let window_data: Vec<_> = state.windows.iter().enumerate().map(|(idx, w)| {
            let surface_size = w.toplevel.current_state().size.unwrap_or((800, 600).into());
            let is_focused = state.seat.get_keyboard()
                .map(|kb| kb.current_focus().map(|f| f == *w.toplevel.wl_surface()).unwrap_or(false))
                .unwrap_or(false);
            (idx, w.location, surface_size, is_focused)
        }).collect();
        
        let mut pending_close = None;
        
        // DEBUG: Log window count for egui rendering
        if !window_data.is_empty() {
            info!("Rendering {} window(s) with egui overlay", window_data.len());
        }
        
        // Render egui UI overlay
        let egui_element = state.egui_state.render(
            |ctx| {
                for (idx, window_pos, surface_size, is_focused) in &window_data {
                    // Create a fixed window for each titlebar
                    egui::Area::new(egui::Id::new(format!("titlebar_{}", idx)))
                        .fixed_pos([window_pos.x as f32, window_pos.y as f32])
                        .show(ctx, |ui| {
                            // Titlebar background - paint at absolute position
                            let title_rect = egui::Rect::from_min_size(
                                egui::pos2(window_pos.x as f32, window_pos.y as f32),
                                egui::vec2(surface_size.w as f32, TITLE_BAR_HEIGHT as f32),
                            );
                            ui.painter().rect_filled(title_rect, 0.0, egui::Color32::from_rgb(38, 38, 38));
                            
                            // macOS button colors - colored when focused, gray when not
                            let colors = if *is_focused {
                                [
                                    egui::Color32::from_rgb(255, 95, 87),  // Red (Close)
                                    egui::Color32::from_rgb(255, 189, 46), // Yellow (Minimize)
                                    egui::Color32::from_rgb(40, 200, 64),  // Green (Maximize)
                                ]
                            } else {
                                [egui::Color32::from_rgb(75, 75, 75); 3] // Gray when inactive
                            };
                            
                            // Hover icons (macOS style)
                            let icons = ["✕", "—", "＋"];
                            
                            // Tweak geometry
                            let btn_radius = 6.0_f32;
                            let btn_spacing = 8.0_f32;
                            let left_margin = 12.0_f32;
                            let center_y = window_pos.y as f32 + (TITLE_BAR_HEIGHT as f32 / 2.0);
                            
                            // Group rect for unified hover feel
                            let group_rect = egui::Rect::from_min_max(
                                egui::pos2(window_pos.x as f32 + left_margin - 4.0, center_y - 10.0),
                                egui::pos2(window_pos.x as f32 + left_margin + 50.0, center_y + 10.0)
                            );
                            let is_hovering_group = ui.rect_contains_pointer(group_rect);
                            
                            for (i, btn_color) in colors.iter().enumerate() {
                                // Calculate center position for each button
                                let center_x = window_pos.x as f32 + left_margin + btn_radius 
                                    + (i as f32 * (btn_radius * 2.0 + btn_spacing));
                                let center = egui::pos2(center_x, center_y);
                                
                                // Draw circle button
                                ui.painter().circle_filled(center, btn_radius, *btn_color);
                                
                                // Draw hover icon when hovering and focused
                                if is_hovering_group && *is_focused {
                                    let icon_color = egui::Color32::from_rgba_unmultiplied(0, 0, 0, 160);
                                    ui.painter().text(
                                        center,
                                        egui::Align2::CENTER_CENTER,
                                        icons[i],
                                        egui::FontId::proportional(8.5),
                                        icon_color,
                                    );
                                }
                                
                                // Create interaction area for click detection
                                let btn_rect = egui::Rect::from_center_size(center, egui::vec2(15.0, 15.0));
                                let response = ui.allocate_rect(btn_rect, egui::Sense::click());
                                
                                if response.clicked() && *is_focused {
                                    if i == 0 {
                                        pending_close = Some(*idx);
                                    }
                                }
                            }
                        });
                }
            },
            renderer,
            Rectangle::new((0, 0).into(), (output_size.w, output_size.h).into()),
            1.0,
            1.0,
        );
        
        // Execute pending actions from egui
        if let Some(idx) = pending_close {
            state.windows[idx].toplevel.send_close();
        }
        
        // RENDER ORDER: Background elements first (rendered at the back)
        // 1. Title Bar Backgrounds (solid gray - at the very back)
        for window in &state.windows {
            let surface_size = window.toplevel.current_state().size.unwrap_or((800, 600).into());
            let bar_rect = smithay::utils::Rectangle::new(window.location, (surface_size.w, TITLE_BAR_HEIGHT).into());
            elements.push(CustomRenderElement::Solid(SolidColorRenderElement::new(
                window.bar_id.clone(),
                bar_rect,
                window.bar_commit_counter.clone(),
                [0.15, 0.15, 0.15, 1.0],
                Kind::Unspecified
            )));
        }
        
        // 2. Client Surfaces (shifted down by TITLE_BAR_HEIGHT)
        for window in &state.windows {
            let surface_location = Point::from((window.location.x, window.location.y + TITLE_BAR_HEIGHT));
            elements.extend(render_elements_from_surface_tree::<GlowRenderer, CustomRenderElement>(
                renderer, 
                window.toplevel.wl_surface(), 
                surface_location, 
                1.0, 1.0, 
                Kind::Unspecified
            ));
        }
        
        // 3. Egui Overlay (on top of everything)
        match egui_element {
            Ok(egui_tex) => {
                elements.push(CustomRenderElement::Egui(egui_tex));
            }
            Err(err) => {
                error!("Failed to render egui overlay: {:?}", err);
            }
        }
        
        if let Err(e) = compositor.render_frame::<GlowRenderer, CustomRenderElement>(renderer, &elements, color, smithay::backend::drm::compositor::FrameFlags::empty()) {

            if format!("{:?}", e) != "EmptyFrame" {
                error!("Rendering: render_frame failed: {:?}", e);
            }
        }
        
        let time = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u32;
        for window in &state.windows {
            with_surface_tree_downward(window.toplevel.wl_surface(), (), |_, _, _| TraversalAction::DoChildren(()), |_, states, _| {
                let mut guard = states.cached_state.get::<SurfaceAttributes>();
                for callback in guard.current().frame_callbacks.drain(..) { callback.done(time); }
            }, |_, _, _| true);
        }

        if let Err(e) = compositor.commit_frame() {
            if format!("{:?}", e) != "EmptyFrame" {
                error!("Rendering: commit_frame failed: {:?}", e);
            }
        }

        let _ = display.borrow_mut().flush_clients();
    }
    Ok(())
}
