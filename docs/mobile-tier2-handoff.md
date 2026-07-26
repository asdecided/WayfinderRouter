# Handoff: Wayfinder iOS, Tier 2 verification on a Mac

You are picking this up in the Claude desktop app on an Apple Silicon Mac, with
Xcode and the iOS Simulator available. The previous sessions ran in a headless
Linux container with no Apple toolchain at all. **That is the only thing that
has changed, and it changes the most important thing:** every visual, layout,
and interaction claim in this project is currently unverified, and you are the
first session able to look at any of it.

Read this file, then `docs/mobile-fidelity.md`, then
`docs/mobile-ux-audit.md` (register + Appendix A). `designs/WF-DESIGN-0020-mobile-chat-shell.md`
and `roadmaps/WF-ROADMAP-0016-native-mobile-v0.2.md` are the governing
contracts. `CONTRIBUTING.md` governs commits.

## State of the branch

Branch `claude/new-session-vrfhmp`, head `1c3861f`, Apple Mobile CI green
(run 73: Rust bridge tests, Apple bridge assembly, and the full iOS test suite
on the simulator all passed).

A nine-domain polish pass ran against the 27-finding UX audit, followed by
adversarial critics per domain, followed by a blind gauntlet review by an agent
that had built nothing. The gauntlet's verdict was not favourable and is worth
carrying forward rather than burying:

> Branding stripped, side by side: ChatGPT and Claude were built by the more
> senior team. It is not close, and the reason is not craft — it is that they
> ship a product that works.

Its three headline findings were verified from source and all held:

1. **Automatic routing could never select a destination.** Every destination
   was constructed `automaticEligible: false`; the routing core hard-excludes
   on exactly that flag. Every unpinned message failed, while three strings
   claimed the app was routing. *Now fixed* — on-device is enrolled by
   construction, hosted joins by explicit reversible opt-in.
2. **The receipt sheet asserted egress that never happened.** Its honesty
   footer branched on a `-preview` ID suffix no shipping destination carries,
   so every real receipt — including an on-device run under On-Device Only —
   claimed the response "was sent directly from this device to the selected
   provider." *Now fixed* — it branches on the persisted execution boundary.
3. **A model picker had been added to the composer**, in the same screen
   position both reference apps use, by the polish pass itself. *Now removed.*

Also fixed since: the onboarding cover presented on every cold launch; the
drawer was inserted at its open offset and so never actually slid; the
scroll-to-latest pill sat inside the composer's safe-area inset and shifted the
transcript under the reader; a pre-restore save could erase onboarding state and
routing consent; the receipt credited the router with the user's pinned choice.

Compliance matrix (Appendix A, re-checked at head): **20 Compliant, 1
Compliant-unverified, 2 Partial, 0 Violated.**

## What you can now do that no previous session could

`docs/mobile-fidelity.md` holds **52 pending rows, none of which has ever been
observed by anyone.** 39 are Tier 2 — a Mac with Xcode — and are yours to close
or fail. 13 are Tier 3 and are not.

The honest framing: every "fixed@SHA" disposition in the audit register is a
claim about source code that compiles and passes unit tests. Not one of them is
a claim that anybody saw the app do the right thing. Several of the defects
above — the drawer never sliding, the transcript shifting under the reader —
are exactly the class of defect that only looking catches, and they survived
multiple rounds of adversarial code review because reading Swift is not the
same as running it.

## Setup

Requirements come from `.github/workflows/apple-mobile-ci.yml`, which is the
authoritative recipe. Apple Silicon is asserted by CI (`uname -m` = arm64);
CI runs on `macos-26`, so Xcode 26 with an iOS 18+ simulator runtime.

```bash
git checkout claude/new-session-vrfhmp

rustup toolchain install 1.85.0 --profile minimal --component rustfmt,clippy
rustup target add aarch64-apple-darwin aarch64-apple-ios aarch64-apple-ios-sim \
  --toolchain 1.85.0

# Reproduce CI before changing anything, so a later failure is attributable.
RUSTUP_TOOLCHAIN=1.85.0 apple/scripts/test_routing_bridge.sh
xcrun simctl list devices | grep iPhone     # confirm an available simulator
xcodebuild test -project ios/WayfinderIOS/WayfinderIOS.xcodeproj \
  -scheme WayfinderIOS \
  -destination 'platform=iOS Simulator,name=iPhone 17,OS=latest' \
  CODE_SIGNING_ALLOWED=NO
```

`Previews.swift` (DEBUG-only) carries 13 `#Preview` fixtures covering the states
that are awkward to reach by driving the app — mid-stream, a reply that failed
halfway, the storage-failure notice, AX5, dark. Use them, but do not let a
preview stand in for the running app: previews do not exercise the restore, the
drawer, scroll behaviour, or the safe-area insets, and three of these fixtures
previously depicted states the app could not actually produce.

## The mission

Close the 39 Tier 2 rows in `docs/mobile-fidelity.md`, in the file, in place.
Fill in its Run metadata table. A row is closed by observing it, not by
reasoning about it. **A row you observed to be broken is a better outcome than a
row you left pending** — record the failure, fix it, re-observe.

Suggested order, highest information first:

1. Build, boot a simulator, install, and drive the nine journeys end to end.
   Screenshot each. This alone will surface more than any static pass has.
2. The appearance and accessibility matrix — light/dark × standard/increased
   contrast, then the Dynamic Type sweep to AX5 on every screen. WF-DESIGN-0020
   forbids Dynamic Type hiding send, privacy, or navigation, and compliance
   row 22 is Partial *only* because nobody has looked.
3. Motion and navigation — especially the drawer, which was rewritten to slide
   and has never been seen sliding.
4. Chrome — app icon on a Home Screen, launch screen composition. The icon was
   generated procedurally by a script that had no way to render it.
5. iPad: split view, and collapse-to-iPhone (compliance row 11, "Compliant by
   construction, unverified").

## Guardrails — these outrank any polish instinct

Carried forward from the original brief. If a fix would violate one, the
guardrail wins and the conflict gets recorded in the audit register.

1. **No model picker, ever.** Routing is automatic; the decision is the
   product. Expose the decision after the fact (receipts) and the mode label
   before it. Do not clone the reference apps' model badges or sheets. This one
   was violated once already, by a previous session, in the composer.
2. **Receipts speak the contract's grammar** — "Ran …" execution-boundary
   language. Copy must never imply a live provider responded when it did not,
   and must never imply a send that did not happen.
3. **No permanent green outline.** Accent belongs on active actions and routing
   identity only. No persistent bottom tab bar. No routing dashboard in the
   transcript.
4. **No fabricated capability.** No suggestion chips, toggles, or affordances
   for things the app cannot do — voice, image generation, plugins, web search.
   Out of scope per the roadmap: accounts, CloudKit, paired-Mac features.
5. **Secrets stay in Keychain.** Never render, log, or persist them elsewhere.
   Do not weaken the truthful terminal states (stopped / interrupted / failed) —
   the audit rates them better than the reference apps. Polish is additive.
6. **No imitation of reference branding.** Interaction quality is the
   benchmark, not their name, icons, or assets.

## What you may not claim

The tier boundary is the whole point of `docs/mobile-fidelity.md`, and it does
not dissolve because you now have a simulator.

These 13 rows need a human holding a physical device and stay pending no matter
what the simulator shows:

- **Haptics.** The simulator has none. The entire feel test is Tier 3.
- **The screen-off VoiceOver run.** Simulator VoiceOver is not the same
  experience; the Accessibility Inspector supports a check, it does not replace
  one.
- **ProMotion frame pacing** during streaming, scrolling, and drawer opening.
- **Apple On-Device execution on an eligible device.** This one matters most:
  the simulator generally reports Foundation Models unavailable, so the
  on-device-enrolled-by-default decision will most likely render the "nothing
  enrolled" branch there. Verify that branch reads well — but it is *not*
  evidence that Automatic routing works. Until a real device with Apple
  Intelligence produces a truthful on-device receipt, the product's central
  claim remains unproven.
- The blind gauntlet against the reference apps, on the same tasks.

## Open items, honestly stated

- **Compliance row 14, Partial.** The receipt sheet still offers no bounded
  error recovery.
- **Compliance row 22, Partial.** The AX5 sweep — Tier 2, now closable.
- `ChatTabView.swift:601` uses `.accessibilityRespondsToUserInteraction(true)`
  to try to make a turn's custom actions reachable. A critic pointed out this
  governs Switch Control, not VoiceOver focus, so it is probably the wrong
  tool for the job. Confirm what it actually does before removing it.
- `SettingsView` shares raw `Data` with a `SharePreview` name rather than a
  named file URL, so the recipient gets an untyped blob rather than
  `conversations.json`. P3, unfixed, not yet in the register.
- Markdown tables have no horizontal scroll container and carry no row/column
  accessibility semantics. Acknowledged-deferred in the fidelity doc; confirm
  what a table actually reads as under VoiceOver before deciding.
- Per-delta cost is reduced but not removed: the markdown parser reparses the
  volatile tail block on every delta, and the turn rotor rebuilds every entry.
  No numeric budget has ever been measured — the signposts exist, Instruments
  has never been attached.
- `ConversationStore` fetches 500 threads unpaginated; search cannot reach past
  that. Deferred with rationale — persistence change, not polish.
- No screen has a standing offline state and nothing monitors reachability.

## Working agreement

- Work on `claude/new-session-vrfhmp`. Never push elsewhere.
- Small conventional commits:
  `feat(ios)|fix(ios)|docs(mobile): imperative summary [roadmap:WF-ROADMAP-0016]`,
  with register IDs in the body.
- Tests green at every commit. Run the CI command locally before pushing —
  you can, now, and a previous session burned five rounds of remote CI on
  type-checker errors that a local build would have caught in seconds.
- Update `docs/mobile-fidelity.md` in the same commit as the fix it covers.
- When an audit disposition turns out to be false, say so in the register in
  those words. Two of them already were, and correcting them plainly cost
  nothing.
