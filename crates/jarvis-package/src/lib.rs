#![deny(unsafe_code)]

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod macos_dir;

#[cfg(test)]
mod dependency_msrv;
