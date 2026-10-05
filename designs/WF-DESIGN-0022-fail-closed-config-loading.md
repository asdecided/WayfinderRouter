---
schema_version: 1
id: WF-DESIGN-0022
type: design
---
# Fail-closed configuration selection

## Status

Proposed

## Problem

Routing and initial server loading treated an explicitly selected missing file
as absent optional configuration. Discovery also skipped directories and broken
symlinks. That can replace intended routing policy with binary defaults or a
parent config. Gateway startup may fail later for missing models, but it must
fail at the selected policy boundary instead of depending on that side effect.

## Design

Explicit CLI/environment selection is required policy. Missing, invalid or
unreadable selected configuration is an error. Discovery retains existing
unusable paths and errors rather than searching for a more permissive ancestor.
Only absence during optional discovery permits documented binary defaults.
Read failures identify path and corrective action without source contents;
runtime TOML/schema errors omit configured values. Successful parsing represents
Loaded; optional absence and explicit Missing represent Absent; parser/schema
errors represent Invalid; read and non-file errors represent Unreadable.

The same source reader serves routing and gateway startup. Valid symlinks remain
supported. Config files and their directories must be protected from untrusted
writes; this is not an atomic filesystem sandbox. Existing activation and reload
retain the last known-good immutable snapshot on failure (WF-ADR-0071), not an
unrestricted default. Requests already using a snapshot retain that snapshot.
No cached approval or authority is created by this change.

## Enforcement audit (5 October 2026)

Primary source: https://github.com/anthropics/claude-code/releases
Claude Code v2.1.285 adds allowedProviders and describes warning/continuation for
OS-denied managed-settings reads. v2.1.289 fixes compound-command approval,
symlink read denial, managed MCP metadata rewriting and shell-variable cases.
These are host-agent findings, not evidence of matching Wayfinder defects.

Confirmed here: explicit-missing/default substitution and skipped unusable
config discovery. Preventive regression: provider delivery restrictions continue
through failover. Wayfinder uses configured model-alias allowlists, destination
capability/privacy checks and offline locality checks, not Claude's
allowedProviders setting. Provider protocol kind is not provider identity: many
hosts use OpenAI-compatible transport. Operators restricting a provider must
restrict its configured aliases and control their deployment endpoints.
The delivery-plan predicate intersects fallback aliases with the authenticated
key's allowed models and destination checks. A regression forces primary failure
and proves a forbidden backup transport is never invoked. Existing named-route,
streaming offline and live-cache identity tests cover adjacent boundaries.

Shell approvals/expansion, IDE symlink access and third-party MCP description
integrity belong to the host agent. Wayfinder does not execute shell tool calls,
implement IDE read permissions or approve arbitrary MCP tool metadata. Updating
Wayfinder does not repair the host; use its fixed release independently.

## Related

- WF-ADR-0001
- WF-ADR-0031
- WF-ADR-0071
