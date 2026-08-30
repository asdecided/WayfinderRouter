# Wayfinder Router 2026.8.2 for Linux

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

The Router remains an independent local process. Installing an executable does
not create or start a service, change routing policy, or add provider
credentials. The standalone Omarchy plugin will pin these exact assets only
after their checksums have been reviewed.

## Included Router changes

- Added a reviewable Pi connection recipe using Pi's official custom-provider
  configuration. The command prints JSON only, writes no client file, imports
  no credential, and documents exact reversal.
- Added a reviewable Aider connection recipe using Aider's official
  OpenAI-compatible endpoint contract. The command prints shell-local exports
  and an explicit invocation, writes no client file, imports no credential,
  and documents exact reversal.
- Added isolated hosted-provider configuration fragments for OpenAI, Gemini,
  OpenRouter, Groq, DeepSeek, Together, Fireworks, Cerebras, xAI, and Mistral.
  Each fragment requires an explicit model identifier and environment-variable
  reference; presets do not add routes, costs, capabilities, fallbacks, or
  Automatic eligibility.
- Added a native Anthropic Messages API destination with bounded buffered and
  streaming text, tool, usage, and error translation. Credentials are resolved
  only at delivery time, unsupported request fields fail closed, and merely
  configuring the destination does not change Automatic.
- Added an opt-in live-provider harness for the ten hosted presets and native
  Anthropic delivery. It checks buffered text and usage, streamed text and
  usage, terminal completion, forced tool calls, and the served-by receipt while
  keeping credentials and response content out of its evidence record.
- Added prompt-free per-project value reports for authenticated workspaces.
  The read-only `wf-project-value-v1` contract keeps durable accounted savings,
  bounded delivery outcomes, current baseline pricing, and unavailable
  correction evidence separate, with every window and denominator disclosed.
  It stores no prompt, response, repository path, tool argument, credential, or
  private reasoning and cannot activate policy or change Automatic routing.

The Pi client contract passed a real streaming tool round-trip, structured
upstream-error propagation, and disconnect cancellation through the Router.
The Aider client contract passed a real streamed file-edit round-trip plus
buffered, error, and cancellation checks through the Router. The native
Anthropic destination is covered by deterministic transport and translation
fixtures. The live-provider harness is disabled by default, and this release
does not claim a live hosted-provider account smoke.

All behavior from Router 2026.8.1 remains included, including project-aware
profiles, native `min-cost` calibration and calibrated deterministic routing,
daily delivery receipts, supported Codex, Claude Code, OpenCode, Pi, and Aider
connection surfaces, and native Omarchy service lifecycle contracts.
