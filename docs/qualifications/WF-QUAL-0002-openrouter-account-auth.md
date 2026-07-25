---
schema_version: 1
id: WF-QUAL-0002
type: provider-qualification
status: approved-for-implementation
date: 2026-07-25
tags: [ios, accounts, oauth, pkce, openrouter]
---

# OpenRouter account authentication qualification

## Decision

Wayfinder may ship the documented OpenRouter OAuth PKCE connection as a
built-in iOS account adapter.

OpenRouter documents a third-party PKCE flow that:

- sends the user to `https://openrouter.ai/auth`;
- accepts a caller-owned `callback_url` and S256 challenge;
- exchanges the returned code at
  `https://openrouter.ai/api/v1/auth/keys`;
- returns a user-controlled API key for normal OpenRouter API requests.

Wayfinder uses `ASWebAuthenticationSession`'s HTTPS callback matcher, a public
Wayfinder GitHub callback identity, S256, and an unguessable state component
embedded in the exact callback path.
The returned key is written directly to the existing device-only Keychain
credential used by the OpenRouter API-key adapter. It is never exposed through
app-visible account state.

## Product boundary

- OpenRouter account connection does not modify `Automatic`.
- `OpenRouter Auto` may consume account credits.
- `OpenRouter Free` pins `openrouter/free`, is labelled no-cost and
  rate-limited, and never silently falls back to a paid destination.
- Free availability and limits belong to OpenRouter and may change.
- Disconnecting removes the issued key and all OpenRouter destinations become
  unavailable until the user reconnects or adds a key manually.

## Evidence

- OpenRouter OAuth PKCE:
  https://openrouter.ai/docs/guides/overview/auth/oauth
- Apple web authentication sessions:
  https://developer.apple.com/documentation/authenticationservices/aswebauthenticationsession
- OpenRouter Free Models Router:
  https://openrouter.ai/docs/cookbook/get-started/free-models-router-playground
- OpenRouter free variants:
  https://openrouter.ai/docs/guides/routing/model-variants/free

## Non-precedent

This approval does not authorize Kimi, ChatGPT, Claude, Gemini, or another
consumer account flow. Each provider still requires an official third-party
execution contract and its own qualification artifact.
