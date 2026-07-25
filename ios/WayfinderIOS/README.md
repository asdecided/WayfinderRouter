# Wayfinder for iPhone and iPad

This target is the standalone native mobile shell governed by
`WF-ROADMAP-0016`. It embeds the authoritative Rust routing core through
`WayfinderRoutingBridge`; it does not require a Mac or localhost gateway.

The current shell routes through the embedded core and can execute a pinned
GPT-5.6 destination directly against the OpenAI Platform API. The generic
OpenAI-compatible executor owns bounded JSON requests, fragmented SSE parsing,
ordered deltas, cancellation, timeouts, sanitized provider errors, and
request/response size limits. Tests continue to use deterministic network-free
providers and an injected streaming client.

Stop, interruption recovery, failure, retry, threads, drafts, terminal message
states, and compact route receipts persist locally through the
`ConversationStore` boundary and a versioned SwiftData implementation.
API keys can be added, replaced, and removed through the native Settings flow.
Secrets remain in the device-only iOS Keychain; app-visible state contains only
configured/not-configured snapshots. The executor reads the key only inside the
provider boundary. Adding a key makes the direct destination ready but does not
silently add it to Automatic; select GPT-5.6 explicitly in Chat.

Additional provider presets, model discovery, Apple Foundation Models, and
optional Mac pairing land in later review boundaries. This compatibility slice
uses Chat Completions; an OpenAI-specific Responses API adapter remains a
separate provider-preset decision.

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
