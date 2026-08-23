---
schema_version: 1
id: WF-ADR-0075
type: decision
status: accepted
date: 2026-08-23
tags: [omarchy, projects, profiles, authentication, local-policy]
---

# Bind local project profiles through authenticated launch keys

## Context

Wayfinder already has immutable routing profiles, deterministic workspace
bindings, authenticated virtual keys, content-addressed policy versions,
last-known-good configuration reload, and explicit receipt identities. The
ordinary local configuration does not connect those contracts, so every coding
agent still uses the global routing profile.

The Router cannot safely infer a repository from prompt content, an arbitrary
HTTP header, or the current working directory of the independently supervised
service. Two agents in different repositories also need to route concurrently
through the same loopback gateway.

## Decision

1. Local gateway configuration may define at most 64 named, routing-only
   profiles under `gateway.profiles`. The reserved `default` profile remains the
   user's top-level `[routing]` configuration.
2. A configured workspace may name one profile. Only a successfully
   authenticated virtual key can supply that workspace identity to the request
   path; no project, repository, client, or workspace assertion is accepted
   from a public request header.
3. Project launch integration mints one local key, stores only its hash in the
   Router configuration, binds that key to one workspace, and injects the
   plaintext token through the reviewed agent launch environment. Selecting the
   correct key from a trusted canonical repository root belongs to the CLI and
   Omarchy surface, not the Router request path.
4. The configured top-level route is installed as the immutable default
   profile. Named profiles and workspace bindings form one content-addressed
   `wf-policy-v1` document. Each successful startup or hot reload receives a
   new snapshot identity; invalid updates retain the existing last-known-good
   `AppState`.
5. A profile may change scoring weights and tiers only. Global destination
   configuration, workspace/key model allowlists, modality support, provider
   readiness, privacy posture, budgets, and rate limits remain independent
   eligibility gates and cannot be broadened by a project profile.
6. Receipts continue to expose the resolved profile, immutable policy version,
   and activation snapshot without storing a repository path or token.

## Consequences

- Two repositories can select different scoring profiles concurrently through
  one local Router, while an unbound authenticated key continues to use the
  global default.
- Copying a project token is an explicit capability transfer. It does not cause
  the Router to trust a caller-asserted path, and provider credentials remain
  separate.
- The first core slice intentionally exposes transparent low-level TOML. A
  following CLI/Omarchy slice must own canonical repository discovery,
  no-clobber project setup, token handling, last-known-good status, and explicit
  rollback before the project-aware roadmap item is complete.

## Related

- WF-ADR-0035 (virtual gateway keys)
- WF-ADR-0052 (workspace-scoped model policy)
- WF-ADR-0071 (versioned policy lifecycle)
- WF-ADR-0073 (Omarchy-first portable core)
- WF-ROADMAP-0017 (project-aware routing)
