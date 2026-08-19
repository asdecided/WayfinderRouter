---
schema_version: 1
id: WF-ROADMAP-0006
type: roadmap
tags: [daily-use, gateway, observability, finops, cost, reliability, integrations, habit, retention]
---

# Roadmap: Daily use — the gateway as a habit

## Status

Planned

## Context

Wayfinder today is excellent at the *moment of decision* — score a prompt offline,
recommend a tier, explain why — and good at *trying it once* (the demo, the TUI, one
preset, `init`/`doctor`). The open question this roadmap answers is **how it becomes
something a person opens or relies on every day**, across all four audiences:
individual developers, platform/ops teams, agent/app builders, and FinOps/cost owners.

Research into sticky developer tools (gh CLI's "stay in the flow", fzf/Atuin bound to a
keystroke used dozens of times a day) points to one structural conclusion: **the habit
surface is the always-on gateway, not the CLI or TUI.** A standalone tool is opened
occasionally; a transparent proxy in front of calls a person *already* makes is exercised
on every request with no new ritual. The highest-leverage work, therefore, makes the
gateway a **trustworthy, observable, savings-proving daily proxy** — which is also,
precisely, the table-stakes feature set that adjacent products (LiteLLM, OpenRouter,
Portkey, Helicone, Langfuse, Cloudflare AI Gateway, Vercel AI Gateway) already ship and
Wayfinder mostly lacks.

Two framing facts shape every item below:

- **Wayfinder's unique wedge is *savings*, not cost.** Every competitor is a cost
  *tracker*; none leads with a deterministic, auditable "you saved X% versus always
  routing to the frontier model." That counterfactual is the daily-reliance hook for cost
  owners and the visible reward (habit loop) for individuals.
- **The hard constraint is non-negotiable (WF-ADR-0001).** The scored decision stays
  deterministic, offline, no model call, no key, no network. The features here live in the
  UX, gateway/proxy, invocation, observability, and workflow layers — or are offline,
  statistical routing-quality improvements. Anything that would put a model call on the
  decision path is explicitly out of scope (see "Core-identity guardrails").

## Outcomes

- The gateway is **safe to depend on daily**: it fails over, retries, caches, and is
  observable per request.
- "It saved me money" is **visible and provable** — a persisted, per-period savings report
  with an auditable counterfactual, not just a session tally.
- Teams can **attribute and cap** spend (and savings) per key/team/tag, turning the
  gateway into a control plane they manage, not just a personal tool.
- Dropping Wayfinder into the tool a developer already uses is a **sub-minute, first-try**
  experience across editors, chat UIs, agent frameworks, and CLIs.
- The routing decision is **trusted** because it tells you when it is unsure and warns when
  your traffic has drifted from what you calibrated on.

## Initiatives

Sequenced by daily-use leverage. Each item lists: persona · direction · the daily-use job ·
how it preserves the no-model-call core · effort · the competitor baseline it closes ·
design reference.

### Wave 1 — Gateway 1.0: a trustworthy, savings-proving daily proxy (the next release)

1. **Integration recipe pack + capability-aware `/v1/models` + liberal params.**
   Dev / Agent · surfaces. *Job:* drop Wayfinder into the tool you already use in under a
   minute and have it work first try. *Core:* pure docs + gateway hygiene, no model call.
   *Effort:* S. *Closes:* the activation gap; a custom `base_url` is a genuine one-line swap
   across Open WebUI, LibreChat, Continue, Cline, Zed, LangChain, LlamaIndex, CrewAI, the
   OpenAI Agents SDK, Vercel AI SDK, and aider — but capability advertising, param
   tolerance, and path handling are the recurring breakage points. **Design: WF-DESIGN-0009.**

2. **Gateway observability — per-request logs (opt-in text) + a cost/savings dashboard.**
   Fin / Ops / Dev · observability. *Job:* see what happened, where spend went, and whether
   routing is working — checkable daily. *Core:* deterministic capture + arithmetic; default
   stays metadata-only (preserving the "never prompt text" posture, WF-ADR-0011/0014), with
   full-text capture strictly opt-in. *Effort:* M. *Closes:* per-request logs/traces +
   cost dashboards that Helicone/Langfuse/Cloudflare/Vercel treat as standard; Wayfinder has
   only aggregate `/metrics` + a read-only routing view. **Design: WF-DESIGN-0008.**

3. **Savings report + auditable counterfactual.**
   Fin / Dev · cost. *Job:* prove ROI — "X% / $Y saved vs always-frontier" — per day/week/
   month, defensibly, to finance. *Core:* tokens × a pinned, versioned price table; no model
   call. *Effort:* M. *Closes:* the wedge no competitor occupies (they track cost, not
   savings). **Design: WF-DESIGN-0007.**

4. **Reliability — fallback, retry, circuit breaker.**
   Ops / Agent · reliability. *Job:* requests don't fail when a provider 5xxs, rate-limits,
   or times out. *Core:* ordered fallbacks, bounded retries, per-target cooldown, and a
   success/failure circuit breaker — all on the invocation layer; the *scored decision* is
   never re-computed, only delivery is retried. *Effort:* M. *Closes:* fallback/retry/
   failover that LiteLLM/OpenRouter/Cloudflare/Vercel all ship; prerequisite for production
   daily use. **Design: WF-DESIGN-0010.**

### Wave 2 — Team control plane

5. **Virtual sub-keys + per-key/team/tag attribution.**
   Ops / Fin · workflow + observability. *Job:* issue scoped keys to teams/apps and attribute
   spend *and savings* to each. *Core:* deterministic key validation + request tagging; no
   model call. *Effort:* M–L. *Closes:* virtual keys with attribution (LiteLLM, Portkey,
   Vercel, Cloudflare) — Wayfinder resolves *provider* keys but issues none of its own.
   **Shipped (hashed-in-config keys: auth + per-key attribution + per-key budgets/rate-limits +
   per-key model allowlists): WF-ADR-0035.** Runtime create/revoke (admin API, #12) is the
   remaining deferred follow-up.

6. **Budgets & spend caps with auto-reset — degrade to local on breach.**
   Fin / Ops · cost. *Job:* cap spend per key/window without surprises. *Core:* deterministic
   counter; on breach **force the cheap/local tier** instead of hard-blocking — a graceful
   degradation no competitor offers, and uniquely natural for a router. *Effort:* M.
   *Closes:* budgets (LiteLLM, Portkey, OpenRouter) — with a Wayfinder-native twist.
   **Shipped (gateway-wide cap, day/month/all windows, degrade-or-block): WF-ADR-0032.**
   Per-*key* budgets ride on #5 (virtual keys); auto-reset is the rolling window.

7. **Configurable rate limiting (RPM/TPM, per key/session).**
   Ops · workflow. *Job:* protect upstreams and contain blast radius. *Core:* deterministic
   sliding/fixed-window counters. *Effort:* S–M. *Closes:* rate limiting (LiteLLM,
   Cloudflare, Helicone, Portkey). **Shipped (gateway-wide RPM/TPM, fixed-window, 429):
   WF-ADR-0034.** Per-*key/session* limits ride on #5 (virtual keys); a sliding window is a
   deferred refinement.

### Wave 3 — Deeper routing trust (mostly already specced)

8. **Decision confidence + abstention band.**
   Dev / Agent · smarter routing. *Job:* know how much to trust each route; escalate to the
   safe tier in a low-confidence band. *Core:* margin = |score − threshold| with a
   reject-option threshold (Chow 1970); no model call. *Effort:* M. **Design: WF-DESIGN-0003
   (already drafted).**

9. **Mature calibration + offline drift detection.**
   Ops / Fin · smarter routing. *Job:* tune thresholds to your traffic, and get warned when
   traffic drifts away from the calibration set. *Core:* threshold sweep on a labeled local
   corpus, plus KS / Chi-Square / PSI / Wasserstein on prompt-feature histograms and the
   router's own tier-mix — statistical, label-free, no inference. *Effort:* M–L. **Designs:
   WF-DESIGN-0004 (calibration), WF-DESIGN-0005 (feature audit); drift is the additive piece.**

10. **Exact-match response cache (gateway).**
    All · cost + reliability. *Job:* instant, free repeats. *Core:* SHA-256 of the normalized
    request → stored response; fully deterministic and offline. *Effort:* M. *Closes:* exact
    caching (Cloudflare, Portkey "simple", Helicone exact). **Note:** *semantic* caching needs
    an embedding model call and is therefore out of scope — exact-match is the compatible
    subset. **Shipped (deterministic, in-memory, opt-in): WF-ADR-0033.** Per-model TTL, an
    on-disk tier, and streaming caching are deferred follow-ups.

### Wave 4 — Reach & longer bets

11. **Claude Code adapter — an Anthropic `/v1/messages` translation endpoint.**
    Dev / Agent · surfaces. *Job:* make Wayfinder a one-line `ANTHROPIC_BASE_URL` swap for
    Claude Code (and other Anthropic-Messages-native clients), which a pure OpenAI gateway
    cannot serve directly. *Core:* format translation only (Messages ⇄ Chat Completions);
    deterministic, no model call. *Effort:* M. *Closes:* the single highest-value surface that
    is *not* a base_url swap today. **Shipped (buffered + streaming + tool use): WF-DESIGN-0011.**
    Image/vision blocks, extended thinking, and prompt-caching controls are the documented follow-up.

12. **Writable admin / config UI.**
    Ops · workflow. *Job:* create keys, teams, budgets, and routing config at runtime. *Core:*
    deterministic config management. *Effort:* L. *Closes:* admin UIs (LiteLLM, Portkey,
    Vercel, Cloudflare) vs Wayfinder's read-only dashboard. *Longer bet.*
    **Foundation shipped:** WF-ADR-0071 defines the versioned profile, binding,
    activation, rollback, receipt, audit, and last-known-good contracts. Issue
    #160 remains the interface over those contracts.

13. **Showback / chargeback export + a metrics API.**
    Fin · observability. *Job:* feed Wayfinder's cost & savings into a central FinOps tool.
    *Core:* deterministic per-period export (CSV/JSON). *Effort:* M. *Closes:* the aggregation
    endpoints LiteLLM/Langfuse expose "for billing / analytics". *Builds on WF-DESIGN-0007/0008.*

## Core-identity guardrails (what we will NOT do on the scored path)

These break WF-ADR-0001 and are out of scope for the default decision path. They may only
ever appear as explicitly-labelled, off-by-default opt-ins:

- **Semantic / embedding caching** — needs an embedding model call (Portkey, Helicone). Ship
  *exact-match* caching only.
- **Learned / semantic routing** — RouteLLM's `mf`/`sw_ranking` require an `OPENAI_API_KEY`;
  Not Diamond and Martian predict per-query with a model. The ~500–1500 ms + hundreds-of-tokens
  "router tax" they incur is the thing Wayfinder *avoids* — to be marketed, not adopted.
- **LLM-as-judge evaluation** — a model call; keep out of the core, opt-in tooling at most.

## Success Measures

- **Activation:** a new user points an existing tool at Wayfinder and sees a real routed
  reply + a savings figure within the first session (leading indicator of daily retention).
- **Savings is provable:** a cost owner can produce a per-period savings number that
  reconciles with provider invoices within a small margin, with a pinned price table.
- **Reliability:** a chaos test (kill the chosen upstream mid-request) yields graceful
  fallback with no dropped request.
- **Stickiness:** instrument workday DAU/MAU for gateway deployments; target the B2B-infra
  band (well above the generic-SaaS floor), tracked via activation and 1–7 day retention,
  not raw installs.

## Assumptions

- Most daily value accrues through the gateway; the CLI/TUI are entry points and operator
  surfaces, not the primary habit.
- The deterministic, offline, no-model-call core remains the identity; every item preserves
  it or is explicitly opt-in.
- Deployers want self-hosted control and BYO-key, not a hosted control plane.

## Risks

- **Privacy regression:** per-request logging could capture prompt text. Mitigation:
  metadata-only by default; full-text strictly opt-in with retention controls (WF-DESIGN-0008).
- **Scope creep:** the team control plane (Wave 2) can sprawl; ship Wave 1 first and treat
  keys/budgets/limits as separate, composable PRs.
- **Counterfactual credibility:** a savings number finance can't trust is worse than none;
  pin and timestamp the price table and label estimated vs actual tokens (WF-DESIGN-0007).
- **Failover policy is cost/security-relevant:** silently escalating to a pricier tier on
  failure could raise spend; the policy must be explicit and conservative, and may warrant
  its own ADR.

## Related Decisions / Designs

- WF-ADR-0001 (deterministic, offline, no-model-call core — the invariant every item preserves)
- WF-ADR-0004 (OpenAI-compatible gateway / invocation layer — where these features live)
- WF-ADR-0011 / WF-ADR-0014 (decision metadata only, never prompt text; the read-only dashboard)
- WF-ADR-0017 (cost metadata — `cost_per_1k`, the price table the savings report uses)
- WF-ADR-0018 (Prometheus `/metrics` — extended with cost/savings gauges)
- WF-ADR-0025 / WF-DESIGN-0006 (key handling; the secret-store path)
- WF-DESIGN-0007 (savings report), WF-DESIGN-0008 (observability/cost dashboard),
  WF-DESIGN-0009 (integration recipes & compatibility), WF-DESIGN-0010 (reliability)
- WF-DESIGN-0003 (confidence/abstention), WF-DESIGN-0004 (calibration), WF-DESIGN-0005
  (feature audit) — the Wave-3 routing-trust work
- WF-ROADMAP-0005 (post-launch routing quality — confidence/calibration/demo/hardening; this
  roadmap is the daily-use complement focused on the gateway and cost surfaces)
