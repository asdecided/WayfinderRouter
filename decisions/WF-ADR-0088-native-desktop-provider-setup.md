---
schema_version: 1
id: WF-ADR-0088
type: decision
status: accepted
date: 2026-09-06
tags: [rust, omarchy, onboarding, credentials, lifecycle]
---

# Own desktop provider setup in the Rust CLI

## Context

The Omarchy first-run flow started a local service without connecting a usable
provider. An initial plugin PR implemented hosted setup in Python. That added a
second runtime and duplicated ownership of credential and policy lifecycle.
The operator requested replacing it with Rust before shipping.

## Decision

`wayfinder-router setup` owns the bounded Linux OpenAI setup workflow. QML is a
presentation and explicit-action client. The command emits secret-free JSON
with `schema_version: 1`; capabilities advertise `setup_schema_version: 1` only
on Linux. Older binaries must leave guided provider controls unavailable.

Actions are status, discover, refresh-models, activate, test, repair, disconnect.
Discovery reads a key through stdin, queries the fixed official OpenAI catalog,
and saves it through Linux Secret Service. It does not activate hosted routing.
Activation takes a discovered model, validates with the existing Rust parsers,
journals intent, atomically promotes the policy, and restarts the user service.
Only the exact local starter or unchanged generated policy can be replaced.
Custom paths, symlinks, and external policy edits are rejected.

The secret is never an argument, TOML value, state value, diagnostic, or response.
There is no plaintext fallback. Existing bounded api_key_cmd resolution supplies
it to the running gateway. Disconnect stops that process before deleting the
credential, preserves cleanup identity on failure, and restores the starter.
The journal format remains compatible with the unshipped plugin prototype.

A successful request alone is insufficient: setup verifies the loaded model,
non-offline service, response text, and exact successful Router receipt. Proof
is dated and cleared before another request attempt or repair. Interrupted
activation is repairable without silently replacing custom configuration.
Child execution and HTTP responses are bounded. Redirects and ambient proxies
are disabled. A filesystem lock serializes setup operations.

## Consequences

The command ships in the existing Rust binary. There is no new executable or
Python runtime dependency. The Omarchy PR depends on a Router release and verified
Linux archive pins; source-level CI does not satisfy that distribution gate.
Tests live alongside the Rust implementation. Live account, desktop keyring,
reboot, and XPS acceptance remain separate from controlled test evidence.

The macOS Swift UI and Keychain integration remain intact. Future consumers can
share this command contract after a reviewed platform credential adapter exists;
this change does not claim cross-platform Secret Service support.

Related: WF-ADR-0001, WF-ADR-0046, WF-ADR-0068, WF-ADR-0070, WF-ADR-0087.
