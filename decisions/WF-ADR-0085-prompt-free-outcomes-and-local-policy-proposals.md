---
schema_version: 1
id: WF-ADR-0085
type: decision
status: accepted
date: 2026-08-31
tags: [omarchy, outcomes, privacy, evidence, policy, local]
---

# Record prompt-free outcomes and propose narrower local routing

## Context

The project value report can identify terminal delivery receipts, but it must
currently report correction evidence as unavailable. Earlier feedback designs
persisted prompt text with a label and fed it into recalibration. That contract
is not suitable for the Omarchy-first product: it widens content retention and
cannot prove that a label describes the route that actually executed after
fallback.

An explicit correction on a local result can justify routing a score band to a
hosted tier, but the inverse is not safe. A negative result selected for hosted
and served locally after fallback is confounded delivery evidence, not proof
that the normal local score band should change. Recommendations must also remain
separate from activation.

## Decision

1. Add the operator-authenticated `wf-outcome-label-v1` contract at
   `POST /v1/outcomes`, with `/outcomes` and `/router/outcomes` aliases. It
   accepts only a bounded workspace ID, retained request ID, and one explicit
   `success`, `correction`, or `failure` judgment.
2. A label can attach only to a terminal prompt-free receipt in the exact
   workspace supplied by the operator. The label stores no prompt, response,
   repository path, tool argument, credential, private reasoning, or free-form
   note. Re-submission replaces that receipt's explicit judgment.
3. Outcome labels share the existing 200-entry process-local receipt ring and
   its reload lifecycle. Eviction or process restart removes the label with its
   receipt. Value reports name coverage and calculate correction and failure
   rates only over explicitly labelled retained receipts.
4. Add the operator-authenticated `wf-local-outcome-policy-v1` report at
   `GET /v1/outcomes/policy?workspace=<id>`, with equivalent aliases. It supports
   only a scored two-tier policy whose lower tier is explicitly local and upper
   tier is hosted.
5. Policy evidence is actionable only when the scored selected tier and actual
   execution boundary agree. Cross-boundary fallback in either direction is
   counted as confounded and excluded. In particular, a hosted selection served
   locally cannot be classified as lower-tier evidence.
6. An actionable local `correction` or `failure` may propose lowering the
   hosted tier's inclusive threshold to the lowest negative local score. This
   narrows the local score range. Local successes that would also move are
   counted visibly. Success-only evidence retains the current threshold.
7. The report cannot raise the hosted threshold, expand local routing, change a
   classifier, add a destination, broaden privacy/capability/budget/allowlist
   eligibility, edit a file, activate policy, or trigger reload. It emits
   structured current and proposed tiers for explicit review only.
8. Label writes and proposal exports use the existing local operator
   authentication and audit boundary. The scored request path remains offline,
   deterministic, keyless, and independent of the evidence store.
9. `wayfinder-router capabilities --json` advertises both versioned schemas,
   canonical endpoints, prompt-free retention, proposal direction, and the
   absence of automatic activation so Omarchy can integrate without probing.

## Consequences

- Omarchy can collect a useful correction signal without retaining project
  content or reviving the raw-text feedback log.
- Project value can report evidence coverage rather than a permanent unknown.
- A recommendation is deliberately conservative: it can reduce local exposure
  after directly observed negative local outcomes, but it cannot claim that
  untested work is safe locally.
- The bounded ring is suitable for a one-workstation review loop, not durable
  experimentation. A separate pilot contract must freeze arms, trials, and
  denominators before broader local routing can be justified.

## Related

- WF-ADR-0001 (offline deterministic decision path)
- WF-ADR-0064 (evidence reports)
- WF-ADR-0074 (bounded delivery receipts)
- WF-ADR-0082 (prompt-free project value reports)
- WF-ROADMAP-0017 (Omarchy-first delivery)
