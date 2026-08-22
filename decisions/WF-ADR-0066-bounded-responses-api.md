# WF-ADR-0066: Bounded authenticated Responses API compatibility

- Status: accepted for implementation
- Date: 2026-08-02
- Expanded: 2026-08-22 for the Codex CLI tool contract
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

The supported core request fields are `model`, `input`, `instructions`,
`stream`, `max_output_tokens`, and `temperature`. The adapter also validates
the Codex CLI Responses envelope: `tools`, `tool_choice`,
`parallel_tool_calls`, `reasoning`, `store`, `stream_options`, `include`,
`service_tier`, `prompt_cache_key`, `text`, and `client_metadata`. Only controls
with a safe destination equivalent are forwarded. Stored/background execution,
structured text output, and hosted tools remain unsupported and fail closed.

`input` accepts a string or up to 64 typed message/tool items. Messages support
`system`, `developer`, `user`, or `assistant` roles and text parts
(`input_text`, `output_text`, or `text`). Function calls, custom tool calls, and
their text outputs are normalized into Chat Completions history. Up to 128
function, custom/freeform, or namespaced tools are translated to model-visible
functions; the adapter retains their identity and restores the correct
Responses `function_call` or `custom_tool_call` item, including namespace, on
the return path. Unknown fields and unsupported item/part/tool types return a
specific `wayfinder_router_unsupported_request` error; they are never silently
discarded.

## Bounded streaming contract

Buffered responses are limited by the existing gateway response bound and
normalize provider usage to `input_tokens`, `output_tokens`, and `total_tokens`.
Streaming emits `response.created`, ordered `response.output_text.delta`
events, completed message/tool output items, and exactly one terminal
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
- Unsupported Responses features such as hosted tools, background jobs,
  multimodal parts, stored conversations, structured text output, and
  previous-response references fail closed until a bounded contract is
  designed for them.
- A provider can continue to return its native usage and streaming cadence;
  Wayfinder normalizes only the fields it can prove and omits unavailable
  usage rather than fabricating it.
- The adapter does not make ChatGPT consumer accounts into OpenAI Platform
  credentials and does not alter privacy or route fallback policy.
