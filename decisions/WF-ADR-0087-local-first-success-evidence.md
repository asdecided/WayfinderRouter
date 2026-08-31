---
schema_version: 1
id: WF-ADR-0087
type: decision
status: accepted
date: 2026-08-31
tags: [omarchy, onboarding, local, privacy, evidence]
---

# Prove a local inference before declaring first-run success

## Context

The native `local` starter preset names Ollama and `llama3.1`, but creating and parsing that file does not prove that either the runtime or model exists. An Omarchy surface can therefore report a configured service as ready while the first agent request will fail. Installing a runtime, pulling a model, scanning arbitrary ports, or choosing a discovered model without review would cross ownership, network, storage, and policy boundaries.

## Decision

1. `wayfinder-router local discover --json` queries only a fixed catalog of literal-loopback runtime endpoints for Ollama, LM Studio, llama.cpp, and vLLM. It emits `wf-local-discovery-v1` candidates containing only runtime name, loopback endpoint, fixed route ID, and public model ID. It does not install, pull, select, or write anything, and it does not scan the network.
2. `wayfinder-router init --preset local --endpoint URL --model MODEL` accepts one explicitly selected discovery result. The endpoint must remain literal loopback, the model ID is bounded, the generated policy is parse-validated, and the existing no-clobber create contract remains authoritative.
3. `wayfinder-router local probe --endpoint URL --model ROUTE_ID --json` sends one fixed, public, low-output request through an already running loopback Router. Success requires a non-empty normalized response plus the exact bounded receipt for that request with a terminal success and an `on-device` or `local-network` execution boundary.
4. The `wf-local-probe-v1` report includes endpoint, route, served destination, execution boundary, timestamp, request count, and the fixed-prompt disclosure. It never includes response text, prompt text beyond the public disclosure label, provider payloads, credentials, repository paths, tool arguments, or private reasoning.
5. Consumers may call setup complete only after the probe passes. An empty discovery, failed probe, missing receipt, hosted boundary, missing capability, or older Router remains visibly not ready.

## Consequences

- First run distinguishes “configuration exists” from “a local model actually answered.”
- Operators keep control of runtime installation, model downloads, and policy selection.
- Discovery intentionally misses runtimes on nonstandard ports until the operator supplies and reviews an explicit loopback endpoint.
- Hosted and account-backed first runs remain separate provider contracts; this decision does not import credentials or change Automatic routing.

## Related

- WF-ADR-0001 (offline deterministic decision path)
- WF-ADR-0068 (Omarchy plugin boundary)
- WF-ADR-0070 (native activation surface)
- WF-ADR-0073 (Omarchy-first portable core)
- WF-ADR-0086 (local runtime ownership and proof)
- WF-ROADMAP-0017 (Omarchy-first delivery)
