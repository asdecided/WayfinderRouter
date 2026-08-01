# Wayfinder Rust workspace

Rust is Wayfinder's sole production router and gateway runtime under
WF-ADR-0046. This workspace contains:

- `wayfinder-routing-core` — authoritative deterministic feature extraction,
  compatibility filtering, scoring, tier selection, and route planning;
- `wayfinder-runtime-contracts` — secret-free requests, destination snapshots,
  exclusion reasons, plans, explanations, and receipts;
- `wayfinder-apple-ffi` — the bounded generated Swift bridge over the
  authoritative routing core;
- `wayfinder-config` — typed routing/gateway configuration and preserved
  mutations;
- `wayfinder-providers` — bounded provider clients and streaming translation;
- `wayfinder-gateway` — Axum data plane, policy, reliability, limits, metrics,
  and provider orchestration;
- `wayfinder-service` — service units, pricing/ledger logic, and secret seams;
- `wayfinder-cli` — the `wayfinder-router` executable;
- macOS XPC clients for credentials and Apple Foundation Models;
- checked fixtures retained as immutable migration evidence.

The runtime never launches or delegates to Python. Unsupported legacy commands
fail closed.

## Build and test

```sh
cargo fmt --manifest-path rust/Cargo.toml --all -- --check
cargo test --manifest-path rust/Cargo.toml --workspace --all-features --locked
cargo clippy --manifest-path rust/Cargo.toml \
  --workspace --all-targets --all-features --locked -- -D warnings
```

Build the executable:

```sh
cargo build \
  --manifest-path rust/Cargo.toml \
  --package wayfinder-cli \
  --bin wayfinder-router \
  --locked
```

## Apple-platform direction

Wayfinder Desktop continues to embed `wayfinder-router` as a separately running
signed helper. Native iPhone and iPad v0.2 does not run that executable or an
internal HTTP server.

WF-ADR-0048 defines the extracted `wayfinder-routing-core` and
`wayfinder-runtime-contracts` as the shared routing authority. The gateway
already consumes that core, and dependency-boundary plus golden-corpus tests
protect its portability. `wayfinder-apple-ffi` exposes bounded generated Swift
score-and-plan calls over that same core, and the Apple build scripts assemble
macOS, iOS-device, and iOS-simulator XCFramework slices. The pure core may not
perform filesystem, Keychain, process, provider, HTTP-server, UI, or
Apple-framework work.

Provider execution choice, the iOS shell, auth, Apple model, and pairing remain
separate pull requests under WF-ROADMAP-0016.

## Governing documents

- `decisions/WF-ADR-0046-rust-only-runtime.md`
- `decisions/WF-ADR-0048-shared-routing-core-apple-embedding.md`
- `roadmaps/WF-ROADMAP-0014-rust-gateway-migration.md`
- `roadmaps/WF-ROADMAP-0016-native-mobile-v0.2.md`
- `decisions/WF-ADR-0050-managed-gateway-surfaces.md`
- `decisions/WF-ADR-0051-bounded-delivery-concurrency.md`
- `docs/apple-platform-capability-matrix.md`
