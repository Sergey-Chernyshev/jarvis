#![deny(unsafe_code)]

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod macos_dir;

mod jcs;

#[cfg(test)]
mod dependency_msrv;
