#![deny(unsafe_code)]

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod macos_dir;

mod archive;
// Step A3.4 wires these pure package primitives into the fd-only source/spool pipeline.
#[cfg_attr(not(test), allow(dead_code))]
mod hash;
mod jcs;
#[cfg_attr(not(test), allow(dead_code))]
mod pack;
#[cfg(target_os = "macos")]
#[cfg_attr(not(test), allow(dead_code))]
mod source;
#[cfg(target_os = "macos")]
#[cfg_attr(not(test), allow(dead_code))]
mod spool;

#[cfg(target_os = "macos")]
pub use pack::pack_plugin;
pub use pack::{PackOptions, PackageDocumentAdapter, PackageError, PackageSignatureSource};

#[cfg(test)]
mod dependency_msrv;
