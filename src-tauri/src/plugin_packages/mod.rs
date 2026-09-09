//! Durable package validation and lifecycle service. Runtime ownership stays in `plugin::PluginHost`.
// Stage A intentionally exposes service seams before runtime wiring.
#![allow(dead_code)]
pub mod developer;
pub mod manifest_v2;
pub mod package;
pub mod package_manager;
pub mod resolver;
pub mod trust;
pub mod verified_exec;
