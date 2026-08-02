---
schema_version: 1
id: WF-ADR-0063
type: decision
status: accepted
date: 2026-08-02
tags: [rust, gateway, enterprise, evidence, shadow, privacy, cost]
---

# Collect bounded deterministic shadow evidence off the request path

## Context

Operators need evidence about a proposed route before changing the production
decision. The evidence path must not turn the scorer into a model call, retain
prompt content, or make production availability depend on a second provider
request. It must also be possible to stop the work through the existing
last-good configuration reload boundary.

## Decision

Add an opt-in `[gateway.shadow]` policy. It names existing routing presets as
counterfactual candidates and deterministically samples request IDs into a
bounded process-local evaluator:

```toml
[gateway.shadow]
enabled = true
sample_rate = 0.05
candidate_routes = ["on-device-first", "cloud-first"]
max_in_flight = 2
max_records = 2048
provider_comparisons = false
provider_sample_rate = 0.0
provider_max_requests = 0
provider_window = 60
```

The production route and delivery complete first. Shadow work is queued with a
separate semaphore and never shares the production admission, retry, budget,
reliability, or cache decision. A candidate evaluates the same authoritative
deterministic score and then selects the first eligible model in that named
route. The retained record includes the scorer/runtime version, routing and
candidate fingerprints, production/counterfactual model and score, bounded
reason codes, and terminal status; prompts, responses, credential material,
and prompt-derived identifiers are not retained.

Provider comparisons are a separate, double opt-in. Configuration must enable
them and the request must carry
`x-wayfinder-shadow-provider-consent: true` (or `1`/`yes`). The request's
privacy posture must be `hosted-allowed`, the candidate must be eligible, and a
deterministic sample and process-local request budget must admit it. The
comparison records only status, bounded latency, and estimated cost. A
comparison failure is shadow evidence, never a production failure.

The store retains at most `max_records` records, evaluates at most
`max_in_flight` jobs concurrently, and bounds provider comparisons per
`provider_window`. Metrics use bounded event/candidate labels and expose
explicit provider-comparison cost only when observed. Hot reload retains the
process-lifetime store but toggles an active flag from the new configuration;
disabling shadow work stops queued and in-flight evaluators at their next
cooperative boundary. Re-enabling it resumes sampling without changing the
production route.

Candidate routes are validated against named routes at configuration load. The
default is disabled, so existing deployments are byte-for-byte no-ops. The
scored model decision remains offline, deterministic, keyless, and explainable.

## Consequences

- Enterprises can collect counterfactual coverage and divergence evidence
  without changing the response a caller receives.
- Provider comparisons are explicit, privacy-gated, sampled, and budgeted; a
  shadow run cannot silently expand hosted egress.
- Records are suitable for deterministic replay from approved fixture inputs
  using the recorded scorer and candidate fingerprints, while remaining
  prompt-free in the running gateway.
- Evidence is process-local and bounded in this slice. The report and fleet
  aggregation layers remain separate follow-on work (issues #151 and #152).

## Rejected alternatives

- **Run the candidate before serving production.** This adds latency and makes
  a non-authoritative comparison a production dependency.
- **Send every request to every candidate.** This creates uncontrolled provider
  spend and violates the privacy boundary of users who did not opt in.
- **Store prompts and responses by default.** It would turn an operational
  evidence feature into a transcript retention system and make the gateway a
  high-value data target.
- **Let shadow failures alter routing or provider health.** Shadow work is
  observational; only the production delivery path can affect production
  health and fallback.

## Related

- WF-ADR-0001 — deterministic, model-free routing
- WF-ADR-0056 — hard destination eligibility
- WF-ADR-0060 — fleet accounting, admission, and provider health
- WF-ROADMAP-0010 — verified-efficiency evidence engine
- Issue #150 — Enterprise evidence: add deterministic shadow routing
