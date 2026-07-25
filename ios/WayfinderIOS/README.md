# Wayfinder for iPhone and iPad

This target is the standalone native mobile shell governed by
`WF-ROADMAP-0016`. It embeds the authoritative Rust routing core through
`WayfinderRoutingBridge`; it does not require a Mac or localhost gateway.

The current shell routes through the embedded core and can execute Apple
Foundation Models in-process on eligible devices, plus pinned OpenAI Platform
GPT-5.6, Moonshot/Kimi Platform Kimi K2.6, OpenRouter Auto, and OpenRouter Free
destinations directly from iOS. The generic OpenAI-compatible executor owns
bounded JSON requests, fragmented SSE parsing, ordered deltas, cancellation,
timeouts, sanitized provider errors, and request/response size limits. Tests
continue to use deterministic network-free providers and an injected streaming
client.

Stop, interruption recovery, failure, retry, threads, drafts, terminal message
states, and compact route receipts persist locally through the
`ConversationStore` boundary and a versioned SwiftData implementation.
API keys can be added, replaced, and removed through the native Settings flow.
Secrets remain in the device-only iOS Keychain; app-visible state contains only
configured/not-configured snapshots. The executor reads the key only inside the
provider boundary. Adding a key makes only that provider's destinations ready
but does not silently add any destination to Automatic; select a direct
destination explicitly in Chat.

Authenticated, bounded model discovery publishes current Moonshot/Kimi and
OpenRouter text models after their keys are configured. Destinations supports
search, pull-to-refresh, and explicit Chat selection. OpenAI Platform keeps the
compiled GPT-5.6 fallback because its general catalog does not reliably
distinguish chat-capable models. Discovery failures preserve the last useful
inventory and never expose raw provider errors.

Apple On-Device reports live framework readiness, streams ordered output,
supports cancellation, and never enters Automatic merely because it is
available. The native capability snapshot reports framework-backed text,
streaming, and supported-language count. Context remains unspecified because
the public framework does not expose a reliable context-window value. The
versioned, content-free physical-device gate is documented in
`docs/ios-apple-foundation-models-live-integration.md`. Optional Mac pairing
lands in a later review boundary. Cloud presets use Chat Completions; an
OpenAI-specific Responses API adapter remains a separate provider decision.

## Build

Generate the ignored bridge products, then build the checked-in Xcode project:

```sh
apple/scripts/build_routing_xcframework.sh
xcodebuild \
  -project ios/WayfinderIOS/WayfinderIOS.xcodeproj \
  -scheme WayfinderIOS \
  -destination 'platform=iOS Simulator,name=iPhone 17,OS=latest' \
  test
```

After changing `project.yml`, regenerate the project with:

```sh
xcodegen generate --spec ios/WayfinderIOS/project.yml
```

When no compatible Simulator runtime is installed, the app module can still
be compile-checked against the iOS Simulator SDK:

```sh
swift build \
  --package-path ios/WayfinderIOS \
  --triple arm64-apple-ios18.0-simulator \
  --sdk "$(xcrun --sdk iphonesimulator --show-sdk-path)"
```
