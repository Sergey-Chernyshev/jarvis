use std::ffi::{CStr, OsString};
use std::io;
use std::os::fd::{AsFd, IntoRawFd};
use std::os::unix::ffi::OsStrExt;
use std::ptr::NonNull;

// Step A3.4 wires this already-probed wrapper into the production source walker.
#[cfg_attr(not(test), allow(dead_code))]
struct DirectoryStream(NonNull<libc::DIR>);

impl Drop for DirectoryStream {
    fn drop(&mut self) {
        unsafe {
            libc::closedir(self.0.as_ptr());
        }
    }
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn read_directory_names<Fd: AsFd>(directory: Fd) -> io::Result<Vec<OsString>> {
    let duplicate = rustix::io::fcntl_dupfd_cloexec(directory, 0)?;
    let duplicate_raw = duplicate.into_raw_fd();
    let stream = match NonNull::new(unsafe { libc::fdopendir(duplicate_raw) }) {
        Some(stream) => DirectoryStream(stream),
        None => {
            let error = io::Error::last_os_error();
            unsafe {
                libc::close(duplicate_raw);
            }
            return Err(error);
        }
    };

    let mut names = Vec::new();
    loop {
        unsafe {
            *libc::__error() = 0;
        }
        let entry = unsafe { libc::readdir(stream.0.as_ptr()) };
        if entry.is_null() {
            let errno = unsafe { *libc::__error() };
            if errno == 0 {
                return Ok(names);
            }
            return Err(io::Error::from_raw_os_error(errno));
        }

        let bytes = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if bytes != b"." && bytes != b".." {
            names.push(std::ffi::OsStr::from_bytes(bytes).to_os_string());
        }
    }
}
