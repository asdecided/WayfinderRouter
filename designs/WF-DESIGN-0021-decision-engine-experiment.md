---
schema_version: 1
id: WF-DESIGN-0021
type: design
---
# Opt-in decision-engine comparison experiment

## Status

Proposed

## Context

OpenAI's 29 September 2026 DevDay recap describes a Decisions API limited
preview using Luna for questions with finite answers. As checked on 5 October,
the recap documents the concept, not an endpoint, SDK schema, model identifier,
pricing contract, confidence distribution, or this account's access:
https://openai.com/index/devday-2026-recap/

No public wire contract was found in the official documentation search. This
is an access/documentation limitation, not a claim that the API does not exist.

## Design

`wayfinder-decision-experiment` is an unpublished, standalone Rust executable
and library. Production gateway, CLI, Apple and scoring crates do not depend on
it. It calls the existing deterministic scorer with a pinned binary policy.
An asynchronous, cancellation-safe adapter accepts only a finite question,
answers and explicitly consented context fields. The evaluator owns local
UUIDs, deadlines, finite-answer validation and sanitized error categories.
Provider request IDs, model versions, confidence, distributions and cost are
optional evidence; absence stays unknown. Inconclusive is terminal for this
experiment: no delivery, hidden retry or production fallback occurs.

Default invocation runs only the deterministic baseline. External adapters
require explicit opt-in plus consent to every context field. The question and
answer choices also leave the process when a real adapter is enabled; callers
must review those along with the selected context. Expected labels and local
receipt IDs are never supplied to the adapter. Receipts omit context text.
The trait assumes trusted implementations: they must not do I/O at construction,
block the async executor, detach requests or ignore cancellation.

No live provider adapter is implemented. The two named contract doubles return
canned local answers and exercise our interface, not OpenAI's API. Their version,
request ID and latency are synthetic and their costs are unknown. Their accuracy
is a plumbing check, never evidence about Luna or Decisions API performance.
A real adapter requires published provider docs, explicit credentials/access,
a transport deadline and cancellation tests, and a reviewed disclosure policy.

## Validation

Boundary tests cover no-call consent rejection, deadlines, unavailable and
malformed responses, unknown evidence, unique local IDs, invalid probabilities,
and aggregate handling of abstentions. A four-case authored smoke corpus has
expected labels independent of engines. It is too small for quality claims.

## Related

- WF-ADR-0001 (deterministic core remains unchanged)
- WF-ADR-0004 (separate optional invocation boundary)
