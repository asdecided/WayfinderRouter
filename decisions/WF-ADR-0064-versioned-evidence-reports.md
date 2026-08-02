---
schema_version: 1
id: WF-ADR-0064
type: decision
status: accepted
date: 2026-08-02
tags: [rust, gateway, enterprise, evidence, statistics, privacy, audit]
---

# Produce versioned evidence reports without inventing quality

## Context

WF-ADR-0063 deliberately retains only bounded, prompt-free shadow metadata.
Raw counters are not a release decision: operators need to see missingness,
provider outcomes, cost class, quality labels, confidence intervals, and the
provenance of any automated evaluator. The report must be reproducible from
the same retained inputs and must refuse to call sparse or conflicting data
significant.

## Decision

Add a deterministic `wf-evidence-v1` report over the bounded shadow snapshot.
The report is a pure transformation of records and labels; it has no clock,
network, model, or filesystem dependency. It records sorted scorer,
configuration, and candidate fingerprints, per-candidate decision/provider
summaries, quality label counts, Wilson intervals, and human-versus-automated
agreement (including Cohen's kappa where paired labels exist).

Quality labels are typed and prompt-free:

```json
{
  "request_id": "shadow-request-id",
  "candidate_route": "on-device-first",
  "label": "win",
  "evaluator": {"kind": "automated", "version": "heuristic-1"}
}
```

`human`, `automated` with an explicit version, and `win`, `loss`, `tie`, or
`abstain` are the only accepted values. The bounded process-local label store
deduplicates by request/candidate/evaluator identity and rejects unknown
fields, prompt/response fields, control characters, and over-capacity batches.
Content retention is not enabled by this release; a future content-bearing
workflow would require a separate access-control and retention decision.

The operator-only surfaces are:

- `GET /v1/evidence` (also `/router/evidence`) for the stable JSON artifact;
- `GET /v1/evidence.txt` (also `/router/evidence.txt`) for a self-contained
  human-readable summary; and
- `POST /v1/evidence/labels` (also `/router/evidence/labels`) to add bounded
  labels.

All three are covered by the existing operator authentication and append-only
audit boundary. The managed data plane does not expose them.

The outcome is tri-state: `enforce`, `keep-shadowing`, or `do-not-enforce`.
Fewer than 30 decisive labels, no records, mixed fingerprints, missing
quality labels, or an interval crossing zero keeps shadowing. A lower bound at
or above zero permits `enforce`; an upper bound below zero permits
`do-not-enforce`. Provider errors over 20% reject the candidate independently
of quality labels. Ties and abstentions remain visible and are never folded
into wins. Cost is labelled `currency`, `relative`, or `unknown`; an unknown
cost is not treated as zero.

## Consequences

- Identical snapshots and labels produce byte-stable JSON and text reports.
- Quality claims are separated from observed provider latency, errors, and
  cost, so missing evidence remains visible.
- Human and automated evidence can disagree without one silently replacing the
  other; agreement is a reported statistic, not an approval shortcut.
- The report is intentionally process-local in this slice. Fleet aggregation,
  rolling tripwires, and canary enforcement remain separate work.

## Rejected alternatives

- **Treat missing labels as ties.** This inflates apparent quality and hides
  coverage bias.
- **Declare significance from a point estimate.** A confidence interval and a
  minimum decisive sample are required to avoid a false flip.
- **Use an unversioned automated judge.** Without evaluator provenance, a report
  cannot be reproduced or compared across releases.
- **Accept prompt/response fields in the label endpoint.** The running gateway
  does not need content to compute this report; accepting it would create an
  accidental retention path.

## Related

- WF-ADR-0063 — bounded deterministic shadow routing
- WF-ADR-0037 — automated sufficiency judge
- WF-ADR-0057 — operator OIDC authentication
- WF-ROADMAP-0010 — evidence reports and canary rollout
- Issue #151 — Enterprise evidence: generate quality and efficiency reports
