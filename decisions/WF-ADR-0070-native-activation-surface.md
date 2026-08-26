---
schema_version: 1
id: WF-ADR-0070
type: decision
status: accepted
date: 2026-08-18
amended: 2026-08-26
tags: [activation, cli, local-policy, developer-tools]
---

# Provide a bounded native activation surface

## Context

Wayfinder already exposes local OpenAI- and Anthropic-compatible inference,
deterministic routing, budgets, privacy filters, audit receipts, and an operator
dashboard. A new command-line user must still assemble configuration and client
settings by hand. This hides the product behind its gateway implementation and
makes the first successful routed request harder than the routing policy itself.

WF-ADR-0046 removed the broad Python `init` and `doctor` commands. It permits a
reviewed native replacement, but not an implicit compatibility layer.

## Decision

Wayfinder describes the Router as local execution policy for AI. The HTTP
gateway is its compatibility surface, not its product category.

The Rust CLI owns four bounded activation commands:

1. `init` writes one gateway-owned preset to an explicit path or
   `./wayfinder-router.toml`. It defaults to the local-only preset, creates
   parents, and never overwrites a file. It does not start services or resolve
   credentials.
2. `doctor` reads and validates the selected policy with both authoritative
   parsers. It reports configured destination count, missing environment
   references by variable name, and loopback gateway reachability. It never
   prints credential values or calls a provider.
3. `connect` prints a client-specific configuration for Codex, Claude Code,
   OpenCode, Pi, or Aider. It writes no client file, accepts only an explicit loopback
   HTTP endpoint, and makes the proposed mutation reviewable before the user
   applies it. A printed placeholder token is local gateway configuration, not
   imported provider authentication.
4. `open` opens the fixed loopback operator dashboard. `--print` exposes the
   target without a side effect. It cannot open an arbitrary URL.

These are new native contracts. They do not reproduce the retired Python
workflow. Client recipes are documentation-shaped output and must remain
covered by contract tests and the public quick starts.

## Consequences

A developer can create a policy, identify missing requirements, connect an
existing coding client, and inspect routing decisions without learning the full
configuration schema. Existing hand-authored configurations and client files
remain untouched.

The first activation target is one local Router with at least two configured
destinations and ten routed requests within seven days. No telemetry is added
to measure this target. Operators may inspect their own local receipts.

Wayfinder does not add hosted identity, organization, billing, or fleet control
to the request path. A future control plane may distribute versioned policy and
read prompt-free receipts, but the Router continues to make every decision
locally.

## Related

- WF-ADR-0001 (standalone deterministic router)
- WF-ADR-0038 (local service surface)
- WF-ADR-0046 (Rust-only runtime)
- WF-ROADMAP-0006 (daily-use gateway habit)
- WF-ROADMAP-0013 (native first-run setup)
