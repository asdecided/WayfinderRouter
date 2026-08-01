# Wayfinder Desktop v0.1.0 release and rollback

The production artifact is `Wayfinder.app` with its Rust gateway nested inside
`Contents/Helpers/WayfinderGateway.app`. The helper is versioned, signed, and promoted only as part
of the containing desktop product. The retired package distribution is not a rollback channel;
`desktop-v*` tags publish only the reviewed native desktop artifact.

## Supported release

- Desktop version: SemVer `0.1.0`, single-sourced in `Packaging/DESKTOP_VERSION`.
- Release tag: `desktop-v0.1.0` (or `desktop-v0.1.0-rc.N` for a prerelease).
- Platform: Apple Silicon (`arm64`) only.
- Minimum deployment target: macOS 14.0.
- Distribution lane: Developer ID signing and notarized direct download outside the Mac App Store.
- Distribution container: notarized and stapled `Wayfinder.app` in `Wayfinder.zip`, accompanied by
  `Wayfinder.zip.sha256`.
- ChatGPT account routing: included, but requires a separately installed, compatible, correctly
  signed `/Applications/ChatGPT.app`. Wayfinder does not bundle or redistribute Codex in v0.1.0.

Intel, universal binaries, DMG packaging, a Homebrew cask, automatic updates, and a Mac App Store
package are not v0.1.0 claims. An App Store lane additionally requires App Sandbox inheritance,
App Store signing/provisioning, and a separately reviewed lifecycle for the embedded gateway and
external ChatGPT dependency.

## Build prerequisites

- An Apple Silicon Mac with Xcode and the macOS 26 SDK or later, targeting macOS 14.
- Stable Rust with the `aarch64-apple-darwin` target.
- A Developer ID Application identity supplied as `CODESIGN_IDENTITY`.
- A `notarytool` keychain profile supplied as `NOTARYTOOL_PROFILE`.
- Optionally, a non-default keychain path supplied as `NOTARYTOOL_KEYCHAIN`.

Local ad-hoc builds may omit the real identity and notary profile. They are useful for architecture,
bundle, version, manifest, and launch verification, but they are not distributable release evidence
and cannot prove Apple Foundation Models production authentication.

## Version and architecture contract

`WAYFINDER_DESKTOP_VERSION` may override the version file for controlled release-candidate testing,
and `WAYFINDER_DESKTOP_BUILD_NUMBER` supplies the monotonically increasing Apple bundle build
number. The build writes the same version and build number into the outer app, containing gateway
app, and both XPC bundles. It compiles the embedded Rust gateway with that desktop product version
and stamps the signed helper manifest accordingly.

Every production executable must report exactly `arm64`:

- `Wayfinder.app/Contents/MacOS/WayfinderMac`;
- `WayfinderGateway.app/Contents/MacOS/wayfinder-router`;
- `com.wayfinder.CredentialBroker.xpc/Contents/MacOS/WayfinderCredentialBroker`;
- `com.wayfinder.FoundationModelBroker.xpc/Contents/MacOS/WayfinderFoundationModelBroker`.

Containing arm64 plus any extra architecture is a failure. The helper manifest and runtime
capabilities must also agree on `target_architecture = "arm64"`.

## Build and verify locally

From `macos/WayfinderMac`:

```bash
CODESIGN_IDENTITY="Developer ID Application: …" \
NOTARYTOOL_PROFILE="wayfinder-notary" \
WAYFINDER_DESKTOP_BUILD_NUMBER=1 \
  script/build_release_bundle.sh
```

The script:

1. builds the arm64 Rust gateway and Swift executables;
2. assembles the containing gateway app and its two XPC services;
3. proves exact architecture, product versions, and manifest/capability agreement;
4. signs the Credential Broker XPC, Foundation Model Broker XPC, helper executable, containing
   gateway app, and outer app, in that order;
5. verifies strict signatures and one non-empty Team ID across production components;
6. submits the ZIP for notarization, staples the accepted ticket, and runs Gatekeeper checks;
7. recreates the ZIP after stapling, emits its SHA-256 checksum, extracts that final ZIP, and repeats
   the bundle verification against the artifact users will download.

To verify an already extracted app independently:

```bash
WAYFINDER_REQUIRE_DISTRIBUTION_SIGNATURE=1 \
WAYFINDER_REQUIRE_NOTARIZATION=1 \
  script/verify_release_bundle.sh /path/to/Wayfinder.app
```

In a managed environment that blocks SwiftPM's normal caches or nested sandbox, set
`CLANG_MODULE_CACHE_PATH` to a writable directory and `WAYFINDER_DISABLE_SWIFTPM_SANDBOX=1`.
Ordinary developer and CI builds retain SwiftPM's sandbox.

## GitHub release environment

`.github/workflows/desktop-release.yml` runs only for `desktop-v*` tags and uses the protected
`desktop-release` environment. Configure these environment secrets; never commit or print them:

- `APPLE_DEVELOPER_ID_APPLICATION_P12_BASE64`;
- `APPLE_DEVELOPER_ID_APPLICATION_P12_PASSWORD`;
- `APPLE_NOTARY_KEY_P8_BASE64`;
- `APPLE_NOTARY_KEY_ID`;
- `APPLE_NOTARY_ISSUER_ID`.

The workflow imports the certificate and notary credentials into an ephemeral keychain, derives and
requires the single Developer ID Application identity in that certificate, validates the tag against
`DESKTOP_VERSION`, builds and verifies the final archive, uploads workflow evidence, and creates or
refreshes a **draft** GitHub release. Pull requests cannot access the release environment or its
credentials, and publishing remains a manual maintainer action after physical evidence is complete.

## Gateway and account boundaries

- Exactly one job owns `com.wayfinder-router.gateway` and port 8088.
- Production bundled-Rust discovery uses the verified absolute path under the containing app and
  never falls back to `PATH`.
- A bundled helper requires `xpc-credential-broker-v1`; broker failure never falls back to an
  environment value, command output, argv, HTTP, or a temporary file.
- The bundled Rust helper is the only supported gateway runtime. Rollback restores a previously
  verified Wayfinder app bundle; it never selects a removed Python backend.
- ChatGPT account routing accepts only the runtime inside `/Applications/ChatGPT.app` after its
  production runtime, version, code-signing, and ownership checks pass. Development overrides and
  sibling executables are ignored in release builds.
- Wayfinder never reads ChatGPT tokens or `~/.codex`, and sign-in never changes Automatic routing.
- Uninstall and rollback do not delete configuration, routing history, ledger/cache state, feedback,
  or Keychain items.

## Apple Silicon clean-machine matrix

Exercise the final extracted ZIP on a clean Apple Silicon user account and record results in
`docs/desktop-release-evidence.md`:

1. Confirm all four executable architecture lists are exactly `arm64`.
2. Run strict signature verification, `stapler validate`, and Gatekeeper assessment.
3. Install with no prior config; complete setup; verify launchd identity and `/healthz`.
4. Quit the UI and verify the gateway continues and both XPC services remain available on demand.
5. Test missing, locked, denied, rotated, and deleted Keychain items without secret output.
6. Kill the gateway and verify bounded launchd restart; repeat across logout/login and sleep/wake.
7. Interrupt setup after each external mutation and verify reassessment is idempotent.
8. Replace an earlier build and verify config, history, ledger/cache state, and Keychain preservation.
9. Force failed health/capability agreement and restore the previous verified app without data loss.
10. Roll back to the previous verified app bundle and prove one launchd label/port owner.
11. On an eligible macOS 26+ Mac, verify Apple inference while the UI is closed, cancellation, and
    rejection of copied, ad-hoc, or mismatched-Team components.
12. With the supported signed ChatGPT app installed, verify login, refresh, logout, model discovery,
    Sol delivery, cancellation, sleep/wake, missing/expired account handling, and adversarial
    no-tools/no-filesystem/no-extra-network isolation.
13. Complete `docs/desktop-fidelity.md` in Light and Dark appearances and supported accessibility
    configurations with no open P0/P1 findings.

Release evidence records hardware model, architecture, OS version, app/build version, ChatGPT/Codex
version where applicable, artifact hash, signing Team ID, exact commands, and observed outcomes. It
must not contain keys, tokens, account files, or unsanitized personal data.

## Rollback

Rollback promotes only a previously verified complete app bundle. It boots out the current gateway,
replaces the app atomically, bootstraps the helper from the restored stable bundle path, and waits
with bounded backoff for launchd identity, `/healthz`, and capability agreement. Failure is visible
and never triggers config rewriting, credential migration, or user-data deletion.
