# Plugin trust fixtures

`package-test-signing-seed.hex` is deterministic public test material from RFC 8032, not a credential.
It exists only to make package and catalog verification fixtures reproducible. Never use this seed,
its matching key, or catalogs signed by it as Jarvis production or release trust roots.
