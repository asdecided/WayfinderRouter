# Wayfinder Desktop v0.1.0

Wayfinder's first native desktop release is a focused Apple Silicon menu-bar app with its Rust
gateway built in. It keeps routing visible without turning the popover into a dashboard, and adds a
dedicated thread-first Chat window where routing details stay secondary in a per-response popover.

## Included

- Native menu-bar status, setup, Settings, endpoint readiness, routing controls, Offline mode, and
  Keychain-backed provider configuration.
- Focused Chat with streaming, Stop, Retry, New Chat, locally persisted conversation history,
  explicit destination selection, personal destination names, and truthful failure/recovery states.
- A consolidated Connections screen for ChatGPT account access and API-key providers, including
  one-click ChatGPT route configuration before the native sign-in flow.
- The arm64 Rust gateway and authenticated Credential Broker and Foundation Model Broker XPC services
  inside the desktop app, all on the same `0.1.0` product version.
- Apple Foundation Models as an on-device provider on eligible macOS 26+ Apple Silicon systems where
  Apple Intelligence and the system model are ready.
- Opt-in ChatGPT account authentication and Sol model access through a separately installed,
  compatible, correctly signed `/Applications/ChatGPT.app`—without an OpenAI Platform API key.

Signing in to ChatGPT does not change Automatic routing or provider defaults. OpenAI Platform API
keys remain a separate provider path, and Wayfinder never imports ChatGPT tokens or reads
`~/.codex`.

## Requirements

- Apple Silicon Mac (`arm64`). Intel Macs are not supported by v0.1.0.
- macOS 14 or later.
- macOS 26+, eligible hardware, Apple Intelligence, and a ready system model for Apple Foundation
  Models delivery.
- A compatible, correctly signed `/Applications/ChatGPT.app` for ChatGPT account authentication and
  Sol access.

## Install

1. Download `Wayfinder.zip` and `Wayfinder.zip.sha256` from this release.
2. Optionally verify the archive from Terminal:

   ```bash
   shasum -a 256 -c Wayfinder.zip.sha256
   ```

3. Extract the ZIP and move `Wayfinder.app` to `/Applications`.
4. Open Wayfinder and complete the native setup flow. Existing configuration and Keychain items are
   detected and preserved.

## Current boundaries

- Conversation history is stored locally on this Mac. It is not synced or sent anywhere merely by
  being retained.
- Chat remains a thin client over the bundled gateway; it is not an agent, tool runner, filesystem
  client, or credential owner.
- The ChatGPT provider depends on verified `/Applications/ChatGPT.app` and is not self-contained.
- The release is distributed as a checksummed ZIP. DMG, Homebrew cask, automatic update, Intel, and
  universal packaging are follow-up work.
- Only Offline mode guarantees that no prompt leaves the Mac.

`Wayfinder.app` inside the downloadable archive is Developer ID signed, notarized by Apple, stapled,
and Gatekeeper checked. The ZIP is accompanied by its SHA-256 checksum and verified again after
extraction. The standalone `wayfinder-router` package keeps its separate SemVer release line;
desktop version `0.1.0` does not change or publish it.
