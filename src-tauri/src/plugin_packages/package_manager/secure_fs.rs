use std::ffi::CStr;
use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;

pub(crate) fn metadata(file: &File) -> io::Result<libc::stat> {
    let mut metadata = std::mem::MaybeUninit::<libc::stat>::zeroed();
    if unsafe { libc::fstat(file.as_raw_fd(), metadata.as_mut_ptr()) } == 0 {
        Ok(unsafe { metadata.assume_init() })
    } else {
        Err(io::Error::last_os_error())
    }
}

pub(crate) fn entry_metadata(parent: &File, name: &CStr) -> io::Result<libc::stat> {
    let mut metadata = std::mem::MaybeUninit::<libc::stat>::zeroed();
    if unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            name.as_ptr(),
            metadata.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } == 0
    {
        Ok(unsafe { metadata.assume_init() })
    } else {
        Err(io::Error::last_os_error())
    }
}

pub(crate) fn chmod(file: &File, mode: u32) -> io::Result<()> {
    if unsafe { libc::fchmod(file.as_raw_fd(), mode as libc::mode_t) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

pub(crate) fn is_type(metadata: &libc::stat, file_type: libc::mode_t) -> bool {
    metadata.st_mode & libc::S_IFMT == file_type
}

pub(crate) fn same_identity(left: &libc::stat, right: &libc::stat) -> bool {
    left.st_dev == right.st_dev && left.st_ino == right.st_ino
}

pub(crate) fn owned_by_effective_user(metadata: &libc::stat) -> bool {
    metadata.st_uid == unsafe { libc::geteuid() }
}

pub(crate) fn has_single_link(metadata: &libc::stat) -> bool {
    metadata.st_nlink == 1
}
