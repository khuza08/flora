use smithay::{
    reexports::input::{
        Libinput, LibinputInterface, 
        event::{EventTrait, keyboard::{KeyboardEventTrait, KeyState}, pointer::{PointerEventTrait, ButtonState, PointerEvent}, device::DeviceEvent},
        Event as LibinputEvent,
    },
    utils::{Point, Physical},
};
use std::{os::unix::io::{OwnedFd, FromRawFd}, path::Path};
use tracing::{info, warn, error};

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

pub struct FloraLibinputInterface;

impl LibinputInterface for FloraLibinputInterface {
    fn open_restricted(&mut self, path: &Path, flags: i32) -> Result<OwnedFd, i32> {
        use std::os::unix::ffi::OsStrExt;
        match std::ffi::CString::new(path.as_os_str().as_bytes()) {
            Ok(c_path) => {
                let fd = unsafe { libc::open(c_path.as_ptr(), flags) };
                if fd < 0 {
                    let err = std::io::Error::last_os_error().raw_os_error().unwrap_or(libc::EIO);
                    warn!("Libinput: Failed to open {:?}: {}", path, err);
                    Err(err)
                } else {
                    info!("Libinput: Successfully opened {:?} (fd: {}, flags: {:x})", path, fd, flags);
                    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
                }
            }
            Err(_) => {
                error!("Libinput: Path contains NUL byte: {:?}", path);
                Err(libc::EINVAL)
            }
        }
    }

    fn close_restricted(&mut self, _fd: OwnedFd) {}
}

pub fn spawn_input_thread(input_sender: smithay::reexports::calloop::channel::Sender<FloraInputEvent>) {
    std::thread::spawn(move || {
        let mut libinput = Libinput::new_from_path(FloraLibinputInterface);
        info!("Input Thread: Started and scanning /dev/input/event* for unique devices...");
        if let Ok(entries) = std::fs::read_dir("/dev/input/") {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.starts_with("event") {
                    let path = format!("/dev/input/{}", name);
                    libinput.path_add_device(&path);
                }
            }
        }
        loop {
            if let Err(e) = libinput.dispatch() {
                error!("Input Thread: Libinput dispatch error: {:?}", e);
            }
            for event in &mut libinput {
                match event {
                    LibinputEvent::Device(DeviceEvent::Added(d)) => {
                        info!("Input Thread: Device Added: {:?}", d.device().name());
                    }
                    LibinputEvent::Keyboard(kb) => {
                        let _ = input_sender.send(FloraInputEvent::Keyboard { keycode: kb.key(), pressed: kb.key_state() == KeyState::Pressed, time: kb.time() as u32 });
                    }
                    LibinputEvent::Pointer(ptr) => {
                        match ptr {
                            PointerEvent::Motion(m) => {
                                let _ = input_sender.send(FloraInputEvent::PointerMotion { delta: (m.dx(), m.dy()).into(), time: m.time() as u32 });
                            }
                            PointerEvent::MotionAbsolute(m) => {
                                // Use transformed coordinates normalized to 0.0-1.0
                                let _ = input_sender.send(FloraInputEvent::PointerMotionAbsolute { 
                                    location: (m.absolute_x_transformed(1), m.absolute_y_transformed(1)).into(), 
                                    time: m.time() as u32 
                                });
                            }
                            PointerEvent::Button(b) => {
                                let _ = input_sender.send(FloraInputEvent::PointerButton { button: b.button(), pressed: b.button_state() == ButtonState::Pressed, time: b.time() as u32 });
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }
        }
    });
}
