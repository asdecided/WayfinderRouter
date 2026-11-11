<!-- See CONTRIBUTING.md for the full conventions. -->

## Summary

<!-- What does this change, and why? A couple of sentences. -->

## Scope

<!-- The single area touched: gateway, cli, config, providers, pricing, service, calibrate, desktop, helm, ci, docs, … -->

## Verification

- [ ] `cargo +1.85.0 fmt --manifest-path rust/Cargo.toml --all -- --check`
- [ ] `cargo test --manifest-path rust/Cargo.toml --workspace --all-features --locked`
- [ ] `cargo +1.85.0 clippy --manifest-path rust/Cargo.toml --workspace --all-targets --all-features --locked -- -D warnings`

## Checklist

- [ ] Conventional, single-scope title (`type(scope): imperative summary`) with a descriptive body
- [ ] No AI attribution in the commits or this PR
- [ ] Behaviour change → an ADR/design/roadmap doc added with the **next free** number, plus a `CHANGELOG.md` `## Unreleased` entry
- [ ] The scored decision path stays offline, deterministic, and keyless (WF-ADR-0001)
