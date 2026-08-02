# WF-ADR-0066: Bounded authenticated Responses API compatibility

- Status: accepted for implementation
- Date: 2026-08-02
- Roadmap: `WF-ROADMAP-0010`
- Issue: #153

## Decision

Expose `/v1/responses` and `/responses` on both the local and authenticated
managed data-plane routers. The endpoint is an adapter boundary: it validates a
small, explicitly documented Responses request, translates supported message
input into the existing Chat Completions execution path, and maps the result
back into a Responses-shaped response. The deterministic scorer, access
preflight, privacy filtering, budgets, admission, delivery, failover, and
accounting therefore remain one shared path.

The supported request fields are `model`, `input`, `instructions`, `stream`,
`max_output_tokens`, and `temperature`. `input` accepts a string or up to 64
typed `message` items with `system`, `developer`, `user`, or `assistant` roles.
Message content accepts text or text parts (`input_text`, `output_text`, or
`text`). Unknown fields and unsupported item/part types return a specific
`wayfinder_router_unsupported_request` error; they are never discarded.

## Bounded streaming contract

Buffered responses are limited by the existing gateway response bound and
normalize provider usage to `input_tokens`, `output_tokens`, and `total_tokens`.
Streaming emits `response.created`, ordered
`response.output_text.delta` events, and exactly one terminal
`response.completed` or `response.failed` event. The relay caps input history,
output characters, and upstream SSE event count. Duplicate `[DONE]` markers,
late events, cancellation, and retry do not create a second terminal event or
second gateway delivery/accounting operation.

Responses errors preserve the authenticated route headers and use the same
sanitized gateway error envelope as the existing inference surfaces. Chat
Completions remains unchanged and is the compatibility implementation beneath
the adapter, not a second routing algorithm.

## Consequences

- Existing OpenAI-shaped clients can adopt Responses without a new provider
  credential path or a second route decision.
- Unsupported Responses features such as tools, background jobs, multimodal
  parts, stored conversations, and previous-response references fail closed
  until a bounded contract is designed for them.
- A provider can continue to return its native usage and streaming cadence;
  Wayfinder normalizes only the fields it can prove and omits unavailable
  usage rather than fabricating it.
- The adapter does not make ChatGPT consumer accounts into OpenAI Platform
  credentials and does not alter privacy or route fallback policy.
