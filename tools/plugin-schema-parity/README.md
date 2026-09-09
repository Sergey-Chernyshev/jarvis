# Plugin schema parity harness

This crate runs the real Draft 7 JSON Schema validator against the shared
public-contract corpus. It intentionally uses the current stable toolchain:
`jsonschema` 0.18.3 resolves a URL/ICU dependency graph whose current lock
entries require newer Rust than Jarvis's public-crate MSRV of 1.77.2.

The public protocol and fake host do not depend on this crate. Their canonical
runtime enforcement remains Rust serde validation. The generated schemas add
`x-maxUtf8Bytes` and `x-maxJsonBytes` metadata; generic Draft 7 validators may
ignore those extension keywords, so this harness registers both keywords and
tests that behavior explicitly.

Run from the repository root:

```bash
cargo test --locked --manifest-path tools/plugin-schema-parity/Cargo.toml
```
