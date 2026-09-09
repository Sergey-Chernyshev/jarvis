# Plugin package foundations

This checkout contains non-activating package foundations for #78: strict
manifest/archive validation, signature and catalog verification, durable receipts,
atomic manager operations, immutable developer snapshots, and public protocol/SDK
crates. It does not yet expose a package CLI or connect these services to the
singular runtime host. Existing protocol-1 external discovery is unchanged by
this foundations commit; it is not a v2 package admission path.

Run the public SDK fixture tests without installing a plugin:

```sh
cargo test --locked --manifest-path crates/jarvis-plugin-sdk/Cargo.toml
cargo test --locked --manifest-path crates/jarvis-plugin-test-host/Cargo.toml
cargo test --locked --manifest-path crates/jarvis-package/Cargo.toml
```

The [manifest guide](manifest.md) and repository fixtures describe the closed
v2 contract. Native manifests require a launchd user service and a separate
bridge; the foundations do not reinterpret that lifecycle as a child process.
UI schemas and generated TypeScript are contract collateral, not a UI host.

The durable manager is exercised with temporary roots and injected lifecycle
ports. Developer snapshot tests require exact native digest consent, revoke
active developer generations before disabling mode, and preserve owned data.
These are tested service contracts; user-facing link/install/enable commands
remain part of the later runtime integration.

Production trust roots are deliberately empty. Fixture keys are test inputs
only; no signed catalog is admitted by default. Package archives produced for
development do not become trusted publisher releases.
