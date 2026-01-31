pub mod state;
mod input;
mod backend;
mod decorations;

use smithay::{
    backend::{
        udev::{UdevBackend, UdevEvent},
        renderer::{
            glow::GlowRenderer,
            element::{Kind, surface::{render_elements_from_surface_tree, WaylandSurfaceRenderElement}, solid::SolidColorRenderElement, memory::MemoryRenderBufferRenderElement},
        },
        drm::DrmEvent,
    },
    reexports::{
        calloop::{EventLoop, Interest, Mode, PostAction, generic::Generic, channel::Event},
        wayland_server::Display,
    },
    utils::{SERIAL_COUNTER, Point, Physical},
    wayland::{
        compositor::{with_surface_tree_downward, TraversalAction, SurfaceAttributes},
    },
    input::keyboard::FilterResult,
};

use std::{time::Duration, rc::Rc, cell::RefCell, os::unix::io::{AsRawFd, BorrowedFd}};
use tracing::{info, warn, error};
use anyhow::Result;

use crate::state::{FloraState, FloraClientData, CompositorClientState, TITLE_BAR_HEIGHT, BUTTON_SIZE, BUTTON_SPACING, MARGIN};
use crate::input::{FloraInputEvent, spawn_input_thread};
use crate::backend::init_graphics;

smithay::backend::renderer::element::render_elements! {
    pub CustomRenderElement<=GlowRenderer>;
    Surface=WaylandSurfaceRenderElement<GlowRenderer>,
    Solid=SolidColorRenderElement,
    Memory=MemoryRenderBufferRenderElement<GlowRenderer>,
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
    if let Some(pointer) = state.seat.get_pointer() {
        let under = state.windows.iter().rev().find_map(|w| {
            let px = state.pointer_location.x.round() as i32;
            let py = state.pointer_location.y.round() as i32;
            let relative_x = px - w.location.x;
            let relative_y = py - w.location.y;
            
            let surface_size = w.toplevel.current_state().size.unwrap_or((800, 600).into());
            
            if relative_x >= 0 && relative_x < surface_size.w && 
               relative_y >= TITLE_BAR_HEIGHT && relative_y < (TITLE_BAR_HEIGHT + surface_size.h) {
                let local_x = relative_x;
                let local_y = relative_y - TITLE_BAR_HEIGHT;
                Some((w.toplevel.wl_surface().clone(), Point::<f64, smithay::utils::Logical>::from((local_x as f64, local_y as f64))))
            } else {
                None
            }
        });

        pointer.motion(state, under, &smithay::input::pointer::MotionEvent {
            location: state.pointer_location.to_logical(1.0),
            serial, time,
        });
    }
}

fn handle_pointer_button(state: &mut FloraState, button: u32, pressed: bool, time: u32) {
    let serial = SERIAL_COUNTER.next_serial();
    let state_enum = if pressed { smithay::backend::input::ButtonState::Pressed } else { smithay::backend::input::ButtonState::Released };
    
    if pressed {
        let hit = state.windows.iter().enumerate().rev().find_map(|(i, w)| {
            let px = state.pointer_location.x.round() as i32;
            let py = state.pointer_location.y.round() as i32;
            let relative_x = px - w.location.x;
            let relative_y = py - w.location.y;
            
            let surface_size = w.toplevel.current_state().size.unwrap_or((800, 600).into());
            
            if relative_x >= 0 && relative_x < surface_size.w && 
               relative_y >= 0 && relative_y < (TITLE_BAR_HEIGHT + surface_size.h) {
                
                // Button handling (Close)
                if relative_y < TITLE_BAR_HEIGHT {
                    let btn_y_start = (TITLE_BAR_HEIGHT - BUTTON_SIZE) / 2;
                    if relative_y >= btn_y_start && relative_y < (btn_y_start + BUTTON_SIZE) {
                        let red_x = MARGIN;
                        if relative_x >= red_x && relative_x < (red_x + BUTTON_SIZE) {
                            info!("🔴 TitleBar: Close clicked");
                            w.toplevel.send_close();
                        }
                    }
                }
                
                let w_loc_f = Point::<f64, Physical>::from((w.location.x as f64, w.location.y as f64));
                Some((i, state.pointer_location - w_loc_f))
            } else {
                None
            }
        });

        if let Some((idx, offset)) = hit {
            let surface = state.windows[idx].toplevel.wl_surface().clone();
            if let Some(keyboard) = state.seat.get_keyboard() {
                keyboard.set_focus(state, Some(surface), serial);
            }
            
            let py = state.pointer_location.y.round() as i32;
            let rel_y = py - state.windows[idx].location.y;

            let win = state.windows.remove(idx);
            state.windows.push(win);
            
            if rel_y < TITLE_BAR_HEIGHT {
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
        for window in &state.windows {
            let surface_size = window.toplevel.current_state().size.unwrap_or((800, 600).into());
            
            // Elements are rendered front-to-back (first element = top layer)
            // So we push: buttons first (top), then surface, then title bar bg (bottom)
            
            // 1. Draw Traffic Light Buttons (on top of everything) - using circle textures
            let button_render_size = BUTTON_SIZE;
            let btn_y = window.location.y + (TITLE_BAR_HEIGHT - button_render_size) / 2;
            
            // Red (Close)
            elements.push(CustomRenderElement::Memory(MemoryRenderBufferRenderElement::from_buffer(
                renderer,
                Point::<f64, Physical>::from((window.location.x as f64 + MARGIN as f64, btn_y as f64)),
                &state.red_button_buffer,
                None, // alpha
                None, // src_transform
                None, // size
                Kind::Unspecified,
            ).expect("Failed to create red button element")));
            
            // Yellow (Minimize)
            elements.push(CustomRenderElement::Memory(MemoryRenderBufferRenderElement::from_buffer(
                renderer,
                Point::<f64, Physical>::from((window.location.x as f64 + MARGIN as f64 + button_render_size as f64 + BUTTON_SPACING as f64, btn_y as f64)),
                &state.yellow_button_buffer,
                None,
                None,
                None,
                Kind::Unspecified,
            ).expect("Failed to create yellow button element")));
            
            // Green (Maximize)
            elements.push(CustomRenderElement::Memory(MemoryRenderBufferRenderElement::from_buffer(
                renderer,
                Point::<f64, Physical>::from((window.location.x as f64 + MARGIN as f64 + (button_render_size as f64 + BUTTON_SPACING as f64) * 2.0, btn_y as f64)),
                &state.green_button_buffer,
                None,
                None,
                None,
                Kind::Unspecified,
            ).expect("Failed to create green button element")));

            // 2. Draw Client Surface (shifted down by TITLE_BAR_HEIGHT)
            let surface_location = Point::from((window.location.x, window.location.y + TITLE_BAR_HEIGHT));
            elements.extend(render_elements_from_surface_tree::<GlowRenderer, CustomRenderElement>(
                renderer, 
                window.toplevel.wl_surface(), 
                surface_location, 
                1.0, 1.0, 
                Kind::Unspecified
            ));

            // 3. Draw Title Bar Background (Solid Gray) - at the back
            let bar_rect = smithay::utils::Rectangle::new(window.location, (surface_size.w, TITLE_BAR_HEIGHT).into());
            elements.push(CustomRenderElement::Solid(SolidColorRenderElement::new(
                window.bar_id.clone(),
                bar_rect,
                smithay::backend::renderer::utils::CommitCounter::default(),
                [0.15, 0.15, 0.15, 1.0],
                Kind::Unspecified
            )));
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
