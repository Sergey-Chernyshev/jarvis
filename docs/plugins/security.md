# Plugin Security Boundary

Jarvis treats plugin UI, package bytes, catalog bytes, and native plugin code as
different security domains. A sandboxed plugin frame is untrusted. Catalog and
package verification run only in trusted host code, and only the host trust
adapter implements the private package verifier.

A capability grant controls access to Jarvis broker APIs. It is not an OS
sandbox for native code. Native activation therefore also requires consent for
the exact package digest; consent for one digest never authorizes another build,
version, publisher, or publisher lineage.

## Fail-closed catalog trust

The bundled `resources/plugin-trust-roots.json` contains public root material
only. An empty production root set is intentional: catalog-backed
install/update fails with `catalog_trust_not_provisioned` until release roots
are provisioned. Deterministic private keys live only under
`src-tauri/tests/fixtures/plugin-trust` and must never become release roots.

Catalog admission validates bounded closed JSON and its bundled schema before
making a trust decision. It then checks the issued/expiry interval, monotonic
sequence, previous digest, root signatures and quorum, root validity horizon,
publisher lineage, release compatibility, and revocation state. A lower
sequence is replay; the same sequence with another digest is conflict. State is
updated only after every check succeeds.

A root rotation must be authorized by both the currently accepted root quorum
and the proposed root quorum. Reusing one verifying key under different root
identities is rejected. The accepted root set changes atomically with the
accepted catalog, so an old-only, new-only, partially signed, expired, or
conflicting rotation cannot advance trust state.

Package-digest and publisher-key revocations are evaluated before a release is
exposed and again by the package verifier. A revoked digest yields
`package_revoked`; a revoked publisher key yields `publisher_key_revoked`.
Catalog expiry, key validity, and lineage binding remain fail-closed at package
verification time.

## Exact package-to-catalog equality

After a catalog release has passed structural and trust selection, the native
verifier compares exactly these ten verifier-reachable fields against the
held-file package observation:

| Exact field |
|---|
| `pluginId` |
| `publisher` |
| `version` |
| `target` |
| `minimumMacos` |
| `jarvisRange` |
| `pluginApi` |
| `archiveDigest` |
| `packageSignature.keyId` |
| `packageSignature.value` |

Changing any one of those ten fields produces the typed trust failure
`package_catalog_mismatch`. The private package engine preserves that exact
code and produces neither verified evidence nor extraction output.

Two related cases are deliberately rejected at earlier structural boundaries,
not added as an eleventh or twelfth equality field:

- `packageSignature.algorithm` is a closed schema/enum value. An unsupported
  algorithm is rejected as `catalog_schema` before a verifier can be built.
- `publisherLineage` must resolve while selecting the catalog release. An
  absent or unbound lineage is rejected as `publisher_lineage_invalid`.

Keep those two boundary failures separate from the exact ten-field verifier
contract; broadening the schema enum or the mismatch table to twelve would
weaken the layer that owns each rejection.

After equality succeeds, Ed25519 verification covers the exact canonical
`package.json` bytes with the package domain separator. A boolean, digest
string, reopened path, or serialized receipt cannot substitute for the opaque
same-file-descriptor verification evidence.

## Consent, installation, and execution

Verification is necessary but does not grant execution. The lifecycle manager
must obtain exact-digest consent before native health checks or activation.
Package directories become immutable before activation, and current receipts
bind the selected digest and lineage.

Developer Mode is an explicit local-source exception, not catalog trust. It
must show a native-code warning and require native consent again for every
different digest. Developer Mode must not promote fixture keys, bypass
revocation for catalog packages, or turn a prior grant into ambient native
authorization.

No native entry point, health check, service, or migration may run before the
package has passed structural inspection, current catalog trust and revocation
checks, exact package matching, signature verification, and the required
consent.
