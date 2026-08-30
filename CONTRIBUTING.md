# Contributing to Wayfinder

Wayfinder is a Rust gateway with a native Swift macOS application.

## Core invariant

The scored decision path is offline, deterministic, and keyless. It must not
call a model, touch the network, or resolve a credential. Network and credential
work belongs only in the delivery layer after the route is chosen
(WF-ADR-0001).

## Set up

Install Rust 1.85 or later. macOS app changes also require the supported Xcode
toolchain documented by the desktop package.

## Required verification

```sh
cargo fmt --manifest-path rust/Cargo.toml --all -- --check
cargo test --manifest-path rust/Cargo.toml --workspace --all-features --locked
cargo clippy --manifest-path rust/Cargo.toml \
  --workspace --all-targets --all-features --locked -- -D warnings
```

For native app changes:

```sh
swift test --package-path macos/WayfinderMac
```

For the retained JavaScript decision-preview contract:

```sh
node clients/shared/test/parity.mjs
```

## Commits and pull requests

Commit subjects follow Conventional Commits:

```text
type(scope): imperative summary
```

Use one lowercase scope and include a descriptive body explaining what changed
and why. Reference the relevant contract in a bracketed trailer such as
`[roadmap:WF-ROADMAP-0014]` or `[design:WF-DESIGN-0018]`.

Do not add AI attribution or bot co-author trailers.

Changes land through a `codex/*` branch and pull request. Never push directly to
the protected default branch.

Behavior changes require an architecture/design/roadmap record and an Unreleased
changelog entry. Pull requests are squash-merged after review and green checks.

## Releases

The portable Router and Wayfinder Desktop use independent SemVer release lines.
Router tags use `router-vMAJOR.MINOR.PATCH`; Desktop tags use
`desktop-vMAJOR.MINOR.PATCH`. A version change in one product does not imply a
version change in the other. The Desktop release includes the native Rust
router and follows
[`macos/WayfinderMac/Packaging/RELEASE.md`](macos/WayfinderMac/Packaging/RELEASE.md).
The retired package distribution is not a release or rollback channel.
