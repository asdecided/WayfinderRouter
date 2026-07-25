# iOS Apple Foundation Models live integration gate

This gate exercises the real in-process Foundation Models provider on an
eligible physical iPhone or iPad. It is skipped on Simulator and never records
generated content. Its retained XCTest attachment contains only the prompt
contract version, normalized availability, supported-language count, response
byte and delta counts, and terminal-completion count.

## Requirements

- an eligible physical iPhone or iPad running iOS or iPadOS 26 or newer;
- Apple Intelligence enabled and `SystemLanguageModel.default` available;
- Developer Mode enabled;
- an Apple Development identity and provisioning profile for the Wayfinder app
  and test bundle;
- for offline evidence, a wired developer connection with Wi-Fi and cellular
  disabled before the test begins.

The offline run uses a wired connection because disabling every device network
interface would otherwise also disconnect Xcode's test runner. The test itself
calls only `NativeAppleFoundationModelsProvider`; no hosted executor, gateway,
or Mac helper participates.

## Run

Regenerate the project after adding or moving test sources:

```sh
xcodegen generate --spec ios/WayfinderIOS/project.yml
```

Then run the explicit device gate:

```sh
ios/WayfinderIOS/scripts/test-apple-foundation-models-device.sh \
  <physical-device-id> \
  <development-team-id>
```

The team ID is optional when Xcode already has a default development team.
The script permits provisioning updates but does not create, copy, or print
credentials.

## Contract

The live gate fails unless all of these are true:

- availability is `available`;
- the framework advertises text, streaming, and at least one supported
  language;
- the public framework continues to omit a stable context-window value;
- prompt contract `apple-on-device-v1` produces at least one non-empty ordered
  delta;
- the response stays within the provider byte bound;
- exactly one terminal completion arrives.

Generated text is intentionally neither attached nor printed. Model quality is
reviewed against the fixed synthetic evidence prompt without retaining user
content:

> In one concise sentence, explain why an on-device model is useful when a
> phone has no internet connection.

Changing the default instructions or the evidence prompt requires a new prompt
contract version and a new physical-device run.

## Recorded evidence

Pending. Record only the date, device/OS class, prompt contract, network state,
test result, and content-free attachment values. Do not record the device
identifier, serial number, generated response, signing material, or private
prompt content.
