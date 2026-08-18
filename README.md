# Wayfinder

Wayfinder is a local-first model router and native Apple AI environment. It
scores each request locally, chooses an eligible destination, and keeps delivery
policy separate from the application making the request.

The production router is implemented in Rust. Wayfinder Desktop embeds that
router inside the signed application bundle; no Python runtime, package, or
fallback is required.

## Products

### Wayfinder Desktop

The native Swift macOS app provides:

- conversation-first Chat with locally persisted history;
- automatic or pinned model selection;
- Apple Foundation Models delivery on eligible Apple Silicon Macs;
- opt-in ChatGPT account routing through a separately verified provider;
- OpenAI-compatible and Anthropic-compatible local gateway endpoints;
- native setup, connection, routing, privacy, and diagnostic surfaces.

Desktop releases use SemVer and `desktop-v*` tags. See
[`macos/WayfinderMac/Packaging/RELEASE.md`](macos/WayfinderMac/Packaging/RELEASE.md).

### Wayfinder for iPhone and iPad

Native standalone iPhone and iPad apps are planned for v0.2.0. They embed the
same authoritative Rust routing core and execute approved on-device or direct
cloud providers without requiring a Mac or localhost gateway. Optional Mac
pairing follows in v0.2.1 as an additional provider.

The governing contracts are
[`WF-ROADMAP-0016`](roadmaps/WF-ROADMAP-0016-native-mobile-v0.2.md),
[`WF-ADR-0047`](decisions/WF-ADR-0047-native-mobile-independence.md), and
[`WF-ADR-0048`](decisions/WF-ADR-0048-shared-routing-core-apple-embedding.md).
Mobile conversation persistence is governed by
[`WF-ADR-0049`](decisions/WF-ADR-0049-mobile-conversation-persistence.md).
The thread-first mobile interaction contract is
[`WF-DESIGN-0020`](designs/WF-DESIGN-0020-mobile-chat-shell.md).

### Rust gateway

The Rust workspace contains the deterministic scoring core, configuration
parser, provider clients, bounded HTTP gateway, service integration, native XPC
clients, and command-line helper.

Build it with:

```sh
cargo build \
  --manifest-path rust/Cargo.toml \
  --package wayfinder-cli \
  --bin wayfinder-router \
  --locked
```

Then run:

```sh
rust/target/debug/wayfinder-router route "Summarise this request"
rust/target/debug/wayfinder-router serve --host 127.0.0.1 --port 8088
```

Versioned `router-v*` releases also provide checksum-verified native Linux
archives for `x86_64` and `aarch64`. These contain the same independent Router
process used by the Omarchy integration; installing an archive does not create
or start a service or alter provider credentials.

For a network-exposed deployment, do not publish the local surface. Mint a
virtual key and select the fail-closed managed data plane:

```sh
rust/target/debug/wayfinder-router keys new --id team-a
rust/target/debug/wayfinder-router serve \
  --surface data-plane --host 0.0.0.0 --port 8088
```

The managed listener contains inference, authenticated model discovery, and
minimal `/livez`/`/readyz` probes only. See
[Managed gateway deployment](docs/managed-gateway-deployment.md).

The gateway exposes:

- OpenAI-compatible: `http://127.0.0.1:8088/v1`
- OpenAI Responses compatibility: `POST /v1/responses` (bounded text and multi-turn contract)
- Modality compatibility: explicit embeddings, image, audio, and batch capability contracts;
  non-text surfaces remain fail-closed until their reviewed adapters are enabled
- Anthropic-compatible: `http://127.0.0.1:8088`
- Health: `http://127.0.0.1:8088/healthz`

The scored decision remains offline, deterministic, and keyless. Credentials
are resolved only for delivery after the route is chosen.

## Container

```sh
docker build -t wayfinder-router .
docker run --rm -p 8088:8088 \
  --read-only --tmpfs /tmp --user 10001:10001 \
  -v "$PWD/config:/etc/wayfinder:ro" \
  -v wayfinder-state:/var/lib/wayfinder \
  wayfinder-router
```

The image is built from the Rust workspace and contains only the native gateway
plus its runtime certificates. It starts the authenticated managed data plane,
so `config/wayfinder-router.toml` must define at least one virtual key and model.
Configuration remains read-only; audit and savings state are written beneath
`/var/lib/wayfinder` by the unprivileged UID/GID `10001` process.

## Verification

```sh
cargo fmt --manifest-path rust/Cargo.toml --all -- --check
cargo test --manifest-path rust/Cargo.toml --workspace --all-features --locked
cargo clippy --manifest-path rust/Cargo.toml \
  --workspace --all-targets --all-features --locked -- -D warnings
swift test --package-path macos/WayfinderMac
node clients/shared/test/parity.mjs
```

## Repository map

```text
rust/                    native router, gateway, providers, and service crates
apple/                   planned shared Apple packages after bridge validation
ios/                     planned native iPhone and iPad product
macos/WayfinderMac/      native Swift macOS app and release packaging
clients/                 retained thin-client contract code and fixtures
decisions/               architecture decisions
designs/                 product and interaction contracts
roadmaps/                delivery plans and closeout records
docs/                    operational and release documentation
```

Wayfinder is licensed under Apache-2.0.
