# Plugin trust fixtures

`package-test-signing-seed.hex` is deterministic public test material from RFC 8032, not a credential.
It exists only to make package and catalog verification fixtures reproducible. Never use this seed,
its matching key, or catalogs signed by it as Jarvis production or release trust roots.

The independent `jarvis.root:2` test key in `catalog-seq-2-rotated.json` is reproducibly derived
from a 32-byte Ed25519 test seed whose every byte is `0x07`. That seed is public test material too;
it is intentionally described only in this fixture directory and must never be used for releases.
