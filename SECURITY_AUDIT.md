# Dependency security audit exceptions

Reviewed: 2026-08-11

Serenya runs `cargo audit` as a fail-closed CI gate. These exceptions are narrowly scoped to
reviewed upstream constraints. Any new advisory still fails CI. Re-review an exception whenever its
upstream dependency line, target architecture, or referenced runtime path changes.

## Fixed rather than ignored

- `RUSTSEC-2024-0370` (`proc-macro-error`): removed from Serenya's selected dependency
  graph by disabling the unused rquickjs `macro` feature while retaining rquickjs 0.6.2.
  The application uses rquickjs runtime APIs rather than its procedural macros, so a
  major rquickjs API migration is not required to eliminate this unmaintained crate.

- `RUSTSEC-2026-0190` (`anyhow`): upgraded from 1.0.102 to patched 1.0.103. The
  advisory is classified as unsound and affects `Error::downcast_mut`; it must not be
  downgraded to an informational-only CI result.

- `RUSTSEC-2026-0221` (`event-listener`): upgraded from 5.4.1 to patched 5.4.2. The
  advisory is classified as unsound and can violate thread-safety/memory-safety; the
  current Moka/async-lock constraints accept the patched release.

- `spin` 0.9.8: upgraded to non-yanked 0.9.9. Flume's 0.9.8 caret requirement accepts
  0.9.9, so retaining a yanked lockfile version is not justified.

- `RUSTSEC-2026-0204` (`crossbeam-epoch`): upgraded from 0.9.18 to patched 0.9.20. The crate is
  active through Moka/Crossbeam, so an exception is not justified.

## Informational maintenance exceptions

These are `cargo audit` informational `unmaintained` warnings, not known exploitable
vulnerability advisories. They remain explicit exceptions so `[output].deny = ["warnings"]`
continues to fail closed for every new warning.

- `RUSTSEC-2024-0388` (`derivative` 2.2.0): latest stable Songbird 0.6.0 and Poise
  0.6.2 still depend directly on derivative 2, and vendored rusty-ytdl also uses it.
  RustSec lists no patched derivative release. Re-review when Songbird/Poise migrate
  away from derivative, or when Serenya replaces those dependency lines.
- `RUSTSEC-2024-0384` (`instant` 0.1.13): current Davey 0.1.4 selects OpenMLS 0.8.1
  with its `js` support, which retains `fluvio-wasm-timer -> instant`. RustSec lists
  no patched instant release. Re-review when Davey/OpenMLS removes that timer path.
- `RUSTSEC-2026-0210` (`libcrux-aesgcm` 0.0.7): the crate was renamed to
  `libcrux-aes`; this is informational maintenance status. The same crate is already
  verified locked-only in Serenya's selected workspace build. Re-review when the
  Davey/OpenMLS/HPKE stack migrates to the renamed libcrux packages.
- `RUSTSEC-2026-0173` (`proc-macro-error2` 2.0.1): pulled by hax-lib-macros in the
  libcrux/OpenMLS stack; the dependency is declared only for `cfg(hax)`. Current HAX
  releases still retain it. Re-review when the DAVE/libcrux stack or HAX removes it.

## Discord DAVE / hpke-rs 0.6.1

Dependency path: `songbird 0.6.0 -> davey 0.1.4 -> openmls_rust_crypto 0.5.1 -> hpke-rs 0.6.1`.

The validation script inspects the real Cargo feature graph. `hpke-rs-libcrux 0.6.1`,
`libcrux-aesgcm 0.0.7`, and `libcrux-chacha20poly1305 0.0.7` are present in `Cargo.lock` but are not
selected by Serenya's workspace build. Their advisories are therefore locked-only for this build:

- `RUSTSEC-2026-0209`
- `RUSTSEC-2026-0211`
- `RUSTSEC-2026-0124`

`libcrux-secrets 0.0.5` is selected transitively and is not classified as locked-only. Its reviewed
advisory, `RUSTSEC-2026-0212`, affects the AArch64 inline-assembly implementation of constant-time
swap/select. Serenya's current validation target is x86_64 Linux, and its auto-installer ships
amd64/win64 FFmpeg artifacts. The exception is valid only for that currently validated/distributed
x86_64 deployment. Adding ARM64/AArch64 support requires removing this exception or upgrading the
upstream DAVE/HPKE/libcrux stack first.

`libcrux-sha3 0.0.8` is also selected because hpke-rs 0.6.1 lists it unconditionally. Davey's DAVE
protocol v1 fixes the MLS ciphersuite to P-256 / AES-128-GCM / SHA-256 and instantiates
`OpenMlsRustCrypto`; it does not select SHAKE/SHA-3 for the DAVE ciphersuite. The affected functions
in the two advisories are SHAKE-specific APIs, so they are outside Serenya's DAVE execution path:

- `RUSTSEC-2026-0207`
- `RUSTSEC-2026-0208`

Re-review these exceptions when Songbird or Davey changes its OpenMLS/HPKE stack.

## Serenity 0.12 TLS compatibility line

Dependency path: `poise/serenity 0.12.5 -> tokio-tungstenite 0.21 -> tokio-rustls 0.25 ->
rustls 0.22.4 -> rustls-webpki 0.102.8`.

The patched webpki line is 0.103.x, while Serenity 0.12.5 remains tied to the 0.102-compatible TLS
stack. Serenya does not configure certificate revocation lists, so the CRL-only advisories are not
reachable through its TLS setup. The name-constraint advisories require a trusted CA to misissue a
certificate. Replacing Discord's TLS backend solely to remove this pinned branch would introduce a
new native-TLS/OpenSSL deployment path, so these are temporary upstream blockers:

- `RUSTSEC-2026-0049`
- `RUSTSEC-2026-0098`
- `RUSTSEC-2026-0099`
- `RUSTSEC-2026-0104`

Re-review them when Serenity/Poise publishes a compatible patched webpki line, or if Serenya changes
its TLS backend, trust store, revocation configuration, or target architecture.
