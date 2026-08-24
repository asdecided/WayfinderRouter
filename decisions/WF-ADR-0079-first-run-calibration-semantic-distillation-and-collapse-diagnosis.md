---
schema_version: 1
id: WF-ADR-0079
type: decision
status: accepted
date: 2026-08-24
tags: [routing, calibration, semantics, doctor, omarchy]
---

# Calibrate automatic first run, distil static semantic signals, and diagnose route collapse

## Context

WF-ADR-0078 replaces the unmeasured `0.5` starter cut with `0.01`, derived from
the independently authored 154-row developer corpus. That repairs the all-local
default reproduced in issue #195, but a constant in source is weaker than a
generated policy receipt, and the structural scorer still assigns `0.0` to
short semantically difficult prompts such as “Prove the halting problem is
undecidable.”

Issue #171 accepts semantic teaching during offline calibration only. A request
may not call an embedding model, touch the network, resolve a credential, or
depend on a non-deterministic runtime. Threshold-only teaching is insufficient:
semantic information must remain visible in the static request-time artifact.

Diagnosis is the remaining operational gap. A valid gateway can silently use
one automatic arm for every request. The prompt-free `/router/recent` ring
already contains enough bounded evidence to detect that state, provided pinned
requests are excluded and an intentional single-arm policy is not warned.

## Decision

1. `init` and `app-setup-init` calibrate every newly created two-arm `hybrid`,
   `openai`, or `gemini` policy against the bundled
   `openai-cross-provider.jsonl` evidence with the native min-cost objective.
   The generated no-clobber TOML records corpus identity, SHA-256, sample count,
   costs, quality penalty, quality recovery, and cost savings. Local-only
   presets do not run a meaningless two-arm calibration.
2. The automatic starter enables `reasoning_term_count` at the deliberately
   small weight `0.05`. The existing reviewed lexicon is therefore a static
   semantic signal at request time. Calibration still selects the cut. On the
   frozen 154-row corpus this retains the WF-ADR-0078 routing baseline, while
   the issue reproduction reaches the high arm.
3. Native `calibrate --distill-lexicon` learns a bounded static vocabulary from
   labelled traffic. It keeps terms that appear in at least two high-arm
   documents and no low-arm documents, removes a fixed stop-word set, caps
   terms at 128, and searches a fixed semantic-weight grid under the same
   min-cost objective. It emits ordinary `[routing.lexicon]`, weights, and
   tiers; the runtime gains no dependency or new I/O path.
4. The semantic evidence fixture contains 16 training and eight held-out
   short-hard/long-easy cases. The compiled Router must improve held-out
   short-hard high routing from 0/4 to 4/4 and long-easy low routing from 0/4 to
   4/4. The generated artifact is capped at 16 KiB. These by-construction
   results establish the blind-spot repair, not universal semantic accuracy.
5. `/router/recent` adds `scored_total` and `scored_by_model`. Both are computed
   only from `mode = "scored"`; pinned, forced, and other modes cannot mask or
   manufacture automatic-route health. The ring remains prompt-free,
   in-memory, and capped at 200 entries.
6. `doctor` performs one bounded loopback read of that summary. For a policy
   with at least two automatic arms it reports:
   - `info` before 20 scored requests or when the surface is unavailable;
   - `warn` when 20 or more scored requests reached fewer than two configured
     arms, naming the zero-use arms; or
   - `pass` when at least two arms were observed.
   The warning does not make a healthy policy invalid and does not mutate it.
7. No user prompt is retained for automatic first run. The bundled bootstrap
   is intentionally not called workload-specific calibration. Workload-specific
   quality still requires explicit labels supplied to `calibrate`; unlabeled
   route frequency cannot establish which model was good enough.

## Consequences

- A fresh automatic policy is generated from executable evidence rather than
  copying an unexplained threshold, and its provenance travels with the file.
- Short semantic difficulty becomes representable without weakening
  WF-ADR-0001. Teams may replace the starter vocabulary with a deterministic
  artifact learned from their own labelled traffic.
- Route collapse becomes visible in both text and JSON doctor output without
  storing prompt content or treating deliberate local-only operation as a
  defect.
- Lexicon distillation can overfit correlated vocabulary. The artifact is
  bounded and inspectable, the evidence is held out, and documentation requires
  workload validation before activation.
- The starter corpus is developer-oriented but not user-specific. Automatic
  personalized learning remains out of scope until Wayfinder has an explicit
  label, privacy, ownership, and rollback contract.

## Related

- Issue #171 (short semantically difficult prompts)
- Issue #195 (silent all-local shipped thresholds)
- WF-ADR-0001 (standalone deterministic Router)
- WF-ADR-0003 (offline calibration and static classifier)
- WF-ADR-0007 (no in-gateway automatic recalibration)
- WF-ADR-0019 (configurable lexical signal)
- WF-ADR-0074 (bounded prompt-free recent receipts)
- WF-ADR-0078 (evidence-backed starter cut)
