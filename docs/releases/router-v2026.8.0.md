# Wayfinder Router 2026.8.0 for Linux

This release publishes the native Rust `wayfinder-router` gateway for direct
installation on glibc-based Linux systems, including Omarchy.

## Assets

- `wayfinder-router-x86_64-unknown-linux-gnu.tar.gz`
- `wayfinder-router-aarch64-unknown-linux-gnu.tar.gz`
- one matching `.sha256` file for each archive

Each archive contains the Router executable plus its Apache-2.0 `LICENSE` and
`NOTICE`. The release workflow builds and smoke-tests each executable on a
matching native GitHub runner. Downstream installers must pin this release and
verify the reviewed SHA-256 digest before extracting or executing the binary.

The Router remains an independent process. Installing an executable does not
create or start a service, change routing configuration, or add provider
credentials.

## Included Router change

This release includes native price-sensitive `min-cost` threshold calibration.
Operators can fit a deterministic routing cut from labelled JSONL and explicit
arm costs. The command stays offline and model-free. Thanks to
[@doctatortot](https://github.com/doctatortot) for raising the use case and
working through the contract in issue 170.
