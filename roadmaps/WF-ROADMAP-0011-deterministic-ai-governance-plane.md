---
schema_version: 1
id: WF-ROADMAP-0011
type: roadmap
status: superseded
updated: 2026-08-22
tags: [governance, policy, identity, scim, agents, fleet, mdm, chargeback, audit, compliance, enterprise]
---

# Roadmap: the deterministic AI governance plane — embedding Wayfinder in the 50–2500-person organization

## Status

Superseded on 2026-08-22 by WF-ADR-0073 and WF-ROADMAP-0017. This document
remains as historical design context; it is not an active source of product
scope or pull-request priority.

## Context

WF-ROADMAP-0010 answers "can I trust this router?" — and stops there. A critical pass of it, taken
with the 50–2500-employee organization as the unit of adoption, finds its ceiling is structural,
not a matter of execution:

- **Routing value is bounded by the LLM bill, and the bill deflates.** A router — even a proven
  one — is a cost tool bought by one platform engineer, optimizing a spend pool that model-price
  deflation shrinks every year. The evidence engine makes the router trustworthy; it does not
  change what the router is worth.
- **The traffic never arrives.** The gateway sees only what is deliberately pointed at it via
  `base_url` swaps, and our own catalog admits the limits (Cursor and VS Code Copilot honor a
  custom base URL for chat panels only — `docs/integrations.md:82`). In a real organization, AI
  usage fragments across editors, chat tools, SDKs, and a fast-growing population of autonomous
  agents. A gateway nobody's traffic reaches governs nothing.
- **There is no *person* anywhere in the system.** Virtual keys are flat shared secrets in a TOML
  file; `VirtualKey.tags` is parsed (`gateway.py:638`) and serialized (`gateway.py:858`) but
  consumed by no line of code; *team*, *department*, and *organization* do not exist as concepts.
  Every enforcement verb in the delivery pipeline is cost-shaped — pin, degrade, block(402),
  throttle, clamp (`gateway.py:1908–2028`). There is no content-based verb at all; the PII gate is
  an unbuilt proposal (WF-ROADMAP-0008).

The question a 50-person company asks — and a 2500-person company asks louder — is not "which tier
deserves this prompt?" It is **"can we let our people and our agents use AI at all, and prove what
happened when they did?"** Today organizations pick between blocking AI tools (losing the
productivity) and tolerating shadow AI (losing the data). The order-of-magnitude capability is
Wayfinder becoming the **policy enforcement point every AI request in the organization flows
through**: every employee and every autonomous agent carries an identity; every request is
evaluated by deterministic policy-as-code, where `route` is just one verb alongside `block`,
`redact`, `warn`, and `log`; every decision is attributable, replayable, and auditable
byte-for-byte. Routing and its savings ledger stop being the product and become the wedge that
pays for the install.

This is Wayfinder-shaped, not a pivot. Every competitor in AI governance puts an ML classifier in
the hot path — opaque, latency-heavy, and often shipping the organization's prompts to the
vendor's cloud in order to "protect" them. Wayfinder's founding invariant (WF-ADR-0001: offline,
deterministic, sub-millisecond, no model call) is precisely the property a CISO and an auditor
want in a policy engine: **it can prove what it did**, and it can do so without the data leaving
the building. The per-identity gates already in the delivery pipeline — budget-degrade
(WF-ADR-0032) and allowlist-clamp (WF-ADR-0035) — are the structural precedents the policy engine
generalizes.

Honesty about what this reverses. This roadmap **repeals, explicitly**: WF-ROADMAP-0010's non-goal
"no multi-tenant org hierarchy or SCIM" (annotated at source); WF-ROADMAP-0009's macOS-only and
no-fleet posture for the desktop client; and WF-ADR-0042's keyless/loopback-only constraint *for
the new enrollment mode* (the personal loopback mode remains, unchanged, the default). It **keeps,
deliberately**: no transparent or kernel-level traffic interception (WF-ROADMAP-0007's non-goal —
managed configuration is auditable and consent-shaped; interception is neither); no telemetry to
any vendor, ever — metadata flows only to the organization's *own* gateway; no hosted SaaS; and
WF-ADR-0001, whose CI guard every initiative below must keep green. WF-ROADMAP-0010 is not
superseded — its evidence engine remains the trust wedge, and its Track B substrate (shared state,
OIDC, audit, Helm) is **promoted from "table stakes" to load-bearing prerequisite**.

## Outcomes

- An organization can write its AI policy as **versioned, testable, deterministic configuration**
  — "prompts with credentials never leave the building," "the finance team routes to approved
  models only," "this agent may spend $50/day" — and every request either passes, is redacted,
  is re-routed, or is blocked with a message, in under a millisecond, with no model call.
- **Every request has an owner.** People are provisioned and deprovisioned from the identity
  provider (SCIM); autonomous agents hold their own keys with a human owner, a budget, and a
  kill-switch. Leaving the company revokes AI access with the same offboarding motion as email.
- **A fleet of laptops enrolls without hand-editing.** An MDM-distributed agent (grown from the
  existing desktop client) carries the person's identity, points their AI tools at the org
  gateway, and shows them what is governed — and what is not.
- **Spend and usage roll up the org chart**: person → team → department, with chargeback exports
  finance can consume, and a coverage report that names the AI tools on enrolled machines that
  are *not* yet governed — the honest, no-interception answer to shadow-AI discovery.
- **Compliance becomes an artifact, not an assertion**: a tamper-evident audit chain, any logged
  decision replayable byte-for-byte from its policy version, and a quarterly report mapped to the
  control language auditors actually use.
- The claims regenerate in public, in the house register: **published precision/recall for every
  detector** (regex detection is recall-imperfect and the tables say so), a policy-latency gate in
  CI, and an org-rollout playbook a platform lead can follow without reading source.

## Initiatives

### 1. The policy engine: deterministic policy-as-code

The keystone. A pure-core module (`wayfinder_router/policy.py`, stdlib-only, unit-tested in the
`sufficiency.py` mold) that evaluates ordered rules from a `[policy.rules.<name>]` TOML table:
**match conditions** — detector hits (secrets/PII patterns riding the `Lexicon` seam proposed in
WF-ROADMAP-0008, generalized into a named detector set), score bands, the requesting identity and
its team/tags, source app, requested model — resolving to a **verb**: `route` / `pin` / `degrade`
/ `block` (a structured 403 carrying the operator's message) / `redact` (deterministic
pattern-based redaction of the forwarded copy) / `warn` (deliver, flag in headers and audit) /
`log`. Routing, it turns out, was always just the first verb.

- **Slots into the existing gate chain.** Evaluation runs in `chat_completions` after auth
  resolves the key (`gateway.py:1871`) and after scoring (`gateway.py:1933`), alongside the two
  per-identity policy precedents already there: budget enforcement (`gateway.py:1975`) and the
  allowlist clamp (`gateway.py:2019` — whose own comment calls it "the final word on the route";
  the policy engine takes that seat, with the clamp folded in as a generated rule).
- **Versioned and visible.** The active policy's content hash is stamped on every response
  (`x-wayfinder-policy`, plus `x-wayfinder-policy-rule` naming the deciding rule) and into the
  audit record — the same family as the existing decision headers (`gateway.py:2029`).
- **Testable before it is live.** `wayfinder policy test` runs the rule set against golden
  fixtures (the `tools/golden.py` idiom); `wayfinder policy explain <prompt-file>` prints the
  full match trace. Rules ship in `log`-only mode first — observe, then enforce.
- **Deterministic redaction, honestly scoped.** `redact` rewrites the *forwarded* body with the
  matched pattern's replacement; the decision metadata records that redaction occurred and by
  which rule. No ML rewriting, no semantic paraphrase — pattern replacement only, and the
  detector benchmarks (Initiative 6) publish exactly how much that catches.

**Acceptance:**

- Same request + same policy version → byte-identical decision and headers (replay test).
- Policy evaluation adds < 1ms p99 over the canonical `dataset.jsonl` at fleet request shapes
  (CI-gated benchmark).
- An absent or empty `[policy]` table leaves every existing test green and behavior
  byte-identical to today (regression).
- Every verb is regression-tested on both streaming and non-streaming paths; `block` returns the
  structured 403 with the rule's message on both.
- `redact` alters only the forwarded body; score, ledger, and recent-decision metadata remain
  consistent, and the unredacted text is never written anywhere.
- The WF-ADR-0001 guard stays green: policy evaluation is offline, deterministic, model-call-free.

### 2. The identity plane: people and agents

Identity today is a shared secret you hand out. This initiative binds keys to the organization's
directory and gives non-human callers first-class, ownable, killable identities.

- **SCIM 2.0 provisioning.** A `/scim/v2` surface (operator-authenticated, per WF-ROADMAP-0010's
  OIDC work) through which the IdP creates, updates, and deactivates users; each user maps to a
  per-person virtual key minted at enrollment (Initiative 3) — deactivation revokes it on the
  next sync. Runtime key CRUD lands with it, closing the gap WF-ADR-0035 explicitly deferred
  (config-file-only key management).
- **The org chart enters the ledger.** A `[gateway.teams.<id>]` table (members, parent) gives
  person → team → department; `VirtualKey.tags` — parsed and round-tripped since WF-ADR-0035 but
  consumed nowhere (`gateway.py:638,858`) — finally acquires a consumer: policy match conditions
  (Initiative 1) and ledger attribution (Initiative 4).
- **Agents are identities, not exceptions.** `kind = "agent"` keys carry an `owner` (a person or
  team key), their own budget and rate limit (the machinery exists per-key already —
  `gateway.py:441–456`), and two agent-specific controls: `wayfinder keys kill <id>` — immediate
  fleet-wide 403 plus an audit record, no config edit, no restart — and a **runaway tripwire**:
  rate-of-spend over a rolling window, reusing WF-ROADMAP-0010 Initiative 3's tripwire state
  machine, tripping to `kill` or `degrade` per policy. The agentic traffic explosion is governed
  by the same plane as the humans, with tighter defaults.

**Acceptance:**

- SCIM deactivate revokes the person's key within one sync cycle; the next request 401s on every
  replica (shared state via WF-ROADMAP-0010's Redis backend).
- Ledger rollups reconcile exactly: sum of person spend = team; sum of teams = department.
- `keys kill` takes fleet-wide effect in < 5s, leaves exactly one audit record, and survives
  config hot-reload.
- A mock agent with a spend spike trips its runaway tripwire within the configured window; the
  trip is sticky until an operator clears it (the Initiative 3 discipline from WF-ROADMAP-0010).
- Existing static `[gateway.keys]` entries continue to work byte-identically (regression).

### 3. Fleet embedding: the enrollment agent

The capture answer, without interception. The desktop client — today a real but minimal Tauri
menu-bar scaffold (`clients/desktop/src-tauri/src/lib.rs`), keyless and loopback-only by design
(WF-ADR-0042) — grows an **enrollment mode**: MDM-distributable, identity-carrying, and able to
point the machine's AI tools at the organization's gateway. The personal loopback mode remains
the default for individuals; enrollment is the org-deployed configuration of the same binary.

- **Enroll via the IdP.** OIDC device flow → the person's virtual key, held in the OS keychain
  (the `api_key_cmd`/Keychain seam WF-ADR-0042 already specifies). The shared wire client
  (`clients/shared/src/gateway.js`) gains the `Authorization` header and a configurable org
  `baseUrl` — the two deliberate absences of the personal mode.
- **Auto-configure the tools we already catalog.** The integrations catalog
  (`docs/integrations.md`) becomes the config-writer's target list: `OPENAI_BASE_URL` /
  `ANTHROPIC_BASE_URL` in shell profiles, per-app settings for Claude Code, Continue, aider, and
  the SDK env pair — written on enroll, restored byte-for-byte on unenroll. What a tool cannot
  redirect (Copilot autocomplete, Cursor's non-chat paths) is *reported*, not intercepted — it
  feeds Initiative 4's coverage report.
- **Fleet packaging.** Signed pkg + MDM profile on macOS first (the signing/notarization/updater
  posture WF-ADR-0042 §7 already specifies), MSI + Windows and Linux following — repealing
  WF-ROADMAP-0009's macOS-only non-goal for this mode.
- **The employee sees what the org sees.** The popover shows: enrolled as whom, which tools are
  governed, what today's routing saved — and nothing leaves the machine except requests to the
  org's own gateway. "No telemetry, ever" survives intact: the vendor (us) receives nothing.

**Acceptance:**

- Fresh machine + MDM push → first governed, identity-attributed request with zero manual file
  edits (end-to-end walkthrough test, macOS).
- Unenroll restores every touched config byte-for-byte and removes the key from the keychain.
- Every request from an enrolled machine carries the person's identity; none carries a shared
  org secret.
- The same binary, unenrolled, still works against a personal loopback gateway exactly as today
  (regression for individuals).

### 4. The org observatory: attribution, coverage, chargeback

- **Team and department become ledger dimensions.** The `SavingsLedger` already buckets per-day
  × per-route × per-key (`pricing.py:130`); teams add a parallel rollup by the same `by_key`
  merge pattern (`pricing.py:236`) — an additive extension of the existing machinery, not a
  rewrite. `/v1/savings` and the dashboard gain `by_team` / `by_department` views,
  operator-authenticated.
- **The coverage report — shadow AI, honestly.** Enrolled agents report which known AI tools are
  present and which are pointed at the gateway. The observatory renders governed / ungoverned /
  unknown per tool per department — naming the gap instead of pretending interception closed it.
- **Chargeback that finance can open.** WF-ROADMAP-0010 Initiative 4's FOCUS-compatible CSV
  export gains team/department columns; a monthly showback statement per department renders from
  the same data, offline, no CDN — the evidence-report artifact discipline applied to money.

**Acceptance:**

- Department rollups reconcile exactly with per-key sums across a synthetic org fixture.
- The export loads into standard FinOps tooling unmodified (column-contract test).
- The coverage report distinguishes governed / ungoverned / unknown per tool, and never claims
  coverage it cannot see.
- All org views 401 without operator OIDC; per-employee views show only that employee.

### 5. The compliance evidence pack

WF-ROADMAP-0010's evidence engine proves *routing quality*. This extends the same discipline to
the question auditors ask: **was the policy enforced?**

- **A tamper-evident audit chain.** WF-ROADMAP-0010's append-only `wayfinder-audit.jsonl` is
  upgraded to a hash chain (each record carries the previous record's hash); `wayfinder audit
  verify` walks it. Policy changes, key lifecycle events, kills, tripwire trips, and every
  `block`/`redact`/`warn` land in it — with actor, policy version, rule name; never prompt text.
- **Deterministic replay as the audit answer.** `wayfinder audit replay <request-id>` re-derives
  the decision byte-for-byte from the stored metadata and the referenced policy version — the
  policy engine's determinism (Initiative 1) turned into the compliance feature no ML-classifier
  competitor can offer.
- **The quarterly artifact.** A compliance report (policy coverage, violations blocked,
  redactions, agent kills, exceptions, per department) rendered offline in the evidence-report
  mold, plus `docs/compliance-mapping.md` mapping shipped controls — only shipped ones, no
  aspirational rows — to EU AI Act, ISO/IEC 42001, and SOC 2 control language.

**Acceptance:**

- Chain verification detects any single-record modification or deletion (tamper test).
- 100% of logged decisions replay byte-identically from policy version + stored metadata.
- The report renders fully offline; the mapping doc contains no control not covered by a shipped,
  tested feature.

### 6. Detector benchmarks and the org rollout playbook

The governance claims get the `blind-eval.md` treatment before anyone is asked to trust them.

- **Published detector precision/recall.** A public corpus of secret/PII shapes (API-key formats,
  credentials, personal identifiers — plus adversarial negatives) with per-detector
  precision/recall tables in `benchmarks/detector-validation.md`, regenerated per release.
  Pattern-based detection is recall-imperfect by construction; the tables say exactly how much,
  per detector — the same honesty that published a below-random skill number.
- **Policy latency as a CI gate.** The policy path joins the existing sub-millisecond decision
  gate (`benchmarks/harness.py` `latency_us` machinery) at fleet request shapes.
- **`docs/org-rollout.md` — the playbook.** Pilot department → policy in `log`-only mode →
  review the would-have-blocked report → enforce → widen. Includes the privacy posture in plain
  language for works councils and DPOs: metadata-only by default (WF-ADR-0011/0014), prompts
  never leave the organization's own infrastructure, and the employee-visible popover shows
  exactly what is collected.

**Acceptance:**

- Detector P/R tables regenerate bit-for-bit offline; a release is blocked if regenerated numbers
  differ from committed ones.
- The policy-latency gate runs in CI and fails on regression past the floor.
- A fresh operator following `docs/org-rollout.md` takes a demo org from unenrolled to
  enforced-policy with chargeback, without touching Python internals (walkthrough-tested per
  release).

## Alternatives Considered

### An agent-fleet-only control plane

Govern autonomous agent traffic first — per-agent identity, kill-switches, runaway detection —
and leave human tooling for later. The agentic explosion is real, but agents are just non-human
identities under the same policy engine; going agent-only rebuilds the person-shaped gap this
roadmap exists to close. Subsumed as Initiative 2's second half.

### Deepening the evidence engine into a model-portfolio autopilot

Continuous fleet-wide evaluation of the whole model market, auto-recommending portfolio changes.
A genuine 100x for the platform team — and still bounded by the LLM bill. It remains
WF-ROADMAP-0010's natural continuation, not the organizational capability.

### Network-layer capture (PAC files, egress interception, DNS discovery)

The strongest-sounding CISO pitch and the wrong one for this project: WF-ROADMAP-0007 already
names transparent interception a non-goal, and the reasoning holds — managed configuration is
auditable and consent-shaped; interception is neither, and it would put Wayfinder inside traffic
it has promised not to see. The coverage report (Initiative 4) names the gap honestly instead.

### Adopting OPA/Cedar as the policy engine

A proven external policy engine instead of a native one. Rejected: a heavyweight dependency and a
foreign DSL grafted onto a stdlib-only pure core whose entire configuration idiom is TOML — and
Wayfinder's policy value *is* its in-register determinism and explainability (`explain_score`,
golden fixtures, byte-replay). The rule table is deliberately small; the engine is the product's
own.

### A hosted governance SaaS

The largest revenue surface and the clearest self-contradiction: a governance plane's trust story
is that the organization's prompts and metadata never leave its infrastructure. Self-hosted is
not a limitation here; it is the pitch.

**The tiebreaker.** Only the governance plane changes who the buyer is (platform engineer →
CIO/CISO), what the value is bounded by (the model bill → the organization's entire AI adoption
and its risk), and what the traffic is (one team's opt-in → every employee's and every agent's
requests). And it is the one direction where Wayfinder's founding constraint — deterministic,
offline, no model call — stops being a routing implementation detail and becomes the product's
central compliance claim.

## Non-goals (explicit, for this roadmap)

- **No transparent or kernel-level traffic interception** — kept from WF-ROADMAP-0007. Managed,
  auditable, reversible configuration only; what can't be redirected is reported as ungoverned.
- **No ML classifiers on the request path.** Detectors are deterministic patterns with published
  precision/recall. An async, off-path advisory scanner behind the `Judge`-style seam may come
  later, opt-in and explicitly labelled — never in the blocking path, and if it is ever model-backed
  it runs as a **local, in-container model with no egress** (WF-ADR-0043), never a call to an
  external provider — the same constraint the governance pitch depends on.
- **No telemetry to any vendor, ever.** The enrollment agent reports to the organization's own
  gateway and nowhere else. "No telemetry" (WF-ROADMAP-0009) survives with its meaning intact.
- **No general DLP/CASB/SSE ambitions.** LLM traffic only — no file scanning, no email, no SaaS
  app inventory beyond the AI tools the agent knows.
- **No detection-completeness guarantee.** Pattern detection is recall-imperfect; the benchmark
  tables quantify it per detector, in the `blind-eval.md` register.
- **No hosted control plane.** Self-hosted, on the organization's infrastructure, as ever.
- **No change to WF-ADR-0001.** Policy evaluation, identity checks, and every verb are offline,
  deterministic, and model-call-free; the CI guard extends to the policy path.

## Success Measures

- **Time-to-governed:** a pilot department goes from MDM push to identity-attributed,
  policy-logged traffic in ≤ 1 day, following only `docs/org-rollout.md`.
- **Coverage:** ≥ 80% of known AI tools on enrolled machines governed within a quarter of
  rollout, with the remainder *named* in the coverage report — measured, not assumed.
- **The offboarding test:** deactivating a user in the IdP revokes their AI access within one
  SCIM sync, demonstrated end-to-end.
- **The audit test:** an external reviewer, given the audit chain and a policy version, replays a
  quarter's blocked/redacted decisions byte-for-byte without access to any prompt text.
- **The zero-regression covenant, again:** no `[policy]`, no `[gateway.teams]`, unenrolled
  client → byte-identical behavior to today, verified by the existing suite unmodified.
- **The 1000x test:** at least one organization adopts Wayfinder *for governance* — where
  routing savings were not the deciding factor — and says so publicly.

## Sequencing and dependencies

- **WF-ROADMAP-0010 Track B is promoted to prerequisite.** Redis shared state, operator OIDC,
  the audit log, and the Helm chart (0010 R3/R4) must land before Initiatives 2–5 go fleet-wide.
  0010's evidence engine remains the adoption wedge that earns the install this roadmap governs.
- **Initiative 1 and Initiative 6 start immediately** — both pure-core, no dependencies; the
  detector benchmarks de-risk the policy engine's central assumption exactly as judge-validation
  de-risks the evidence report.
- **Initiative 2 needs 0010's Redis + OIDC**; Initiative 3 needs Initiative 2 (an identity to
  enroll with); Initiative 4 needs Initiative 2 plus 0010's export; Initiative 5 needs
  Initiative 1 plus 0010's audit log.
- Release shape (CalVer point releases): **G1** policy engine in log-only + block/redact, with
  detector benchmarks; **G2** the identity plane — SCIM, teams, agent keys, kill-switch;
  **G3** the enrollment agent (macOS) + the org observatory; **G4** the compliance pack,
  Windows/Linux agents, and the rollout playbook.

## Related Decisions

- WF-ADR-0001 (deterministic, offline, no-model-call core — here promoted from routing invariant
  to the product's central compliance claim)
- WF-ADR-0004 (gateway), WF-ADR-0011 / WF-ADR-0014 (metadata-only posture — governs the
  observatory and the audit chain), WF-ADR-0019 (lexicon as configuration — the detector seam)
- WF-ADR-0022 (conversation latch), WF-ADR-0032 / 0034 / 0035 (budgets, rate limits, virtual
  keys — the per-identity gate precedents the policy engine generalizes; 0035's deferred runtime
  key management lands in Initiative 2)
- WF-ADR-0036 (slash directives — per-request steering, now subordinate to policy)
- WF-ADR-0038 / WF-ADR-0042 (service surface + desktop client — the fleet agent's chassis;
  0042's keyless/loopback constraint is repealed for enrollment mode only)
- WF-ADR-0039 (offline-first delivery)
- WF-ADR-0043 (Wayfinder's own model use is local and in-container — the constraint the off-path
  advisory scanner and any future model-backed component inherit)
- Roadmaps: WF-ROADMAP-0008 (its PII gate becomes the first detector set — absorbed with
  credit), WF-ROADMAP-0009 (macOS-only and no-fleet non-goals repealed for enrollment mode),
  WF-ROADMAP-0010 (extended; Track B promoted to prerequisite; its org-hierarchy/SCIM non-goal
  repealed and annotated at source)
- Future designs, written when the work is scheduled: WF-DESIGN-0016 (policy engine & verb
  semantics), WF-DESIGN-0017 (SCIM & the identity plane), WF-DESIGN-0018 (the enrollment agent).
- Future ADRs, named descriptively — IDs assigned at authoring time: the policy-as-code engine;
  content verbs & redaction semantics; SCIM-provisioned identities; agent identities & the
  kill-switch; the enrollment-mode desktop client; the hash-chained audit log; the org hierarchy
  in the ledger.
