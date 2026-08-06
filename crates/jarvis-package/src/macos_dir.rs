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
    read_directory_names_bounded(directory, u64::MAX, u64::MAX, 0)?
        .ok_or_else(|| io::Error::new(io::ErrorKind::OutOfMemory, "directory exceeds limits"))
}

pub(crate) fn read_directory_names_bounded<Fd: AsFd>(
    directory: Fd,
    max_entries: u64,
    max_stored_bytes: u64,
    allocation_charge: u64,
) -> io::Result<Option<Vec<OsString>>> {
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
    let mut stored_bytes = 0_u64;
    loop {
        unsafe {
            *libc::__error() = 0;
        }
        let entry = unsafe { libc::readdir(stream.0.as_ptr()) };
        if entry.is_null() {
            let errno = unsafe { *libc::__error() };
            if errno == 0 {
                return Ok(Some(names));
            }
            return Err(io::Error::from_raw_os_error(errno));
        }

        let bytes = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if bytes != b"." && bytes != b".." {
            let next_entries = u64::try_from(names.len())
                .ok()
                .and_then(|count| count.checked_add(1));
            let next_stored_bytes = u64::try_from(bytes.len())
                .ok()
                .and_then(|length| length.checked_add(allocation_charge))
                .and_then(|length| stored_bytes.checked_add(length));
            let (Some(next_entries), Some(next_stored_bytes)) = (next_entries, next_stored_bytes)
            else {
                return Ok(None);
            };
            if next_entries > max_entries || next_stored_bytes > max_stored_bytes {
                return Ok(None);
            }
            stored_bytes = next_stored_bytes;
            names.push(std::ffi::OsStr::from_bytes(bytes).to_os_string());
        }
    }
}
