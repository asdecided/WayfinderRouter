---
schema_version: 1
id: WF-ADR-0078
type: decision
status: accepted
date: 2026-08-24
tags: [routing, defaults, calibration, omarchy, evidence]
---

# Use an evidence-backed developer starter threshold

## Context

The binary router and unconfigured TOML loader inherited a `0.5` threshold that
was not tied to measured traffic. Automatic `hybrid`, `openai`, and `gemini`
starter policies separately used `0.08`; the historical lexical RouterBench
example uses `0.09` with different weights.

Issue #195 reproduced the first-run effect against the independently-authored
`benchmarks/blind/openai-cross-provider.jsonl` corpus with the real 2026.8.1
release candidate. Both `0.5` and `0.09` routed zero of 154 prompts to the high
arm. The product therefore looked operational while providing no escalation.

The public scalar score is rounded to two decimal places before tier selection.
On this corpus, `0.01` is the smallest positive effective cut. With low-arm cost
`0`, high-arm cost `1`, and a missed-high quality penalty of `2`, the native
`min-cost` calibrator selects `0.01`.

## Decision

1. `0.01` is the single developer starter cut for unconfigured binary routing,
   generated project profiles, and automatic two-arm `hybrid`, `openai`, and
   `gemini` presets.
2. Explicit TOML, `WAYFINDER_ROUTER_THRESHOLD`, and per-run threshold overrides
   remain authoritative. The offline `local` preset remains `1.0`; it promises
   local-only operation rather than automatic escalation.
3. CI executes the compiled Router against the complete frozen independent
   corpus with its default configuration. The reviewed baseline must retain
   both arms: 122/154 prompts route high, including 90/94 hard prompts, while
   32/154 remain low. Any scorer, rounding, corpus, or default change must
   update that evidence deliberately.
4. These numbers are bootstrap evidence from one by-construction corpus, not a
   universal accuracy or cost claim. Operators should run native `min-cost`
   calibration on representative labelled traffic and review the resulting
   policy before treating a cut as settled.
5. RouterBench's `0.09` lexical example remains a historical, weight-specific
   benchmark recipe. Documentation must not present it as the product default
   or as a generally deployable cut.
6. A short semantically difficult prompt may still score `0.0`; changing the
   cut cannot repair absent semantic signal. That limitation remains explicit
   and belongs to issue #171 rather than this defaults fix.

## Consequences

- A fresh Omarchy/developer setup exercises both local and strong arms instead
  of silently behaving as an all-local policy on the independent corpus.
- Core defaults, config fallbacks, generated project profiles, and automatic
  presets no longer drift independently.
- The starter sends substantially more traffic to the high arm than the old
  constants on this corpus. Users with different quality penalties, prices, or
  privacy requirements must configure or calibrate their policy.
- Frozen Python migration fixtures continue to run with an explicit `0.5` cut;
  they prove scorer compatibility, not the current product default.

## Related

- Issue #195 (silent all-local shipped thresholds)
- Issue #171 (semantic difficulty for short prompts)
- WF-ADR-0017 (native price-sensitive calibration)
- WF-ADR-0070 (native activation surface)
- WF-ROADMAP-0017 (Omarchy-first release hardening)
