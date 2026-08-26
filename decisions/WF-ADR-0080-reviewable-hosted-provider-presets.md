---
id: WF-ADR-0080
type: decision
status: accepted
date: 2026-08-26
tags: [providers, activation, credentials, openai-compatibility]
---

# Reviewable hosted-provider destination presets

## Context

Wayfinder's generic OpenAI-compatible delivery already reaches many hosted
providers, but configuring each endpoint from memory invites URL mistakes and
encourages examples to embed stale model, price, and context claims. A provider
connection is also a different act from admitting that destination to
Automatic.

## Decision

1. `wayfinder-router provider presets` lists the bounded built-in catalog.
2. `wayfinder-router provider preset PROVIDER --model MODEL [--id ID]` prints
   one gateway destination table. It performs no network call and writes no
   file.
3. Each preset owns only an official OpenAI-compatible base URL and a
   conventional environment-variable reference. The operator supplies the
   model ID. The output contains no key value, price, context window, routing
   tier, fallback, or deployment policy.
4. The command accepts bounded model and destination identifiers and the
   generated fragment must parse through the production gateway parser.
5. Presets are transport conveniences, not blanket certification of every
   provider feature or model. Native or provider-specific APIs require their
   own reviewed delivery kind.

## Consequences

- Provider connection stays reviewable, reversible, and credential-free.
- Adding a destination cannot silently change Automatic, privacy posture,
  billing selection, or fallback behavior.
- Model IDs, prices, and capabilities remain explicit operator policy rather
  than stale compiled guesses.
- The preset catalog needs maintenance when an official provider endpoint or
  authentication convention changes.
