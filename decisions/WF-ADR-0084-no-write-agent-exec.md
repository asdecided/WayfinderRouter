---
schema_version: 1
id: WF-ADR-0084
type: decision
status: accepted
date: 2026-08-30
tags: [omarchy, cli, coding-agents, activation, no-clobber]
---

# Launch supported coding agents without changing their configuration

## Context

Omarchy has one explicit default-agent launcher, while Wayfinder chooses the
model and execution boundary behind an agent. Making Wayfinder selectable at
that launcher boundary must not make Wayfinder a competing agent, import an
agent's authentication, or silently rewrite per-client configuration.

The existing `connect` command prints reviewable recipes, but the Omarchy
launcher needs a bounded process contract. A missing or unhealthy Router must
also remain visible: silently running the agent against its previous provider
would make the selected routing mode untruthful.

The supported clients expose different launch-time controls. Codex accepts
ephemeral configuration overrides, Claude Code accepts endpoint/model/token
environment overrides, and OpenCode accepts inline configuration content. Pi
currently has provider, model, and token arguments but no verified no-write
custom-endpoint override.

## Decision

The Router provides this process-owned command:

```text
wayfinder-router exec codex|claude-code|opencode \
  [--endpoint http://LOOPBACK:PORT] -- PROGRAM [ARG ...]
```

The default endpoint is `http://127.0.0.1:8088`. An override must be an
explicit HTTP loopback origin with a port and no credentials, path, query, or
fragment. The command requires an argv separator and launches the selected
client executable directly without a shell. It rejects a mismatched program
name and child options that could replace the injected provider or model.

Before launch, the command performs three bounded, redirect-free probes. The
probe client ignores process proxy settings so a loopback URL is connected to
directly rather than delegated to an ambient HTTP proxy:

1. `/healthz` must have a recognized status and at least one configured model
   not listed as missing its key;
2. `/v1/models` must advertise the `auto` model as owned by Wayfinder; and
3. the selected client's required POST route must exist: `/v1/responses` for
   Codex, `/v1/messages` for Claude Code, or `/v1/chat/completions` for
   OpenCode.

Probe bodies are size-limited and time-bounded. No probe calls a provider or
submits a prompt.

After successful probes, the Router applies only these process-local values:

- Codex receives `auto`, a temporary `wayfinder` Responses provider pointed at
  the loopback `/v1` origin, and `WAYFINDER_LOCAL_TOKEN=wayfinder-local`;
- Claude Code receives the loopback Anthropic base URL, `auto`, its gateway
  discovery switch, and the same placeholder local token; and
- OpenCode receives an inline `wayfinder/auto` OpenAI-compatible provider with
  the loopback `/v1` origin and placeholder local token.

The Router does not read or write client config, auth stores, API keys, shell
files, or repository files. The placeholder authenticates no upstream
provider. Existing process environment still belongs to the launched client,
but the selected provider's request path uses only the injected local values.

On Unix the Router replaces itself with the client so terminal signals and exit
behavior remain native. Other supported platforms wait for the child and
return its exit code. Any parse, readiness, capability, or launch failure exits
nonzero with a visible error and never runs the original command directly.

`wayfinder-router capabilities --json` advertises the versioned
`wf-agent-exec-v1` contract, exact supported clients, probes, no-write posture,
and absence of fallback.

Pi is excluded from this contract until it exposes a verified launch-time
custom endpoint. `exec pi` fails before launch and points to the reviewable
`connect pi` recipe. A later transactional connect path may configure Pi with
an explicit diff, confirmation, ownership markers, and rollback; this command
will not approximate that by reading or mutating `models.json`.

## Consequences

- Omarchy can make routing an explicit launcher mode without taking ownership
  of the user's agent selection or configuration.
- A selected Wayfinder mode cannot degrade silently to a direct provider when
  the Router is missing, unhealthy, or incompatible.
- The machine-readable capability handshake lets integrations enable only the
  clients the installed Router can launch honestly.
- Pi and additional Omarchy agents remain direct until each has a verified
  no-write launch surface or a separate transactional connection contract.
- Client CLI changes can break a launch adapter, so each supported version
  still needs repeatable end-to-end verification before release claims expand.

## Related

- WF-ADR-0001 (standalone deterministic Router)
- WF-ADR-0070 (bounded native activation surface)
- WF-ADR-0073 (Omarchy-first portable core)
- WF-ROADMAP-0017 (Omarchy-first delivery)
