---
id: WF-ADR-0081
type: decision
status: accepted
date: 2026-08-26
tags: [providers, anthropic, messages, streaming, tools]
---

# Native Anthropic destination

## Context

Wayfinder accepts Anthropic Messages requests from clients, but outbound
delivery has historically required an OpenAI-compatible destination.
Anthropic's native Messages API has different authentication, request,
response, error, tool, and SSE contracts and must not be represented as a
generic base-URL substitution.

## Decision

1. Add `provider = "anthropic"` as a distinct hosted, API-metered destination
   kind. It requires an explicit `base_url`, model, and `api_key_env` reference.
2. The native client appends `/v1/messages`, sends `x-api-key` and the stable
   `anthropic-version: 2023-06-01` header, disables redirects and ambient
   proxies, and uses the existing bounded timeout/response policy.
3. The delivery adapter translates the Router's normalized OpenAI Chat shape
   to Messages and translates buffered responses, errors, tool calls, usage,
   and incremental SSE back to OpenAI Chat shape. Incoming Anthropic clients
   therefore continue to reuse the same routing path without a second routing
   decision.
4. Text, streaming, system/developer instructions, function tools, tool
   results, tool choice, stops, and common sampling fields are supported.
   Images, audio, response-format constraints, log probabilities, penalties,
   seeds, provider-hosted tools, and unknown content blocks fail closed before
   delivery.
5. When an OpenAI-shaped request omits its optional output bound, the adapter
   supplies a documented bounded `max_tokens = 4096`, because Messages requires
   the field. An explicit positive request bound is preserved.
6. Connecting this destination does not add it to a routing tier, fallback,
   virtual-key allowlist, project profile, or Automatic.

## Consequences

- Native Anthropic delivery is no longer falsely described as OpenAI
  compatibility.
- The scored path remains offline, deterministic, and credential-free; only
  the selected delivery adapter resolves `ANTHROPIC_API_KEY`.
- Translation has an explicit bounded feature surface. Provider-specific beta
  headers, prompt caching, extended thinking, and hosted tools need separate
  reviewed contracts.
- Dropping the translated byte stream drops the Reqwest response stream, so
  downstream cancellation propagates to the provider transport.
