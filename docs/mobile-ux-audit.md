# Wayfinder for iPhone/iPad UX audit — benchmarked against ChatGPT and Claude iOS

Status: advisory audit; not a release gate. Findings feed WF-ROADMAP-0016 phase planning.
Governing contracts: WF-DESIGN-0020 (mobile chat shell), WF-DESIGN-0019 (providers and credentials),
WF-ROADMAP-0016 (native mobile v0.2.0). Severity language mirrors `docs/desktop-fidelity.md`.

## Run metadata

| Field | Value |
|---|---|
| Date | 2026-07-25 |
| Audited commit | `6c5479cc66a90b8acc61386dda3ed28360994f6b` (`claude/ios-app-ux-audit-natkxk`) |
| App state | End of Phase 2 (deterministic provider) plus first Phase 3 slice (Keychain credentials) |
| iOS marketing version | 0.2.0 |
| Reference A | ChatGPT iOS app, light appearance, 31 s user-captured screen recording (2026-07-24) |
| Reference B | Claude iOS app, dark appearance, 25 s user-captured screen recording (2026-07-25) |
| Review method | Static SwiftUI review at the audited commit; frame-by-frame recording review at 1 fps |
| Reviewer | Claude Code (automated review session) |

## Executive summary

The shell is faithful to its own contract: chat owns the screen, the drawer stays out of the way, the
composer is always reachable, receipts stay quiet, and the deterministic build never pretends a live
provider answered. Structure, honesty, and restraint are all in place. What is missing is the layer
of conversational craft that makes ChatGPT and Claude feel finished: rendered markdown, message
actions, regenerate-in-place, scroll that respects the reader, input while streaming, onboarding, and
an accessibility pass. None of these compromise the routing-first thesis; they are the table stakes
underneath it.

Top findings (register IDs in section 9):

1. Retry appends a duplicate user turn instead of regenerating in place — misleading transcript
   state and the single largest behavioral divergence from both references (UX-001).
2. Streaming force-scrolls on every delta with no way to read earlier content, and the composer's
   text field is disabled while a response streams (UX-002, UX-008).
3. Assistant output is plain text — no markdown, code blocks, or syntax highlighting; both
   references treat rich rendering as baseline (UX-006).
4. VoiceOver users never learn that assistant text is Wayfinder speaking, nor that streaming
   started or finished (UX-003).
5. Composer controls are 32–34 pt, below the 44 pt HIG floor, and fixed frames are likely to break
   at accessibility text sizes — the contract explicitly forbids that (UX-004, UX-005).
6. Two visible affordances are dead: the "Wayfinder ⌄" title menu and the composer "+" menu
   (UX-009, UX-024).
7. The receipt copy shipped as "Routed to …" where the contract and roadmap prescribe the
   "Ran …" receipt grammar — the differentiator is present but under-realized (UX-010).

The benchmark here is interaction quality, not feature parity. Section 10 lists reference patterns
Wayfinder should continue to reject on purpose (model picker, plugin surface, fabricated capability
chips). The recommendation set in section 11 argues for pulling a handful of cheap accessibility and
truthfulness fixes forward from Phase 6 into the current phase, where they cost little and prevent
retrofits.

## Methodology and evidence conventions

- **Wayfinder evidence** is cited as `file:line` at the audited commit; all paths are relative to
  `ios/WayfinderIOS/WayfinderIOS/` unless another root is shown. Every citation was re-verified
  against the working tree at the audited commit.
- **Reference evidence** is cited as `[GPT m:ss]` or `[CLA m:ss]`, approximate offsets into the two
  recordings. The recordings contain the user's personal account data (name, address-book email,
  conversation titles), so no frames are committed to the repository and no on-screen personal
  content is quoted; observations are described structurally. This follows the desktop checklist's
  own screenshot-hygiene rule.
- Both recordings show current builds of the reference apps; specific model names and plugin lists
  visible in them are irrelevant to this audit — the interaction patterns are what is being
  compared.
- **Not evaluated:** live-device behavior (Dynamic Type at AX sizes, VoiceOver traversal, haptic
  feel, real streaming rates), because this audit is static. Items that need device confirmation
  are marked "verify on device". iPad-specific layout was reviewed in code only.

## Severity rubric

- **P0** — blocks use, risks data/credential exposure, or misstates an execution boundary
  (the mobile analog of the desktop one-gateway rule: on-device/hosted claims must be true).
- **P1** — broken primary workflow, inaccessible required action, clipping, misleading state, or
  lost recovery path. Would block a v0.2.0 release gate.
- **P2** — material polish or secondary-workflow defect. Recorded with a disposition and target
  phase.
- **P3** — benchmark-parity or refinement opportunity with no gate impact; feeds the phase backlog.

P1 wording matches `docs/desktop-fidelity.md` verbatim; P0 and P2 are deliberately adapted mobile
analogs of its wording; P3 is added because an audit records
opportunities a release gate would not track. **No P0 findings exist at this commit** — the
deterministic provider makes network-boundary violations impossible, credential custody is
Keychain-only with dedicated tests, and no copy claims a live provider responded.

## Reference pattern inventory

### ChatGPT iOS (Reference A)

- **Home / new chat** `[GPT 0:00–0:08]` — near-empty screen; three quiet action rows ("Create an
  image", "Write or edit", "Look something up") sit directly above the composer, not centered in
  the void; the composer is a floating pill with placeholder, "+" attach, a compact model badge,
  mic, and a voice-mode button. The send button only appears once text exists, morphing from the
  voice button.
- **Top bar** `[GPT 0:00, 0:20]` — three circular controls: leading drawer toggle, centered title
  that is itself a menu (workspace/profile switcher with checkmark), trailing new-chat. Every
  toolbar hit target is a generous circle.
- **Sidebar drawer** `[GPT 0:02–0:09]` — slide-over with scrim; wordmark + search at top; six
  destination rows with icons; a "Pinned" section; a long "Recents" list of conversation titles;
  pinned to the bottom, a prominent accent-colored "Chat" CTA and a settings gear. The drawer is
  the app's hub: navigation, history, and settings all live here.
- **Settings sheet** `[GPT 0:10–0:12]` — full-height sheet with X to close: avatar + editable
  name; grouped cards (Personalization / Memory / Plugins), Account (email, subscription tier,
  restore purchases, usage and limits), Theme (Appearance: System; accent color choice).
- **Attachment menu** `[GPT 0:21–0:26]` — "+" opens a compact anchored menu: Camera, Photos,
  Files, Plugins, Intelligence — each row icon-led; current mode carries a checkmark.
- **Inline "@" tool picker** `[GPT 0:28–0:30]` — typing "@" in the composer raises a panel of
  invokable tools above the keyboard with a filter control; arrow-key-free, tap-first design.

### Claude iOS (Reference B)

- **Home / new chat** `[CLA 0:00]` — warm dark surface; brand starburst above a serif first-name
  greeting; composer pill with placeholder, "+", and a combined model + effort chip
  ("‹model› · High"); mic and voice-mode buttons; the greeting gives the empty state personality
  without adding controls.
- **Model / effort sheet** `[CLA 0:01–0:03]` — the composer chip opens a bottom sheet: each model
  row pairs a name with a one-line plain-language description ("For your toughest challenges",
  "Most efficient for everyday tasks"), a checkmark on the active row, and a separate
  "Effort: High ›" row. Selection is one tap deep and self-explaining.
- **"Add to Chat" sheet** `[CLA 0:06–0:12]` — "+" opens a bottom sheet: Camera tile beside a
  horizontally scrolling recent-photos rail (with the iOS limited-photos permission prompt handled
  in place), then rows for Add files / Add to project / Tool access, then per-conversation feature
  toggles (Research off, Web search on), then Connectors. Capability control lives with attach, in
  one sheet.
- **Incognito mode** `[CLA 0:14–0:15]` — a persistent ghost button in the top bar swaps the whole
  screen into an incognito variant: title change, ghost glyph, and a three-line inline explainer of
  exactly what incognito does and does not persist, with a "Learn more" link. Privacy state is a
  visible mode, explained where it is toggled — not a buried setting.
- **Chats list** `[CLA 0:16–0:17]` — dedicated screen: filter icon, rows of title + relative
  timestamp ("51 minutes ago"), chevrons, a floating "+ New chat" pill bottom-trailing, and a
  search field at the bottom within thumb reach.
- **Sidebar** `[CLA 0:18–0:19]` — wordmark; destination rows (Chats, Projects, Artifacts, Code,
  Dispatch, Cowork with a "New" badge); Recents; bottom row with avatar and "+ New chat".
- **Code area** `[CLA 0:19–0:24]` — out of comparison scope (agent sessions), but notable for
  pattern language: session cards with repo chips and status glyphs, per-repo suggestion chips, and
  a mode pill ("Accept edits") inside the composer.

### Patterns observed but out of comparison scope

Voice conversation modes, image generation, plugins/connectors marketplaces, workspace/profile
switching, projects, memory, and Claude's Code/agent surfaces. These depend on capabilities
Wayfinder does not have and, per WF-DESIGN-0020, must not be imitated as fabricated capability.

## Wayfinder current-state inventory

Twelve Swift files, ~3,050 product lines, SwiftUI-only, iOS 18 floor, Observation-based state, Rust
routing core over UniFFI, deterministic mock provider, SwiftData persistence behind an actor.

| Surface | File | Notes |
|---|---|---|
| App entry | `WayfinderIOSApp.swift` | Storage-degradation notice seeded at init |
| Adaptive shell | `RootView.swift:9-129` | iPhone: custom drawer over `ZStack`; iPad: `NavigationSplitView` |
| Sidebar / drawer | `RootView.swift:132-297` | New chat, last-12 threads, All chats, Destinations, Settings, posture footer |
| Chat + transcript | `ChatTabView.swift:13-333` | Empty state, transcript, receipt chip, retry, status labels |
| Receipt detail | `ChatTabView.swift:335-373` | Medium-detent sheet; destination/runs/tier/score |
| Composer | `ChatTabView.swift:375-463` | Multiline field, "+" menu, Automatic label, privacy menu, send/stop |
| Threads | `ThreadsView.swift` | List + swipe-delete; `ContentUnavailableView` empty state |
| Destinations | `DestinationsView.swift` | Two static informational rows; no interaction |
| Settings | `SettingsView.swift` | API keys link, posture picker, runtime rows, retention/export/clear |
| API keys | `APIKeysView.swift` | Provider rows, badge, editor sheet, removal dialog |
| Theme | `WayfinderTheme.swift` | Single token: accent #15AB75 |

Design-system state: no asset catalog (and therefore no app icon, color sets, dark-accent or
increased-contrast variants); typography is all system text styles except one fixed 30 pt mark
(`ChatTabView.swift:162`); spacing and radii are per-call literals; no haptics; two animations
(drawer `RootView.swift:65`, autoscroll `ChatTabView.swift:54`) with no Reduce Motion guards; no
SwiftUI `#Preview` blocks; no reusable component layer beyond `WayfinderMark` and
`SidebarToolbarButton`.

## Journey-by-journey evaluation

### 1. First launch and onboarding

**References.** Both apps land signed-in users in a ready, self-explaining home; ChatGPT's action
rows and Claude's greeting each communicate "what this is" within one screen `[GPT 0:00]`
`[CLA 0:00]`.

**Wayfinder.** First launch drops into an empty chat with three generic suggestions
(`ChatTabView.swift:153-157`). Nothing explains what Wayfinder is, what routing means, or why the
app is different. The roadmap specifies a first-launch chooser ("Use Apple On-Device / Connect an
Account / Add an API Key / Connect a Mac", WF-ROADMAP-0016 "First launch") with a useful
no-destination state; none of it exists yet. Adding an API key today changes nothing outside the key
screens themselves — the provider row flips to "Saved" and Settings shows "1 saved", but no
destination, routing, or chat surface reflects it (`APIKeysView.swift:20-22` states this
honestly) — so the only configuration flow in the app ends without payoff.

**Gap.** Planned but absent; the honest footnote is good, the missing payoff is not. → UX-013.

### 2. Starting a new chat

**References.** ChatGPT: instant blank state, chips above the composer, new-chat always one tap
`[GPT 0:07]`. Claude: greeting + composer, new-chat available from list and sidebar
`[CLA 0:00, 0:17]`.

**Wayfinder.** Empty state is short, centred, and populates the composer (contract-compliant,
`ChatTabView.swift:150-186`); new chat exists top-trailing and in the drawer. Two defects: both
new-chat buttons and thread switching are silently disabled while a response streams
(`ChatTabView.swift:127`, `RootView.swift:154`, `AppModel.swift:462-479`) — the references never
lock navigation during generation — and suggestion chips sit mid-screen rather than adjacent to the
composer where ChatGPT anchors task entry. → UX-008 (shared with journey 3), UX-026.

### 3. Composing a message

**References.** Composer is a persistent pill; attach and capability controls are live; text entry
remains available while a response streams, queueing the next turn `[GPT 0:00–0:30]`
`[CLA 0:00–0:12]`. ChatGPT reveals send only when text exists; both use ≥44 pt circular controls.

**Wayfinder.** Anatomy matches the contract (field, Automatic label, privacy menu, send/stop —
`ChatTabView.swift:383-449`). Defects: the text field is `.disabled` during generation
(`ChatTabView.swift:396`), so a user cannot draft while streaming — worse than both references; the
"+" affordance opens a menu whose only item is disabled (`ChatTabView.swift:399-411`) — literal
contract compliance ("unavailable state is explicit and accessible") but a hollow interaction; controls
are 32–34 pt (`ChatTabView.swift:404, 427, 438`). → UX-008, UX-024, UX-004.

### 4. Receiving a response

**References.** Streamed text renders as markdown with code blocks; the reader can scroll up freely
while streaming continues; a scroll-to-bottom affordance returns to the tail; stop is immediate.

**Wayfinder.** Streaming works and stop/interruption/failure states are truthful and well-labelled
(`ChatTabView.swift:298-316` — genuinely better-explained terminal states than either reference).
Defects: every delta force-scrolls to the bottom via `onChange` on the last message
(`ChatTabView.swift:50-57`) with no "user scrolled away" detection and no scroll-to-bottom button —
the desktop checklist explicitly forbids exactly this ("later stream fragments do not steal the
selection or auto-scroll unless the user is following", `docs/desktop-fidelity.md:64-65`); assistant
output is plain `Text` (`ChatTabView.swift:250`) with no markdown, code, list, or link rendering; no
streaming cursor or content transition — the spinner vanishes at the first delta, and from then on
completion is signalled only by the composer's stop button reverting to send; every
delta also triggers a full-thread JSON re-encode and SwiftData save (`AppModel.swift:669-690,
742-754`) — at real provider token rates this is a per-token write storm with UI-jank risk.
→ UX-002, UX-006, UX-016, UX-021.

### 5. Acting on a message

**References.** Long-press or buttons expose copy/share/select/regenerate; regenerate replaces the
failed attempt in place; errors render inline with a retry that does not mutate history.

**Wayfinder.** Assistant text is selectable (`ChatTabView.swift:251`); user bubbles are not
(`ChatTabView.swift:211-228`). There is no copy button, share, context menu, or swipe action on any
message. Retry re-injects the prior prompt as a brand-new user message (`AppModel.swift:438-456`,
`draft = prompt; await sendMessage()`), so the transcript shows a duplicated user turn plus a second
answer — the persisted history now misrepresents what the user did. The covering test
(`WayfinderIOSTests/AppModelTests.swift:237-263`) asserts this duplication, so it is designed-in
rather than accidental. → UX-001, UX-007.

### 6. Understanding what ran (the flagship divergence)

**References.** ChatGPT shows a model badge in the composer `[GPT 0:00]`; Claude one-taps into a
model + effort sheet with plain-language per-model descriptions `[CLA 0:01–0:03]`. Both treat
"which brain answered" as a user-facing, glanceable fact.

**Wayfinder.** The equivalent surface is the route receipt — deliberately post-hoc rather than
pre-hoc, because routing is automatic. The chip (`ChatTabView.swift:255-271`) and detail sheet
(`ChatTabView.swift:335-373`) exist and work. Defects: chip copy reads "Routed to on this device"
where the contract prescribes the "Ran …" receipt grammar — "Ran on this iPhone · Apple
On-Device" is its on-device example, applicable once Apple execution exists (WF-DESIGN-0020
"Transcript and routing receipts"; WF-ROADMAP-0016 "Destination and receipt truth") — and "Ran"
is stronger language for the product's core claim; the sheet shows
score as a bare two-decimal number with no explanation, and omits the contract-owned reason codes,
fallback truth, and error recovery; the "Wayfinder ⌄" title menu — the surface the contract says
should expose the Automatic mode — contains only a permanently disabled button and static text
(`ChatTabView.swift:99-116`), reading as a broken model picker to anyone arriving from the
reference apps. Claude's model sheet is the pattern to borrow *for explanation, not selection*: one
tap → "why this destination", in words. → UX-010, UX-009, UX-017.

### 7. Finding and managing conversations

**References.** ChatGPT: pinned + recents + search in the drawer `[GPT 0:03–0:15]`. Claude: a full
chats screen with filter, relative timestamps, bottom search, floating new-chat `[CLA 0:16–0:17]`;
rename/pin/archive via long-press in both.

**Wayfinder.** Drawer lists the last 12 threads (`RootView.swift:169`) and an "All chats" screen
lists everything with swipe-delete (`ThreadsView.swift:50-57`). No search, rename, pin, or archive;
titles are the first ~49 characters of the first prompt, never revisited; timestamps are absolute
("Jul 25, 8:43 PM") where both references use relative time; the store fetch cap is 500 with no
pagination. Fine at Phase 2 volume, insufficient the week real usage starts. → UX-014, UX-022.

### 8. Controlling privacy

**References.** Claude's incognito is the standout: a visible mode with an in-place, plain-language
contract of what is and is not retained `[CLA 0:14–0:15]`. ChatGPT keeps data controls in settings.

**Wayfinder.** Structurally ahead: privacy posture is a first-class composer control and a sidebar
footer (`ChatTabView.swift:419-430`, `RootView.swift:238-257`), backed by real execution-boundary
semantics — something neither reference has. But the posture menu offers three titles with no
in-context explanation of consequences; the boundary summary lives one line deep in Settings
(`SettingsView.swift:24-34`). Borrow Claude's explainer *copy pattern* (state change → immediate
one-paragraph consequence, where the toggle happened), not the incognito feature. Retention,
export, and clear-all exist and are honestly captioned; export is an awkward two-step
("Prepare Export" then a separate share row, `SettingsView.swift:49-64`). → UX-018, UX-023.

### 9. Configuring the app

**References.** Settings are one sheet from the drawer: identity, subscription, theme, per-feature
groups `[GPT 0:10–0:12]`.

**Wayfinder.** Settings is a drawer destination with honest runtime rows and a clean API-key flow
(secure field, device-only Keychain copy, replace/remove with confirmation —
`APIKeysView.swift:116-187` is genuinely well-built and its accessibility treatment of the secure
field is exemplary). Defects: errors surface as blocking alerts ("Keychain",
`SettingsView.swift:101-117`; "Conversation storage", `RootView.swift:17-33`) with no inline
recovery; nine distinct persistence-failure strings exist with no retry affordance anywhere; and
the key flow ends with no payoff beyond the key screens (journey 1). → UX-015, UX-013.

## Cross-cutting evaluations

### Accessibility

Strengths worth keeping: correct modal drawer semantics (`RootView.swift:49, 61`), labelled
icon-only controls throughout, combined user-message elements (`ChatTabView.swift:225-227`), and a
secure field that reports "Empty"/"Entered" instead of content (`APIKeysView.swift:131-132`).

Gaps: assistant messages have no label identifying the speaker — `.contain` only
(`ChatTabView.swift:291`) versus the explicit "You" on user turns, so VoiceOver reads unattributed
text; no streaming lifecycle announcements (`.updatesFrequently` trait or
`AccessibilityNotification.Announcement`) — generation start/finish is invisible non-visually; tap
targets 32–34 pt (`ChatTabView.swift:404, 427, 438`); fixed frames (32/34 pt controls, 56 pt sidebar
header, 30 pt mark) with no `@ScaledMetric`, so AX text sizes will crowd or clip the exact controls
WF-DESIGN-0020 says Dynamic Type must never hide; the drawer has no explicit close control and its
scrim is `accessibilityHidden` (`RootView.swift:56`), leaving destination-selection as the only
assistive dismissal; no Reduce Motion guard on either animation; the accent (#15AB75) sits at
≈2.9:1 on white where it tints the receipt-chip icon (`ChatTabView.swift:261`) and the
caption-weight "Saved" badge text (`APIKeysView.swift:97-98`) — the badge text falls below the
4.5:1 small-text threshold and the icon is marginal against the 3:1 non-text threshold, partially
mitigated by paired labels.
→ UX-003, UX-004, UX-005, UX-011, UX-012, UX-019, UX-027.

### Visual design system and dark mode

One accent token is not a design system. The macOS target already has an 8-token theme with
semantic route colors (local teal / cloud amber); none of that language reached iOS, so route
boundary — the product's core concept — has no color semantics on the platform where it matters
most. Dark mode works via system semantic colors, but the accent literal does not adapt, and the
scrim/shadow literals (`RootView.swift:52`, `ChatTabView.swift:461`) are appearance-agnostic. No
asset catalog means no app icon — invisible in the simulator, disqualifying on a device. Claude's
recording demonstrates how much identity a dark surface + one serif face + one brand glyph can
carry; Wayfinder's equivalent assets (mark, accent, boundary glyphs) exist but are used timidly.
→ UX-011, UX-012, UX-025.

### Motion and haptics

Two animations, both fine, neither guarded for Reduce Motion. No haptics anywhere; the references
use light impact on send, selection changes, and mode switches. Send/stop swaps glyphs with no
`contentTransition`; streaming has no cursor. Motion is the cheapest place to close perceived
quality with the references. → UX-016, UX-019, UX-026.

### Perceived performance

The mock provider hides two real problems: per-delta full-thread re-encode + dual persistence
writes (`AppModel.swift:669-690, 742-754`) and per-delta forced autoscroll — at 30–80 tok/s both
will churn. The transcript's `LazyVStack` contains a single plain `VStack` child
(`ChatTabView.swift:25-44`, `188-209`), so all messages build eagerly; long threads pay full cost
on every switch. → UX-021, UX-002.

## Findings register

| ID | Sev | Surface / journey | Finding | Evidence | Phase |
|---|---|---|---|---|---|
| UX-001 | P1 | Chat / retry | Retry duplicates the user turn instead of regenerating in place; persisted history misrepresents user actions | `AppModel.swift:438-456`; asserted by `AppModelTests.swift:237-263` | Now (pre-Phase 3) |
| UX-002 | P1 | Chat / streaming | Every delta force-scrolls to bottom; no scrolled-away detection, no scroll-to-bottom control; reading position is unrecoverable while streaming | `ChatTabView.swift:50-57`; rule at `docs/desktop-fidelity.md:64-65` | Now |
| UX-003 | P1 | Accessibility | Assistant messages carry no speaker identification and streaming has no start/finish announcements for VoiceOver | `ChatTabView.swift:291` vs `:225-227` | Now |
| UX-004 | P1 | Composer | "+", privacy, and send controls are 32–34 pt, below the 44 pt HIG minimum | `ChatTabView.swift:404, 427, 438` | Now |
| UX-005 | P1 | Accessibility | Fixed-frame chrome (controls, 56 pt header, 30 pt mark) has no Dynamic Type scaling; contract requires send/privacy/navigation to survive AX sizes — verify on device | `ChatTabView.swift:162, 404, 427, 438`; `RootView.swift:273`; WF-DESIGN-0020 accessibility rules | Now |
| UX-006 | P2 | Chat / rendering | No markdown, code block, list, link, or table rendering; assistant output is plain `Text`. Becomes P1 the day a live provider ships | `ChatTabView.swift:250` | Phase 3 |
| UX-007 | P2 | Chat / actions | No copy, share, or context menu on messages; user bubbles not even selectable | `ChatTabView.swift:211-228, 250-251` | Phase 3 |
| UX-008 | P2 | Composer / navigation | Text field disabled while streaming; new chat and thread switching also locked during generation with no feedback | `ChatTabView.swift:396, 127`; `AppModel.swift:462-479` | Phase 3 |
| UX-009 | P2 | Chat / title | "Wayfinder ⌄" title menu contains only a permanently disabled item plus static text — a dead affordance where the contract wants the Automatic mode exposed | `ChatTabView.swift:99-116` | Now |
| UX-010 | P2 | Receipts | Receipt chip copy "Routed to …" diverges from the contract's prescribed "Ran …" receipt grammar; weaker execution-boundary language | `ChatTabView.swift:262`; WF-DESIGN-0020; WF-ROADMAP-0016 | Now |
| UX-011 | P2 | Design system | Accent #15AB75 at ≈2.9:1 on white tints the receipt-chip icon and caption-weight badge text; no dark/increased-contrast variant possible without an asset catalog | `WayfinderTheme.swift:4-8`; `ChatTabView.swift:261`; `APIKeysView.swift:97-98` | Phase 6 |
| UX-012 | P2 | Design system | No asset catalog: no app icon, color sets, or appearance variants | `project.yml:29` | Phase 6; icon gates Phase 8 |
| UX-013 | P2 | Onboarding | No first-launch experience; API-key flow has no payoff beyond the key screens (no destination, routing, or chat change) | roadmap "First launch"; `APIKeysView.swift:19-23` | Phase 3 |
| UX-014 | P2 | Threads | No search, rename, pin, or archive; naive 49-char titles; absolute timestamps; 500-thread fetch cap unpaginated | `ThreadsView.swift`; `ConversationStore.swift:44-55, 190`; `RootView.swift:169` | Phase 6 |
| UX-015 | P2 | Errors | Failures surface as blocking alerts with no inline recovery; nine distinct persistence-failure strings, zero retry affordances | `RootView.swift:17-33`; `SettingsView.swift:101-117`; `AppModel.swift:751-752` | Phase 6 |
| UX-016 | P2 | Streaming | No streaming cursor or content transition; spinner vanishes at first delta, leaving the composer's stop→send swap as the only completion signal | `ChatTabView.swift:238-252, 432-434` | Phase 3 |
| UX-017 | P2 | Receipts | Score shown as bare decimal with no explanation; sheet omits contract-owned reason codes, fallback truth, and error recovery | `ChatTabView.swift:346-349`; WF-DESIGN-0020 receipts | Phase 6 |
| UX-018 | P2 | Privacy | Posture menu offers three titles with no in-context consequence explanation (Claude's inline-explainer copy pattern) | `ChatTabView.swift:419-430`; `SettingsView.swift:24-34` | Phase 6 |
| UX-019 | P2 | Motion | No Reduce Motion guards on drawer or autoscroll animations | `RootView.swift:65`; `ChatTabView.swift:54` | Now |
| UX-020 | P2 | Navigation | Per-section `NavigationStack` state rebuilt on every drawer switch; roadmap and ADR require independent per-section stacks, and the contract's permitted hidden-container mechanism is unused | `RootView.swift:68-86, 98-116`; WF-ROADMAP-0016 information architecture; WF-ADR-0047 | Phase 6 |
| UX-021 | P2 | Performance | Per-delta full-thread JSON re-encode + dual persistence writes; transcript defeats `LazyVStack` laziness | `AppModel.swift:669-690, 742-754`; `ChatTabView.swift:25-44, 188-209` | Phase 3 |
| UX-022 | P3 | Threads | Relative timestamps ("51 minutes ago") in lists, as both references use | `ThreadsView.swift:36-41` | Phase 6 |
| UX-023 | P3 | Settings | Two-step export ("Prepare" then "Share") where one `ShareLink` would do | `SettingsView.swift:49-64` | Phase 6 |
| UX-024 | P3 | Composer | Dead "+" menu: replace with an inert labelled state or hide until attachments exist | `ChatTabView.swift:399-411` | Phase 3 |
| UX-025 | P3 | Design system | No spacing/radius/type token scale; no route-boundary color semantics carried from macOS theme | `WayfinderTheme.swift`; `macos/.../WayfinderTheme.swift` | Phase 6 |
| UX-026 | P3 | Polish | No haptics; no send-button content transition; empty-state chips mid-screen rather than composer-adjacent; redundant `navigationTitle` declaration superseded by the principal toolbar item | `ChatTabView.swift:92, 106-112, 150-186, 432-445` | Phase 6 |
| UX-027 | P2 | Accessibility / drawer | Drawer offers no explicit close control; the scrim is hidden from assistive tech, leaving destination selection as the only assistive dismissal | `RootView.swift:51-62` | Now |

## Intentional divergences to keep

| Reference pattern | Wayfinder position | Governing rationale |
|---|---|---|
| Model picker / model badge (both apps) | No user-facing model selection; Automatic routing is the product. Expose the decision *after* the fact (receipt) and the mode *label* before it | WF-DESIGN-0020; WF-ADR-0001 lineage; WF-DESIGN-0012 — "the decision is the product" |
| Claude incognito mode | Keep privacy postures (stronger: execution boundaries, not retention flags); borrow only the in-place explainer copy pattern | WF-ADR-0047 privacy postures |
| Capability chips ("Create an image", Research/Web search toggles, "@" tools) | Do not fabricate; suggestions must stay within real capability | WF-DESIGN-0020: "must not fabricate provider capability" |
| Voice modes, plugins, projects, memory, profile switching | Out of v0.2.0 scope entirely | Not in WF-ROADMAP-0016's v0.2.0 scope |
| Settings-as-sheet with account identity | Wayfinder has no account; keys-in-Keychain framing is correct and more honest | WF-DESIGN-0019 |
| Permanent routing dashboard (no reference has one either) | Receipts stay compact and post-hoc | WF-DESIGN-0020: no routing dashboard |

## Prioritized recommendations

**Pull forward to now (pre-Phase 3, small):** the audit's core argument is that nine P1/P2
register items (eight small fixes) are cheap at 3k lines and expensive later — fix retry to regenerate in place reusing the failed
message's slot (UX-001); gate autoscroll on bottom-proximity and add a scroll-to-bottom pill
(UX-002); add `accessibilityLabel("Wayfinder")` + streaming announcements to assistant messages
(UX-003); lift the three composer controls to 44 pt and adopt `@ScaledMetric` (UX-004/005); correct
receipt grammar to the contract's "Ran …" form (UX-010); guard both animations with
`accessibilityReduceMotion` (UX-019); give the drawer an explicit close control for assistive
users (UX-027); either make the title menu explain Automatic routing in one sentence or remove the
chevron affordance (UX-009). None of these touch provider scope.

**Phase 3 (with the first live provider):** markdown + code rendering (UX-006) — this is the
biggest single quality gap against both references and becomes P1 with real output; message
copy/context menus (UX-007); allow drafting while streaming, unlock navigation during generation
(UX-008); streaming cursor + `contentTransition` (UX-016); move per-delta persistence to
checkpointed writes (UX-021); replace the dead "+" (UX-024); first-launch chooser per roadmap and a
visible payoff for a saved key (UX-013).

**Phase 6 (planned polish, confirmed by this audit):** thread search/rename/pin + relative
timestamps (UX-014/022); inline error recovery (UX-015); one-step export via `ShareLink` (UX-023);
receipt reason codes and score explanation (UX-017); posture consequence copy (UX-018); per-section stack preservation (UX-020); token scale,
route-boundary color semantics, dark-adaptive accent, asset catalog + icon (UX-011/012/025);
haptics and micro-motion (UX-026).

**Phase 8:** app icon and appearance-variant assets become release-blocking if not done earlier
(UX-012).

## Appendix A — WF-DESIGN-0020 compliance matrix

| Contract clause | Status | Evidence |
|---|---|---|
| Chat is the default and dominant screen | Compliant | `AppModel.swift:135` (`selectedTab = .chat`); `RootView.swift:69-86` |
| Leading button opens slide-over drawer | Compliant | `RootView.swift:44-66, 299-310` |
| No persistent bottom tab bar | Compliant | No `TabView` in target |
| New chat in top trailing and drawer | Compliant | `ChatTabView.swift:118-129`; `RootView.swift:142-155` |
| Title exposes Automatic mode without implementation detail | Partial | `ChatTabView.swift:99-116` — surface exists but is a dead menu (UX-009) |
| Empty state short, centred, useful; no fabricated capability | Compliant | `ChatTabView.swift:150-186` |
| Composer via `safeAreaInset`, neutral surface, multiline, compact controls | Compliant | `ChatTabView.swift:66-90, 375-463` |
| Attachment affordance only with explicit accessible unavailable state | Partial | `ChatTabView.swift:399-411` — hint present; interaction shape is a dead menu (UX-024) |
| Hidden `TabView`/equivalent may preserve per-section stack state | Not adopted | `RootView.swift:68-86, 98-116` — permitted mechanism unused; independent stacks are mandated by WF-ROADMAP-0016 and WF-ADR-0047 (UX-020) |
| iPad `NavigationSplitView`, no third column, receipt in sheet | Compliant | `RootView.swift:88-96`; `ChatTabView.swift:131-135` |
| Collapse produces iPhone model | Compliant (by construction) | `RootView.swift:10-15`; verify on device |
| Quiet trailing user bubble; assistant reads as transcript | Compliant | `ChatTabView.swift:211-229, 231-252` |
| Receipt copy uses the "Ran …" grammar | Violated | `ChatTabView.swift:262` — "Routed to on this device" (UX-010) |
| Receipt detail owns boundary, tier, score, reason codes, fallback truth, recovery | Partial | `ChatTabView.swift:341-360` — no reason codes, fallback, or recovery (UX-017) |
| Deterministic provider visibly bounded; copy never implies live provider | Compliant | `ChatTabView.swift:352-360`; `ChatExecution.swift:43-52` |
| Composer contract (field, Automatic label, privacy menu, gated send) | Compliant | `ChatTabView.swift:383-449`; `AppModel.swift:204-207` |
| Send/Stop labels; no concurrent submit | Compliant | `ChatTabView.swift:448`; `AppModel.swift:204-207` |
| System materials; no strong permanent green outline | Compliant | `ChatTabView.swift:453-461` — hairline is `primary.opacity(0.08)` |
| Drawer modal to AT; obscured content unfocusable | Compliant | `RootView.swift:48-49, 61` |
| Icon-only controls labelled | Compliant | throughout; drawer lacks an explicit close control (UX-027) |
| Receipt rows combine into one reading unit | Partial | sheet rows are separate `LabeledContent` elements (`ChatTabView.swift:341-360`) |
| Dynamic Type never hides send/privacy/navigation | Unverified — likely violated | fixed frames `ChatTabView.swift:404, 427, 438` (UX-005); verify on device |
| Keyboard dismissal preserves draft | Compliant | `ChatTabView.swift:64, 136-138` |
| New chat clears only transient state, returns to Chat | Compliant | `AppModel.swift:462-474` |

## Appendix B — related documents

- `designs/WF-DESIGN-0020-mobile-chat-shell.md` — governing shell contract
- `designs/WF-DESIGN-0019-mobile-providers-accounts-and-credentials.md` — readiness vocabulary the
  destination/receipt UI will need
- `roadmaps/WF-ROADMAP-0016-native-mobile-v0.2.md` — phase plan and acceptance criteria
- `decisions/WF-ADR-0047-native-mobile-independence.md` · `WF-ADR-0048` · `WF-ADR-0049`
- `docs/desktop-fidelity.md` — severity model and the macOS analog of this review
- `docs/apple-platform-capability-matrix.md` — capability truth table behind boundary language
- `.agents/skills/wayfinder-codex-ux-review/` — reusable UX principles (calm density, color has
  meaning, explicit state, reduced-motion-aware motion)
