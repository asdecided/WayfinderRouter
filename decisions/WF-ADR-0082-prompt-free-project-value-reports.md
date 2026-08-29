---
schema_version: 1
id: WF-ADR-0082
type: decision
status: accepted
date: 2026-08-29
tags: [omarchy, projects, value, privacy, accounting, evidence]
---

# Report per-project value without retaining project content

## Context

Wayfinder already keeps two different prompt-free evidence sources: a durable
daily savings ledger for successful accounted usage and a bounded process-local
ring for actual delivery outcomes. The Omarchy panel can show global savings
and recent routes, but it cannot truthfully answer what a particular project
used, where that project's requests executed, or how complete the evidence is.

Combining these sources must not turn the Router into project surveillance.
Prompts, responses, repository paths, tool arguments, credentials, and private
reasoning remain outside both stores. Missing prices, estimated tokens,
process restarts, and absent user outcome labels must remain visible rather
than being converted into confident zeroes.

## Decision

1. The successful-request savings ledger gains additive workspace attribution.
   Each attributed daily bucket retains only the workspace ID already verified
   from the virtual key, route, token count, estimated-usage flag, and aggregate
   realized/baseline amounts. Existing unattributed history remains readable
   but is not guessed into a project.
2. The bounded recent ring gains the same verified workspace ID and can report
   actual served-route, execution-boundary, terminal success, failure,
   cancellation, cache-hit, in-progress, and unobserved counts for that
   workspace. Its shared 200-entry, process-local retention remains explicit.
3. Add the authenticated local operator endpoint
   `GET /v1/value?workspace=<id>&period=today|7d|30d|all`, with equivalent
   `/value` and `/router/value` aliases. The default window is 30 days.
4. The `wf-project-value-v1` response names every denominator and observation
   window. It discloses whether prices are real or relative, how many accounted
   requests use estimated tokens, the current price-table fingerprint, and the
   `dearest-configured-rate` counterfactual used by existing savings arithmetic.
   Historical totals retain their recorded baseline amounts but not each prior
   table fingerprint, and the response names that limitation.
5. Delivery failure rate is calculated only from retained terminal delivery
   receipts. User correction rate remains `null`, evidence coverage is zero
   only when eligible receipts exist, and the response explains that explicit
   user outcome labels are not collected by this schema.
6. Export is operator-authenticated and audit-recorded. The endpoint cannot
   activate policy, change scoring, add a destination, import credentials, or
   make a recommendation.
7. Omarchy may render these source-of-truth facts and remediation copy. QML
   must not recalculate routing quality, persist a parallel ledger, or treat a
   missing field as a successful outcome.

## Consequences

- A project can receive an honest cost, savings, boundary, and delivery report
  without widening content retention.
- Workspace reporting starts when this schema is active; earlier aggregate
  ledger entries remain global only.
- Cost history can span the ledger's bounded daily retention, while delivery
  health is deliberately a shorter current-process sample. Consumers must not
  merge those denominators.
- Correction-aware recommendations remain later work and require a separate
  explicit, prompt-free outcome-label contract and activation review.

## Related

- WF-ADR-0064 (evidence reports)
- WF-ADR-0068 (Omarchy plugin boundary)
- WF-ADR-0074 (bounded delivery receipts)
- WF-ADR-0075 (authenticated local project profiles)
- WF-ROADMAP-0017 (Omarchy-first delivery)
