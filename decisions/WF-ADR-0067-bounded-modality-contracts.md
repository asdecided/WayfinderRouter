# WF-ADR-0067: Bounded modality capability contracts

- Status: accepted for incremental implementation
- Date: 2026-08-02
- Roadmap: `WF-ROADMAP-0010`
- Issue: #154

## Decision

Represent non-text execution as an explicit `InferenceSurface` in the shared
runtime contract. Destination snapshots carry independent, secret-free
capabilities for embeddings, image generation, audio input/output, and batch
execution. The routing core applies those capabilities as hard exclusions
before deterministic scoring; a score cannot make an incompatible destination
eligible.

The current Rust gateway advertises and executes text surfaces only. The new
capability flags default to `false`, and the existing provider adapters do not
opt into them. A known embeddings, image, audio, or batch-shaped payload sent
to the text endpoint fails with `wayfinder_router_unsupported_modality` before
provider delivery. No surface is advertised merely because a provider happens
to accept an OpenAI-shaped request.

## Incremental enablement

Each surface gets its own provider adapter and parity fixtures before being
enabled:

1. embeddings — bounded input arrays, response vectors, dimensions, usage, and
   provider-specific cost accounting;
2. image — strict binary/input and generated-output bounds;
3. audio — input/output format, duration, streaming, and retention bounds;
4. batch — durable state, cancellation, expiry, reconciliation, and batch
   accounting.

The surfaces remain independently shippable. Image, audio, and batch are not
implicitly enabled by the embeddings work, and no batch persistence exists in
this slice.

## Consequences

- Swift/FFI hosts and the gateway share one capability vocabulary and stable
  exclusion reason names.
- Unsupported modality content cannot be silently forwarded to a text-only
  provider or counted as a successful text turn.
- Existing text routing remains wire-compatible; the default contract is
  unchanged for ordinary Chat Completions and Responses requests.
- Provider adapters must explicitly opt in only after request/response bounds,
  concurrency/deadline policy, usage normalization, cost classification, and
  gateway-versus-adapter parity tests are complete.
