// Keep one parity implementation and one corpus. The source file is marked
// `cfg(feature = "schema-parity")`; this harness declares that feature by
// default, while the public MSRV test-host deliberately leaves it disabled.
mod parity {
    include!("../../../crates/jarvis-plugin-test-host/tests/public_schema_parity.rs");
}
