# Cargo macro identity adaptation

The package boundary fails closed on unaudited registry versions and checks the
locked archive checksum, physical source tree, macro provider, and its dependency
closure. This adaptation preserves those checks and all source/work budgets.

The integration base already selects Tokio 1.53.1. The extracted audit covered
1.52.3. Both registry archives were hashed locally and their macro sources
compared before adding the exact 1.53.1 identity:

| Version | Registry archive SHA-256 |
| --- | --- |
| 1.52.3 | `8fc7f01b389ac15039e4dc9531aa973a135d7a4135281b12d7c1bc79fd57fffe` |
| 1.53.1 | `202caea871b69668250d242070849eb495be178ed697a3e98aebce5bc81a0bed` |

`src/macros/join.rs` is byte-identical, SHA-256
`85d9d743e986ffad45800d34a26fab3389e32f44dcaf704d7a8c2ed4bfc27005`.
`src/macros/pin.rs` is byte-identical, SHA-256
`d8347f8258e8feccf56f132dadbfbe846d20fed3515e2211a4b41543177b2029`.
The only `select.rs` difference adds five documentation lines about cancellation
safety; its expansion code is unchanged. Its old/new file hashes are
`bb7e5dbcff7ac610de2abe43793558e2286a1e93d5737ad1cf4b67d90cb457af` and
`a1f6cb8d20a378653f2dda59d7137567b07896d76051c19e471812aab86e5229`.
The remaining macro-directory changes are internal cfg helpers: an additional
`s390x` taskdump target and schedule-latency feature gates. They do not change
any authorized exported macro. The root `tokio_macros` reexports are unchanged.

The selected `tokio-macros` provider remains 2.7.0 with archive checksum
`385a6cb71ab9ab790c5fe8d67f1645e6c450a7ce006a33de03daa956cf70a496`.
No version range, arbitrary local source, or path-only trust entry was added.

The node entry also uses `#[tokio::main]`, now authorized through that same exact
2.7.0 provider. Its `src/lib.rs` main/main_rt exports delegate to
`entry::main`; main_fail only emits a compile error. `entry.rs::main` parses the
annotated function and runtime options, then calls `parse_knobs` with
`is_test = false`. That function retains the parsed body and generates runtime
builder/block_on scaffolding. It performs no filesystem source discovery or
include expansion. The previously audited test attribute uses the same generator
with `is_test = true`. The source scanner still examines the original function
body and rejects untrusted nested expansion paths. No other attribute was added.

The only additional function-like names are `tokio::try_join` and `objc2::sel`.
`try_join.rs` is identical between 1.52.3 and 1.53.1, SHA-256
`ec788f1b490234d97ac5eb7831e4eed6f9ba00b1101c56553ce7511bbd2ca228`.
It generates tuple polling/error propagation and does not discover source files.
`objc2` stays pinned to the already audited 0.6.4 archive identity; its
`src/macros/mod.rs` hash is
`0385ab935ec5b30f948b43eff0988f05035d68b8ee4980a62bcb790c3b70bc5f`.
The selector grammar accepts identifiers/colons, delegates to string/cache or
static selector helpers, and performs no include expansion. The actual host
features use the non-static `CachedSel` branch. Nested source-expanding input
still fails the source scanner. These names do not authorize arbitrary macros
from either package. The main/test generator `tokio-macros/src/entry.rs` hash is
`4cead7e27e9352b4ca2126b81bf166eb89d8baf5604b26606d61322673acaebb`.
