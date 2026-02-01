pub mod compositor;
pub mod shell;
pub mod input;
mod backend;

use smithay::{
    backend::{
        udev::{UdevBackend, UdevEvent},
        drm::DrmEvent,
        input::{KeyState, ButtonState, KeyboardKeyEvent, PointerMotionEvent, PointerButtonEvent, Event as InputEventTrait, AbsolutePositionEvent},
        libinput::LibinputInputBackend,
    },
    reexports::{
        calloop::{EventLoop, Interest, Mode, PostAction, generic::Generic},
        wayland_server::Display,
        input::{Libinput},
    },
    utils::{SERIAL_COUNTER},
};

#[cfg(feature = "winit")]
use smithay::backend::winit::{WinitEvent, WinitInput};

use std::{time::Duration, rc::Rc, cell::RefCell, os::unix::io::{AsRawFd, BorrowedFd}};
use tracing::{info, warn, error};
use anyhow::Result;

use crate::compositor::state::{FloraState, FloraClientData, BackendData};
pub use smithay::wayland::compositor::CompositorClientState;
use crate::compositor::render::render_frame;
use crate::input::FloraInputEvent;
use crate::input::handler::handle_input_event;
use crate::backend::drm::init_drm_graphics;

#[cfg(feature = "winit")]
use crate::backend::winit::init_winit_graphics;

fn main() -> Result<()> {
    tracing_subscriber::fmt().init();
    info!("Flora: Starting macOS-like Compositor...");

    // 1. Setup Event Loop
    let mut event_loop: EventLoop<FloraState> = EventLoop::try_new()?;
    let handle = event_loop.handle();

    // 2. Setup Wayland Display
    let mut display_raw = Display::new()?;
    let poll_fd = display_raw.backend().poll_fd().as_raw_fd();
    let dh = display_raw.handle();
    let display = Rc::new(RefCell::new(display_raw));

    // 3. Initialize State
    let mut state = FloraState::new(&dh);
    
    // 4. Setup Wayland Socket
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

    // 5. Setup Display Event Source
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

    // 6. Backend Initialization
    #[allow(unused_mut)]
    let mut use_winit = true;
    #[cfg(not(feature = "winit"))]
    { use_winit = false; }

    if use_winit {
        #[cfg(feature = "winit")]
        match init_winit_graphics() {
            Ok((backend, winit_event_loop, output)) => {
                output.create_global::<FloraState>(&state.display_handle);
                state.output = Some(output.clone());
                state.backend_data = BackendData::Winit { 
                    backend,
                    damage_tracker: smithay::backend::renderer::damage::OutputDamageTracker::from_output(&output),
                };
                state.needs_redraw = true;
                info!("Winit (Nested) initialized successfully!");

                handle.insert_source(winit_event_loop, |event, _, state| {
                    match event {
                        WinitEvent::Resized { size, .. } => {
                            if let Some(output) = state.output.as_ref() {
                                let mode = smithay::output::Mode { size, refresh: 60_000 };
                                output.change_current_state(Some(mode), None, None, None);
                            }
                            state.needs_redraw = true;
                        }
                        WinitEvent::Input(input_event) => {
                            match input_event {
                                smithay::backend::input::InputEvent::Keyboard { event } => {
                                    handle_input_event(state, FloraInputEvent::Keyboard { 
                                        keycode: KeyboardKeyEvent::<WinitInput>::key_code(&event).into(), 
                                        pressed: KeyboardKeyEvent::<WinitInput>::state(&event) == KeyState::Pressed, 
                                        time: InputEventTrait::<WinitInput>::time(&event) as u32 
                                    });
                                }
                                smithay::backend::input::InputEvent::PointerMotion { event } => {
                                    let scale = crate::compositor::render::get_output_scale(state);
                                    handle_input_event(state, FloraInputEvent::PointerMotion { 
                                        delta: PointerMotionEvent::<WinitInput>::delta(&event).to_physical(scale), 
                                        time: InputEventTrait::<WinitInput>::time(&event) as u32 
                                    });
                                }
                                smithay::backend::input::InputEvent::PointerButton { event } => {
                                    handle_input_event(state, FloraInputEvent::PointerButton { 
                                        button: PointerButtonEvent::<WinitInput>::button_code(&event), 
                                        pressed: PointerButtonEvent::<WinitInput>::state(&event) == ButtonState::Pressed, 
                                        time: InputEventTrait::<WinitInput>::time(&event) as u32 
                                    });
                                }
                                _ => {}
                            }
                            state.needs_redraw = true;
                        }
                        WinitEvent::Redraw => { state.needs_redraw = true; }
                        WinitEvent::CloseRequested => { state.should_stop = true; }
                        WinitEvent::Focus(gained) => {
                            if gained {
                                if let Some(keyboard) = state.seat.get_keyboard() {
                                    for keycode in keyboard.pressed_keys() {
                                        keyboard.input::<(), _>(state, keycode, smithay::backend::input::KeyState::Released, SERIAL_COUNTER.next_serial(), 0, |_, _, _| smithay::input::keyboard::FilterResult::Forward);
                                    }
                                }
                            }
                        }
                    }
                }).expect("Failed to insert Winit event loop");
            }
            Err(e) => { error!("Failed to initialize Winit backend: {}", e); }
        }
    }
    
    if !use_winit {
        // DRM / Udev setup
        let _tty_file = setup_tty_graphics()?;
        let udev = UdevBackend::new("seat0")?;
        for (_device_id, dev_path) in udev.device_list() {
            if dev_path.to_string_lossy().contains("card") || dev_path.to_string_lossy().contains("render") {
                state.drm_devices.push(dev_path.to_path_buf());
            }
        }
        handle.insert_source(udev, |event, _, state| {
            match event {
                UdevEvent::Added { path, .. } => {
                    if path.to_string_lossy().contains("card") || path.to_string_lossy().contains("render") {
                        state.drm_devices.push(path);
                    }
                }
                _ => {}
            }
        }).map_err(|_| anyhow::anyhow!("Failed to insert udev source"))?;
        
        // Setup Libinput as a Fallback Input Source
        let mut libinput_context = Libinput::new_from_path(crate::input::libinput::FloraLibinputInterface);
        if let Ok(entries) = std::fs::read_dir("/dev/input/") {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.starts_with("event") {
                    let path = format!("/dev/input/{}", name);
                    libinput_context.path_add_device(&path);
                }
            }
        }
        
        let libinput = LibinputInputBackend::new(libinput_context);
        
        handle.insert_source(libinput, |event, _, state| {
            use smithay::backend::input::InputEvent;
            let scale = crate::compositor::render::get_output_scale(state);
            match event {
                InputEvent::DeviceAdded { device } => {
                    info!("Libinput Device Added: {:?}", device.name());
                }
                InputEvent::Keyboard { event } => {
                    handle_input_event(state, FloraInputEvent::Keyboard { 
                        keycode: event.key_code().into(), 
                        pressed: event.state() == KeyState::Pressed, 
                        time: InputEventTrait::time(&event) as u32 
                    });
                }
                InputEvent::PointerMotion { event } => {
                    handle_input_event(state, FloraInputEvent::PointerMotion { 
                        delta: event.delta().to_physical(scale), 
                        time: InputEventTrait::time(&event) as u32 
                    });
                }
                InputEvent::PointerMotionAbsolute { event } => {
                    handle_input_event(state, FloraInputEvent::PointerMotionAbsolute { 
                        location: (event.x_transformed(1), event.y_transformed(1)).into(), 
                        time: InputEventTrait::time(&event) as u32 
                    });
                }
                InputEvent::PointerButton { event } => {
                    handle_input_event(state, FloraInputEvent::PointerButton { 
                        button: event.button_code(), 
                        pressed: event.state() == ButtonState::Pressed, 
                        time: InputEventTrait::time(&event) as u32 
                    });
                }
                _ => {}
            }
        }).expect("Failed to insert libinput source");
    }

    // 7. Main Loop
    while !state.should_stop {
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
                    state.output = Some(output.clone());
                    output.create_global::<FloraState>(&state.display_handle);
                    state.backend_data = BackendData::Drm { gbm, egl, compositor, device: drm_device };
                    state.needs_redraw = true;
                    info!("DRM Backend initialized successfully!");
                }
                Err(e) => { error!("Failed to initialize DRM backend: {}", e); }
            }
        }

        if let Err(e) = event_loop.dispatch(Duration::from_millis(16), &mut state) {
            error!("Event loop error: {:?}", e);
            break;
        }

        if state.needs_redraw {
            let _ = render_frame(&mut state, &display);
            state.needs_redraw = false;
        }
    }

    info!("Flora: Shutting down...");
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
