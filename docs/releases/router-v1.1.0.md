# Wayfinder Router 1.1.0 for Linux

This release adds native OpenAI provider setup to the Rust CLI. Omarchy can
connect a provider, select a model, activate routing, verify a request, repair
interrupted setup, and disconnect without a separate Python helper.

## Assets

- `wayfinder-router-x86_64-unknown-linux-gnu.tar.gz`
- `wayfinder-router-aarch64-unknown-linux-gnu.tar.gz`
- one matching `.sha256` file for each archive

Each archive contains the native executable, Apache-2.0 `LICENSE`, and `NOTICE`.
The workflow builds and smoke-tests each executable on a matching Linux runner.
Installers must pin and verify the archive checksum before executing it.

## Native setup

`wayfinder-router setup` exposes status, discover, refresh-models, activate,
test, repair, and disconnect. Linux capabilities advertise setup schema 1.
API keys enter through stdin and stay in desktop Secret Service. Discovery does
not activate routing. Activation requires explicit model selection and preserves
custom policies. Interrupted changes retain a non-secret recovery journal.
A successful test requires response text and the exact successful Router receipt.

The command requires an unlocked Linux Secret Service keyring. It only replaces
the exact standard local starter or its own unchanged generated policy. Custom
policies and other provider setups retain their existing configuration workflow.
Installing this binary alone does not change policy or start a service.

The Rust workspace tests cover failure recovery, credential boundaries, bounded
subprocesses, cancellation with open stdin, and receipt verification through
real gateway handlers. Both Linux architectures are built and smoke-tested in
CI. These checks use controlled providers and credential fixtures; this release
does not claim a live OpenAI account, desktop login/keyring, or XPS reboot test.

The Omarchy plugin update is separate and must pin these exact archives before
it enables the new setup flow. Existing routing, streaming, coding-agent
connections, project profiles, and native min-cost calibration remain included.
See `docs/desktop-provider-setup.md` and WF-ADR-0088 for the command contract.
