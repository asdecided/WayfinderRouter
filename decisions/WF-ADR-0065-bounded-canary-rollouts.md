# WF-ADR-0065: Bounded fleet-wide canary rollouts and tripwires

- Status: accepted for implementation
- Date: 2026-08-02
- Roadmap: `WF-ROADMAP-0010`
- Issue: #152

## Decision

Canary routing is an opt-in gate after the authoritative deterministic route
decision. It may replace an automatic scored destination with the first
eligible model in one named route preset; it never changes the complexity
score, recommendation, privacy eligibility, or explicit pins.

The assignment identity is selected from a configured scope (`request`,
`workspace`, `key`, or an explicitly approved `cohort`) and hashed with the
stable rollout identifier. A request must pass both the deterministic fraction
and a fleet-wide Redis fixed-window exposure admission. If the identity,
candidate, Redis state, or tripwire state cannot be verified, the request stays
on the normal route.

Canary observations are prompt-free and idempotent by request ID. Redis stores
bounded fixed-window counters for requests, errors, latency, priced cost, and
optional quality labels. Any replica may trip the rollout; the shared trip
reason and bounded rollback hold prevent further assignments on every replica.
The existing operator audit log records the rollback decision without prompt or
response content.

## Configuration

```toml
[gateway.canary]
enabled = true
rollout_id = "release-2026-08"
candidate_route = "candidate"
fraction = 0.05
scope = "workspace"
max_requests = 1000
window = 300
min_samples = 30
max_error_rate = 0.20
max_latency_ms = 30000
max_cost_multiplier = 2.0
max_quality_loss = 0.10
rollback_hold = 900
```

Enabling a rollout requires `gateway.state.backend = "redis"`; memory state is
not a fleet authority and therefore fails closed. The exposure ceiling is a
fixed-window request limit, not an unbounded percentage promise.

## Tripwire semantics

Tripwires are evaluated only after `min_samples` unique observations. Error
rate, mean latency, cost multiplier, and labelled quality loss are evaluated
independently. Missing cost or quality observations are not imputed. The first
failing reason is written as the shared rollback reason and remains active for
`rollback_hold` seconds.

## Consequences

- A canary cannot silently broaden hosted or local-network eligibility.
- A Redis outage suppresses new canary assignments rather than expanding them.
- Existing Chat Completions behavior is unchanged when the policy is disabled.
- The Responses API and shadow evidence reports consume the same prompt-free
  route receipts but do not alter canary decisions.
