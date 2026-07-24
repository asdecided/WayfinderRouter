# Wayfinder for iPhone and iPad

This target is the standalone native mobile shell governed by
`WF-ROADMAP-0016`. It embeds the authoritative Rust routing core through
`WayfinderRoutingBridge`; it does not require a Mac or localhost gateway.

The current shell routes through the embedded core, then exercises the full
Chat lifecycle through a deterministic network-free provider. Ordered deltas,
stop, interruption recovery, failure, retry, threads, drafts, terminal message
states, and compact route receipts persist locally through the
`ConversationStore` boundary and a versioned SwiftData implementation.
Credentials, live providers, Apple Foundation Models, and optional Mac pairing
land in later review boundaries.

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
