use smithay::reexports::input::LibinputInterface;
use std::{os::unix::io::{OwnedFd, FromRawFd}, path::Path};
use tracing::{info, warn, error};

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
