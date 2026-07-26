# Wayfinder for iPhone and iPad fidelity checklist

Status: release gate for the v0.2.0 mobile client; incomplete until every required row has dated
evidence and no open P0/P1 finding. Governing contracts: WF-ROADMAP-0016 and WF-DESIGN-0020.
Companion to `docs/desktop-fidelity.md`, whose severity model and evidence hygiene this file adopts.

This checklist reviews a build installed on a simulator or device, not SwiftUI previews. Record
screenshots in a release-specific evidence folder containing no credentials, account tokens, private
prompts, or unrelated device content.

## Why this file exists

The polish pass that produced the current build ran in a **Tier 1** environment: a headless Linux
container with no Xcode and no Swift toolchain. Everything it could prove, it proved with XCTest and
the `apple-mobile-ci.yml` simulator job. Everything it could not prove is enumerated below rather
than assumed. **No row here has been observed.** Each is traceable to a finding in
`docs/mobile-ux-audit.md`.

| Tier | Available to the implementer | Used for |
|---|---|---|
| 1 | Headless CI, `xcodebuild test` on an iPhone 17 simulator | Adversarial code review, unit and lifecycle tests, contrast arithmetic, parser and scroll-rule coverage |
| 2 | A Mac with Xcode | Everything in the "Tier 2" rows below |
| 3 | A human holding a device | Everything in the "Tier 3" rows below |

## Run metadata

| Field | Value |
|---|---|
| Date/time | Pending |
| Commit | Pending |
| Marketing version / build | 0.2.0 / Pending |
| Device model | Pending |
| iOS version | Pending |
| Appearance and accessibility configuration | Pending |
| Reviewer | Pending |

## Severity and completion

- **P0:** blocks use, risks credential exposure, or misstates an execution boundary — the mobile
  analog of the desktop one-gateway rule: on-device, local-network, and hosted claims must be true.
- **P1:** broken primary workflow, inaccessible required action, clipping, misleading state, or lost
  recovery path. Blocks release.
- **P2:** material polish or secondary-workflow defect. Record with an owner and disposition.
- Mark a row complete only after observing it in a real build. Automated coverage may support but
  does not replace visual, keyboard, or assistive-technology observation.

## Tier 2 — a Mac with Xcode

### Performance (UX-021)

- [ ] Cold launch to first interactive frame is under 400 ms on the oldest supported device.
      The `appInit` signpost in the `com.wayfinder.router.ios` `launch` category covers only
      `App.init()` — container and provider construction — so it bounds a prefix of that budget,
      not the whole of it. Measure the frame separately.
- [ ] Thread restore for a 200-turn conversation completes without a visible stall. Read the
      `restoreConversations` interval in the `conversation` category.
- [ ] Streaming a 10 000-token reply at 80 tok/s holds 120 Hz with zero dropped frames in the
      Animation Hitches instrument.
- [ ] The `applyDelta` signpost fires once per delta and the `checkpoint` interval fires on the
      cadence in `StreamingCheckpointPolicy.standard`, not per delta.
- [ ] Memory does not grow unbounded across a 50-turn conversation with long replies. Applying a
      delta is still O(reply length) — the thread snapshot and its message array are copied per
      token — so a very long reply remains quadratic in memory traffic even though the per-token
      encode and write are gone.
- [ ] Typing continuously while a long reply streams loses no reply text and no keystrokes.

### Typography and layout (UX-005)

- [ ] Every screen at Dynamic Type xSmall through AX5 shows no clipped, truncated, or hidden
      control. WF-DESIGN-0020 names send, privacy, and navigation explicitly.
- [ ] The composer expands with accessibility text sizes without pushing send off-screen or
      collapsing the transcript below usability.
- [ ] Streaming a 10 000-token reply containing headings, nested lists, tables, and fenced code
      shows no re-layout of content already on screen.
- [ ] Long unbroken tokens — URLs, base64, minified code — wrap or scroll rather than forcing
      horizontal page scroll.
- [ ] Wide markdown tables: they wrap rather than scroll, unlike code blocks, so confirm a
      four-column table at AX3 is readable.
- [ ] Scrolling up during a streaming reply keeps position, the scroll-to-latest control appears,
      and returning to the bottom resumes following — including while the composer grows and the
      keyboard appears, both of which change the scroll view's insets.
- [ ] Switching to a long conversation restores its tail; the anchor is inside a `LazyVStack` and
      may not be realised.
- [ ] Code blocks scroll horizontally inside their own container; the page never scrolls sideways.

### Appearance and accessibility matrix

Repeat the required surfaces in each row: Chat, Threads, Destinations, Settings, API Keys.

| Configuration | Chat | Threads | Destinations | Settings | Keys | Result/evidence |
|---|---:|---:|---:|---:|---:|---|
| Light, default text | [ ] | [ ] | [ ] | [ ] | [ ] | Pending |
| Dark, default text | [ ] | [ ] | [ ] | [ ] | [ ] | Pending |
| Light, AX3 text | [ ] | [ ] | [ ] | [ ] | [ ] | Pending |
| Dark, AX5 text | [ ] | [ ] | [ ] | [ ] | [ ] | Pending |
| Increased Contrast | [ ] | [ ] | [ ] | [ ] | [ ] | Pending |
| Reduce Transparency | [ ] | [ ] | [ ] | [ ] | [ ] | Pending |
| Reduce Motion | [ ] | [ ] | [ ] | [ ] | [ ] | Pending |
| Bold Text | [ ] | [ ] | [ ] | [ ] | [ ] | Pending |

For every configuration:

- [ ] Text, controls, selection, route-boundary colour, and destructive states retain adequate
      contrast and never rely on colour alone. Ratios are asserted in `WayfinderThemeContrastTests`;
      this row confirms they hold once composited over materials.
- [ ] Content reflows or scrolls intentionally; no label truncates a required distinction.
- [ ] Reduce Motion replaces every spring with a crossfade and hides no state change.
- [ ] Reduce Transparency replaces the composer and drawer materials with a readable opaque surface.

### Motion and navigation (UX-019, UX-020)

- [ ] **Automatic actually routes on a supported device.** On-device execution is enrolled by
      construction, but its readiness is reported by the live device — no simulator or CI run can
      confirm that Apple Intelligence resolves as available, that Automatic then selects it, and
      that the receipt reads "Ran on this device". On hardware without Apple Intelligence the
      honest "nothing enrolled" state is what should appear instead; both branches need a device.
- [ ] Enrolling a hosted destination in Destinations changes the Chat copy and the composer's route
      label in the same session, and withdrawing it changes them back.
- [ ] **The drawer slides.** It was conditionally inserted into the hierarchy on open, so it
      appeared already at its final offset and was removed outright on close — it popped in and out
      rather than animating either way, and no critic caught it from the code until the drawer's
      existence gate was read as a layout question rather than a lifecycle one. Scrim and panel now
      stay mounted in both states. This is the app's most-used interaction and the fix is entirely
      unobserved.
- [ ] The drawer is interruptible: reversing a drag mid-flight retargets the spring rather than
      snapping to either end.
- [ ] The scroll-to-latest pill floats above the composer as an overlay with an alignment guide,
      and the transcript does **not** shift when it appears or leaves. Verify specifically while
      scrolled up mid-stream, which is the only moment it appears.
- [ ] The drawer dismisses by swipe as well as by scrim tap, and the scrim fades in proportion to
      drag distance.
- [ ] Pushing a detail screen inside Settings, opening the drawer, switching to Threads, and
      returning to Settings preserves the pushed screen.
- [ ] No page indicator, tab bar, or inter-section swipe is reachable from the hidden state
      container.
- [ ] Sheets keep their detents and drag physics; the receipt sheet opens at medium and expands.

### Chrome (UX-012)

- [ ] The app icon renders correctly on the Home Screen, in Settings, in Spotlight, and in the app
      switcher, at every size the system derives.
- [ ] The launch screen background matches the first frame's `systemBackground` in both appearances
      with no visible flash or jump as the app becomes interactive.
- [ ] Status bar style is legible against Chat in both appearances.
- [ ] Safe areas are correct in portrait, landscape, and with the Dynamic Island; the composer
      clears the home indicator without a floating gap.
- [ ] iPad: the split view collapses to the iPhone model rather than compressing columns; Command-N,
      Command-Return, and Escape all work from a hardware keyboard, and none of them fires while a
      section other than Chat is showing.
- [ ] The drawer panel itself extends under the status bar and home indicator, not only its scrim,
      in portrait, landscape, and with the Dynamic Island.
- [ ] Status-bar glyphs stay legible over the open drawer and its scrim, not only over Chat.
- [ ] The app icon's iOS 18 dark and tinted appearance variants — the catalogue currently ships the
      single universal entry only, so the system derives both.
- [ ] The launch screen's composition against the real first frame: the background matches, but the
      launch mark is centred while Chat's empty state sits at 55% height behind a navigation bar.

### Screens and flows

- [ ] Capture the light/dark × default/AX3 screenshot matrix for every screen.
- [ ] Record XCUITest flow videos for: first launch, send and stop, retry after failure, thread
      rename and pin, and the privacy posture change.
- [ ] Blind screenshot comparison against the reference-pattern inventory in
      `docs/mobile-ux-audit.md` section 5, branding stripped, judged pair by pair.

## Tier 3 — a human with a device

### Haptics (UX-026)

- [ ] The feel test: cover the screen, perform ten actions — send, stop, complete, fail, retry,
      copy, pin, delete, posture change, new chat — and judge whether the haptic vocabulary alone
      communicates what happened.
- [ ] No haptic fires on scrolling, or on any gesture the user already felt through their own touch.
- [ ] Send and completion are distinguishable from each other without looking.

### VoiceOver (UX-003, UX-027)

- [ ] Screen-off run of the entire app: start a conversation, read a reply, retry a failure, open a
      receipt, change privacy posture, and delete a thread, without sighted assistance.
- [ ] Streaming start, completion, and failure are announced and are not drowned by the
      `updatesFrequently` trait on the growing reply.
- [ ] The turn rotor moves between turns in visual order.
- [ ] The drawer traps focus while open and the explicit close control is reachable.
- [ ] Reading a long markdown reply — headings, lists, code, tables — is coherent, and code blocks
      announce as code. Tables currently carry no row/column semantics, so confirm what a table
      actually reads as.
- [ ] A failed reply can be retried, and its receipt opened, using only the turn's custom actions.

### Live device behaviour

- [ ] ProMotion: no dropped frames while streaming, scrolling, or opening the drawer.
- [ ] Apple On-Device execution on an eligible device with the network disabled produces a truthful
      on-device receipt.
- [ ] Backgrounding mid-stream and returning never duplicates a turn or claims false completion.
- [ ] The blind gauntlet: alternate between Wayfinder and each reference app on the same tasks and
      record every moment the reference app feels better, with the specific deficiency named.

## Findings

| ID | Severity | Surface/state | Finding | Owner | Disposition / verification |
|---|---|---|---|---|---|
| — | — | — | No findings recorded yet | — | Pending review |

## Release sign-off

- [ ] All required configurations were exercised against the final build.
- [ ] Screenshot references are attached and contain no private information.
- [ ] `apple-mobile-ci.yml` is green for the reviewed commit.
- [ ] No P0/P1 finding remains open.
- [ ] Any accepted P2 is documented with an owner and follow-up.
- [ ] `docs/mobile-ux-audit.md` dispositions are updated to match what was observed.
