---
schema_version: 1
id: WF-ADR-0076
type: decision
status: accepted
date: 2026-08-23
tags: [omarchy, projects, profiles, credentials, rollback]
---

# Load project profiles from an owned local directory

## Context

WF-ADR-0075 connects authenticated workspaces to routing-only profiles, but
transparent TOML fragments are not a safe daily project lifecycle. Editing a
repository or the user's complete Router configuration would make setup
clobber-prone. Trusting a repository name supplied by an HTTP caller would make
profile selection spoofable.

The local integration also needs to preserve existing unkeyed loopback clients
when one repository gains a project capability. A configured project key must
not silently turn an existing Omarchy workstation into an externally reachable
unauthenticated gateway.

## Decision

1. `wayfinder-router project setup` discovers the canonical local Git root and
   resolves an exact `owner/name` or GitHub URL through the GitHub repository
   API. Missing, inaccessible, archived, malformed, and ambiguous repositories
   fail closed.
2. Generated state lives only under the XDG-owned Wayfinder project directory.
   Each repository root receives one private child directory containing an
   ownership-marked manifest and a routing-only profile. Setup never edits the
   repository or the user's main TOML.
3. The project capability is accepted only through
   `WAYFINDER_PROJECT_TOKEN` or an explicit stdin prompt. Only its SHA-256 hash
   enters the manifest. Tokens are not accepted as arguments, URLs, status
   fields, logs, or repository files.
4. The CLI merges valid owned manifests into the in-memory gateway
   configuration before policy construction. Collisions with user-managed
   profile, workspace, or key identifiers fail closed. Invalid project state
   participates in the existing last-known-good reload contract.
5. Missing authorization retains the global default only when owned project
   manifests are active and the listener is explicitly loopback. A presented
   invalid key still returns unauthorized. Project mode is rejected for a
   non-loopback listener.
6. Status exposes canonical repository, root, profile directory, ownership,
   profile modification, setup requirement, and token source without the
   token. Rollback verifies the ownership marker, renames the exact child
   directory out of service, removes it, and restores it if removal fails.

## Consequences

- Repository-specific routing can be added without changing unrelated
  repositories, the main Router policy, or existing unkeyed loopback clients.
- A project token remains an explicit local launch capability. The Omarchy
  surface may hold it only long enough to run setup or launch a supported
  agent; durable secret storage is outside the project directory.
- A profile file may be edited transparently. Its generated digest lets status
  distinguish the deterministic scaffold from a user-modified profile without
  treating modification as loss of ownership.
- The project directory is portable core behavior. Omarchy consumes the CLI
  contract but does not become a routing or identity authority.

## Related

- WF-ADR-0071 (versioned local policy lifecycle)
- WF-ADR-0075 (authenticated local project profiles)
- WF-ROADMAP-0017 (Omarchy-first project-aware routing)
