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
        winit::WinitEvent,
    },
    reexports::{
        calloop::{EventLoop, Interest, Mode, PostAction, generic::Generic, channel::Event},
        wayland_server::Display,
    },
    input::keyboard::FilterResult,
    utils::{SERIAL_COUNTER, Point, Physical, Logical, Rectangle, Size},
    wayland::compositor::{with_surface_tree_downward, TraversalAction, SurfaceAttributes},
};
use smithay_egui::EguiState;

use smithay::backend::renderer::gles::GlesTexture;

use std::{time::Duration, rc::Rc, cell::RefCell, os::unix::io::{AsRawFd, BorrowedFd}};
use tracing::{info, warn, error};
use anyhow::Result;

use crate::state::{FloraState, FloraClientData, CompositorClientState, TITLE_BAR_HEIGHT, BackendData};
use crate::input::{FloraInputEvent, spawn_input_thread};
use crate::backend::{drm::init_drm_graphics, winit::init_winit_graphics};

smithay::backend::renderer::element::render_elements! {
    pub CustomRenderElement<=GlowRenderer>;
    Surface=WaylandSurfaceRenderElement<GlowRenderer>,
    Solid=SolidColorRenderElement,
    Egui=TextureRenderElement<GlesTexture>,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt().init();
    info!("Flora: Starting macOS-like Compositor...");

    // 1. Setup Event Loop
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
    
    // Check if we should use Winit (nested) or DRM (native)
    let use_winit = std::env::var("WAYLAND_DISPLAY").is_ok() || std::env::var("DISPLAY").is_ok();
    
    let mut tty_file: Option<std::fs::File> = None;
    if use_winit {
        match init_winit_graphics() {
            Ok((backend, winit_event_loop, output)) => {
                // Advertise the output
                output.create_global::<FloraState>(&state.display_handle);
                state.output = Some(output.clone());
                
                state.backend_data = BackendData::Winit { 
                    backend,
                    damage_tracker: smithay::backend::renderer::damage::OutputDamageTracker::from_output(&output),
                };
                state.needs_redraw = true;
                info!("Winit (Nested) initialized successfully!");

                // Insert winit event loop into calloop
                handle.insert_source(winit_event_loop, |event, _, state| {
                    match event {
                        WinitEvent::Resized { size, .. } => {
                            info!("Winit Resized: {:?}", size);
                            if let Some(output) = state.output.as_ref() {
                                let mode = smithay::output::Mode { size, refresh: 60_000 };
                                output.change_current_state(Some(mode), None, None, None);
                            }
                            state.needs_redraw = true;
                        }
                        WinitEvent::Input(_input_event) => {
                            // Forward input to handle_input_event
                            // Note: Winit input needs conversion to FloraInputEvent or handled directly
                            // Simplified for now: just trigger redraw on input
                            state.needs_redraw = true;
                            
                            // Map Winit events to FloraInputEvent if possible
                            // For now, let's just use the winit source directly to update state
                        }
                        WinitEvent::Redraw => {
                            state.needs_redraw = true;
                        }
                        WinitEvent::CloseRequested => {
                            state.should_stop = true;
                        }
                        _ => {}
                    }
                }).expect("Failed to insert Winit event loop");
            }
            Err(e) => {
                error!("Failed to initialize Winit backend: {}", e);
            }
        }
    } else {
        // DRM setup
        tty_file = setup_tty_graphics()?;
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
    }

    while !state.should_stop {
        // A. DRM Graphics Initialization (if using DRM and not yet initialized)
        if !use_winit && state.renderer.is_none() && !state.drm_devices.is_empty() {
            let device_path = state.drm_devices.pop().unwrap();
            match init_drm_graphics(&device_path) {
                Ok((gbm, egl, renderer, output, compositor, drm_device, notifier)) => {
                    handle.insert_source(notifier, |event, _, state| {
                        match event {
                            DrmEvent::VBlank(_) => state.needs_redraw = true,
                            DrmEvent::Error(err) => error!("DRM Event Error: {:?}", err),
                        }
                    }).expect("Failed to insert DRM notifier");

                    state.renderer = Some(renderer);
                    
                    // Advertise the output to clients
                    output.create_global::<FloraState>(&state.display_handle);
                    state.output = Some(output);
                    
                    state.backend_data = BackendData::Drm { gbm, egl, compositor, device: drm_device };
                    state.needs_redraw = true;
                    info!("DRM (Native) initialized successfully!");

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
    restore_tty(tty_file);
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
    let scale = get_output_scale(state);
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

/// Shared hit-test logic for a single window operating in logical space
fn hit_test_window(
    pointer_logical: Point<f64, Logical>, 
    window_location_logical: Point<f64, Logical>, 
    surface_size: smithay::utils::Size<i32, Logical>
) -> HitRegion {
    let relative_x = pointer_logical.x - window_location_logical.x;
    let relative_y = pointer_logical.y - window_location_logical.y;
    
    if relative_x >= 0.0 && relative_x < surface_size.w as f64 {
        if relative_y >= 0.0 && relative_y < TITLE_BAR_HEIGHT as f64 {
            HitRegion::TitleBar
        } else if relative_y >= TITLE_BAR_HEIGHT as f64 && relative_y < (TITLE_BAR_HEIGHT + surface_size.h) as f64 {
            HitRegion::Client { 
                local_x: relative_x.round() as i32, 
                local_y: (relative_y - TITLE_BAR_HEIGHT as f64).round() as i32 
            }
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

fn forward_pointer_motion(state: &mut FloraState, time: u32, scale: f64) {
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

fn handle_pointer_button(state: &mut FloraState, button: u32, pressed: bool, time: u32, scale: f64) {
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
        let pointer_logical = state.pointer_location.to_logical(scale);
        // Use shared hit-test helper
        let hit = state.windows.iter().enumerate().rev().find_map(|(i, w)| {
            let surface_size = w.toplevel.current_state().size.unwrap_or((800, 600).into());
            let window_location_logical = Point::<f64, Logical>::from((w.location.x as f64 / scale, w.location.y as f64 / scale));
            
            let region = hit_test_window(pointer_logical, window_location_logical, surface_size);
            if region != HitRegion::None {
                // Return offset in physical space for grabbing
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

fn render_frame(state: &mut FloraState, display: &Rc<RefCell<Display<FloraState>>>) -> Result<()> {
    let scale = get_output_scale(state);
    
    // 1. Get output size
    let output_size = match &state.backend_data {
        BackendData::Drm { .. } => {
            state.output.as_ref()
                .and_then(|o| o.current_mode())
                .map(|m| m.size)
                .unwrap_or((1280, 800).into())
        }
        BackendData::Winit { backend, .. } => {
            backend.window_size()
        }
        BackendData::None => return Ok(()),
    };

    let color = [0.1, 0.1, 0.1, 1.0];

    // 2. Collect immutable window data for egui
    let focused_surface = state.seat.get_keyboard().and_then(|kb| kb.current_focus());
    let window_data: Vec<(usize, Point<i32, Physical>, Size<i32, Physical>, bool, smithay::backend::renderer::element::Id)> = state.windows.iter().enumerate().map(|(idx, w)| {
        let surface_size = w.toplevel.current_state().size.unwrap_or((800, 600).into());
        let surface_size_physical = Size::from((surface_size.w as i32, surface_size.h as i32));
        let is_focused = focused_surface.as_ref().map(|f| f == w.toplevel.wl_surface()).unwrap_or(false);
        (idx, w.location, surface_size_physical, is_focused, w.bar_id.clone())
    }).collect();

    // 3. Render to backend
    match &mut state.backend_data {
        BackendData::Drm { compositor, .. } => {
            let mut renderer = state.renderer.as_mut().ok_or_else(|| anyhow::anyhow!("No renderer"))?;
            
            let (elements, pending_close) = gather_elements(
                &mut state.egui_state, 
                &state.windows, 
                &window_data[..],
                &mut renderer, 
                output_size, 
                scale
            )?;

            if let Some(idx) = pending_close {
                state.windows[idx].toplevel.send_close();
            }

            if let Err(e) = compositor.render_frame::<GlowRenderer, CustomRenderElement>(&mut renderer, &elements, color, smithay::backend::drm::compositor::FrameFlags::empty()) {
                if format!("{:?}", e) != "EmptyFrame" {
                    error!("Rendering: render_frame failed: {:?}", e);
                }
            }
            if let Err(e) = compositor.commit_frame() {
                if format!("{:?}", e) != "EmptyFrame" {
                    error!("Rendering: commit_frame failed: {:?}", e);
                }
            }
        }
        BackendData::Winit { backend, damage_tracker } => {
            let buffer_age = backend.buffer_age().unwrap_or(0);
            
            let res = (|| -> Result<(Vec<CustomRenderElement>, Option<usize>)> {
                let (renderer, mut framebuffer) = backend.bind().map_err(|e| anyhow::anyhow!("Bind failed: {}", e))?;
                
                let (elements, pending_close) = gather_elements(
                    &mut state.egui_state, 
                    &state.windows, 
                    &window_data[..],
                    renderer, 
                    output_size, 
                    scale
                )?;

                if pending_close.is_none() {
                    damage_tracker.render_output(
                        renderer,
                        &mut framebuffer,
                        buffer_age,
                        &elements,
                        color,
                    ).map_err(|e| anyhow::anyhow!("Winit render failed: {}", e))?;
                }
                
                Ok((elements, pending_close))
            })();
            
            match res {
                Ok((_, pending_close_idx)) => {
                    if let Some(idx) = pending_close_idx {
                        state.windows[idx].toplevel.send_close();
                    }
                    if let Err(e) = backend.submit(None) {
                        error!("Winit Submit Error: {:?}", e);
                    }
                }
                Err(e) => {
                    error!("Winit Rendering Error: {:?}", e);
                }
            }
        }
        BackendData::None => {}
    }
    
    // 6. Send frame callbacks
    let time = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u32;
    for window in &state.windows {
        with_surface_tree_downward(window.toplevel.wl_surface(), (), |_, _, _| TraversalAction::DoChildren(()), |_, states, _| {
            let mut guard = states.cached_state.get::<SurfaceAttributes>();
            for callback in guard.current().frame_callbacks.drain(..) { 
                let c: smithay::reexports::wayland_server::protocol::wl_callback::WlCallback = callback;
                let _ = c.done(time); 
            }
        }, |_, _, _| true);
    }

    let _ = display.borrow_mut().flush_clients();
    state.needs_redraw = false;
    Ok(())
}

fn gather_elements(
    egui_state: &mut EguiState,
    windows: &[crate::state::Window],
    window_data: &[(usize, Point<i32, Physical>, smithay::utils::Size<i32, Physical>, bool, smithay::backend::renderer::element::Id)],
    renderer: &mut GlowRenderer,
    output_size: smithay::utils::Size<i32, Physical>,
    scale: f64,
) -> Result<(Vec<CustomRenderElement>, Option<usize>)> {
    let mut elements = Vec::new();
    let mut pending_close = None;

    let egui_element = egui_state.render(
        |ctx| {
            egui::Area::new(egui::Id::new("overlay"))
                .anchor(egui::Align2::LEFT_TOP, egui::vec2(0.0, 0.0))
                .show(ctx, |ui| {
                    ui.label(egui::RichText::new("Flora Compositor").color(egui::Color32::WHITE).size(20.0));

                    for (idx, pos, size, is_focused, bar_id) in window_data {
                        egui::Window::new(format!("window_{}", idx))
                            .id(egui::Id::new(bar_id))
                            .fixed_pos(egui::pos2(pos.x as f32, pos.y as f32))
                            .fixed_size(egui::vec2(size.w as f32, TITLE_BAR_HEIGHT as f32))
                            .title_bar(false)
                            .frame(egui::Frame::NONE.fill(egui::Color32::from_rgba_unmultiplied(20, 20, 20, 240)))
                            .show(ui.ctx(), |ui| {
                                ui.horizontal(|ui| {
                                    let btn_radius = 6.0;
                                    let spacing = 8.0;
                                    let start_x = 10.0;
                                    let center_y = (TITLE_BAR_HEIGHT as f32) / 2.0;

                                    for i in 0..3 {
                                        let color = match i {
                                            0 => egui::Color32::from_rgb(255, 95, 87),
                                            1 => egui::Color32::from_rgb(255, 189, 46),
                                            2 => egui::Color32::from_rgb(40, 201, 64),
                                            _ => egui::Color32::GRAY,
                                        };
                                        let center = egui::pos2(start_x + i as f32 * (btn_radius * 2.0 + spacing), center_y);
                                        ui.painter().circle_filled(center, btn_radius, color);

                                        let rect = egui::Rect::from_center_size(center, egui::vec2(btn_radius * 2.0, btn_radius * 2.0));
                                        let response = ui.interact(rect, egui::Id::new(("btn", bar_id, i)), egui::Sense::click());
                                        if response.clicked() && *is_focused && i == 0 {
                                            pending_close = Some(*idx);
                                        }
                                    }
                                });
                            });
                    }
                });
        },
        renderer,
        Rectangle::new((0, 0).into(), (output_size.w, output_size.h).into()),
        scale,
        scale as f32,
    );

    // 2. Gather elements
    match egui_element {
        Ok(egui_tex) => elements.push(CustomRenderElement::Egui(egui_tex)),
        Err(err) => error!("Failed to render egui overlay: {:?}", err),
    }

    for window in windows {
        let surface_location = Point::from((window.location.x, window.location.y + TITLE_BAR_HEIGHT));
        elements.extend(render_elements_from_surface_tree::<GlowRenderer, CustomRenderElement>(
            renderer, 
            window.toplevel.wl_surface(), 
            surface_location, 
            1.0, 1.0, 
            Kind::Unspecified
        ));
    }

    for window in windows {
        let surface_size = window.toplevel.current_state().size.unwrap_or((800, 600).into());
        let bar_rect = Rectangle::new(window.location, (surface_size.w, TITLE_BAR_HEIGHT).into());
        elements.push(CustomRenderElement::Solid(SolidColorRenderElement::new(
            window.bar_id.clone(),
            bar_rect,
            window.bar_commit_counter.clone(),
            [0.15, 0.15, 0.15, 1.0],
            Kind::Unspecified
        )));
    }

    Ok((elements, pending_close))
}
