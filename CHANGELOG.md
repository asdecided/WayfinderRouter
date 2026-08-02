# Changelog

User-visible changes to Wayfinder, by release. Follows the spirit of
[Keep a Changelog](https://keepachangelog.com/): user impact over implementation
details, release history over commit history.

## Desktop v0.1.0 — Unreleased

### Added

- **Opt-in ChatGPT account routing for Desktop Chat** (WF-DESIGN-0018,
  WF-ROADMAP-0012, WF-ROADMAP-0015). The Rust gateway can now run an isolated, bounded Codex
  app-server provider and deliver text-only Chat through models available to an eligible signed-in
  ChatGPT account. Native Settings keeps this under Accounts, separate from OpenAI Platform keys,
  and receives only normalized account/model state—never tokens. The provider must be explicitly
  added as `codex-app-server`; the native Connections screen can add that route with one click before
  sign-in. Signing in does not change Automatic routing, make it a preferred
  hosted destination, or expand the credential broker. Offline mode excludes it, and a pinned ChatGPT
  destination fails visibly instead of falling back. Development builds can use an explicitly
  selected helper; release builds reject unverified sibling executables, and the ChatGPT-app
  fallback must pass runtime and signing validation. Concurrent turns report a bounded Busy state
  without poisoning provider health. Desktop v0.1.0 requires a separately installed, compatible,
  correctly signed `/Applications/ChatGPT.app` and does not bundle or redistribute the Codex
  executable; a self-contained provider is explicitly deferred.

- **Wayfinder Desktop v0.1.0 Chat** (WF-ADR-0042, WF-ROADMAP-0012, WF-ROADMAP-0015). The native
  macOS app now includes a dedicated, thread-first Chat window that streams replies through the
  bundled arm64 Rust gateway. This first desktop release supports Apple Silicon on macOS 14 or
  later; Intel and a universal artifact are deferred. The complete conversation stays central while
  the authoritative provider, mode, score, explanation, and routing signals remain available from
  an information popover on each response instead of consuming permanent thread width.
  Navigator filters never remove messages from the transcript. Conversation history persists
  locally across New Chat and relaunch; Stop, contextual Retry, New Chat, bounded request history,
  personal destination names, and actionable delivery failures are included. Settings consolidates
  ChatGPT accounts and API-key providers under Connections while keeping their authentication paths
  visibly distinct. The app uses its own SemVer release line (`0.1.0`), and its embedded router
  reports the desktop product version.

- **Apple Foundation Models availability contract** (WF-DESIGN-0017). The native macOS package now
  capability-detects `SystemLanguageModel.default` on macOS 26 and distinguishes available,
  device-ineligible, Apple Intelligence disabled, model-not-ready, unsupported, and sanitized
  unavailable states through a bounded versioned protocol. On an eligible, never-configured Mac,
  Setup preselects Apple Local only after a live `available` response and still requires user
  confirmation. Existing configuration and route ladders remain unchanged, and Chat opens on
  `Automatic`.

- **Apple Foundation Models delivery boundary** (WF-ROADMAP-0014). Eligible `apple-local` requests
  use a separate authenticated, bounded inference XPC service inside the native bundle. Delivery is
  capability-gated and does not broaden the credential broker or make Apple a global Automatic route.

### Changed

- **Fleet-wide accounting, admission, and provider health** (WF-ADR-0060, issue #147).
  Redis-backed deployments now coordinate idempotent realized-cost and token accounting,
  conservative global/workspace/virtual-key budget reservations, in-flight delivery leases,
  and provider circuit state across replicas. Requests keep their reservation through streaming
  or buffered completion, retries and cancellation settle exactly once, and Redis outages fall
  back to the bounded local primitives while exposing a degraded-state signal. The memory backend
  remains the zero-configuration compatibility mode; prompt and credential content never enters
  shared state.

- **Bounded gateway delivery concurrency** (WF-ADR-0051). The Rust gateway now admits up to 32
  simultaneous provider deliveries by default, allows a bounded queue of 64 requests for up to two
  seconds, and returns a retryable `503` when saturated. Streaming requests retain their slot until
  the response body completes or is dropped. Operators can tune all three limits and inspect
  in-flight, peak, queue-wait, and rejection metrics. An authenticated 20-client transport test
  protects the expected shared-gateway concurrency behavior; provider capacity remains a separate
  deployment constraint.

- **Rust-only runtime cutover** (WF-ADR-0046, WF-ROADMAP-0014). Wayfinder now builds, tests,
  containers, and ships one native Rust router and gateway. The helper no longer delegates commands
  to Python, and the old package, PyPI release, Python container, and coexistence-only command
  surfaces are removed. Native Desktop Chat and setup remain the supported user experience. This
  does not change Automatic routing preferences, broaden the credential broker, or make Apple a
  global default.

- **Python gateway retirement closeout** (WF-ADR-0046). Current release guidance, issue intake,
  and Rust contract tests no longer present the removed Python implementation as a runnable
  fallback or live oracle. Checked-in pre-cutover vectors remain immutable migration evidence only;
  they execute entirely inside the Rust workspace.

## v2026.7.0 — 2026-07-01

The **run-it-anywhere** release. Install the gateway as an always-on **local service**, route
**offline-first** when there's no network, and lean on a broad **production-audit hardening pass** that
closes reliability, correctness, and input-validation gaps across the request path and the surface tools
(with a stronger test suite behind them). All additive; the scored decision stays deterministic and
offline (WF-ADR-0001).

### Added

- **Run the gateway as a local service** (WF-ADR-0038, WF-ROADMAP-0007). `wayfinder-router service
  install` registers Wayfinder with the OS service manager so it auto-starts at login on a stable
  `127.0.0.1:8088` and restarts if it exits — turning it into your machine's always-on local LLM
  endpoint that every OpenAI-compatible app can share (point them at it once with
  `OPENAI_BASE_URL`). **macOS** (launchd LaunchAgent) is the primary target; **Linux** (systemd user
  unit) ships alongside; `uninstall` and `status` round it out, and `--print` emits the unit without
  installing. If no service manager is present it writes the unit and prints the one command to start
  it. Packaging only — the deterministic decision is unchanged (WF-ADR-0001); your keys stay in the
  gateway config. The near-term, localhost slice of the "LLM routing as infrastructure" idea.

- **Offline-first delivery** (WF-ADR-0039, WF-ROADMAP-0007). Set `[gateway] offline = true` (or send
  `X-Wayfinder-Offline: true` for one request) and Wayfinder serves the **cheapest/local tier and
  never calls a dearer/cloud tier** — so a request can't hang on a timeout when there's no network
  (the "works on a plane" case), and it doubles as a privacy / air-gapped mode. It reuses the existing
  `degrade` failover + circuit breaker; the scored decision is unchanged and still reported, with a new
  `x-wayfinder-router-offline: true` header marking the degrade. Explicit signal only (reactive
  cloud-down is already handled by the breaker); no model call enters the decision path (WF-ADR-0001).
  Offline takes precedence over the response cache (it never replays a dearer tier's cached answer —
  the cache keys on the served, cheapest tier) and over a hard budget block (a request that incurs no
  cloud spend is delivered from local rather than rejected with a 402). The standing mode is reported at
  `/healthz`, in `wayfinder-router service status`, and in the `?debug` decision payload, so you can
  confirm air-gapped mode is on without inspecting a request's headers.

### Fixed

- **A bad/expired upstream key now trips the circuit breaker** (WF-ADR-0031). A non-retryable auth
  failure (401/403/407) from a provider was being recorded as a breaker *success*, so a stale key —
  the most common real-world failure — never opened the breaker and failover/degrade never engaged.
  Such statuses now count as a target failure: the response is still returned (you see the auth error),
  and after repeats the breaker opens and delivery degrades to a healthy tier. Ordinary client 4xx stay
  breaker-neutral.
- **Rate-limit admission now precedes virtual-key auth** (WF-ADR-0034/0035). The gateway-wide RPM/TPM
  cap is the documented "outermost guardrail," but auth ran first — so a flood of requests with no/invalid
  token was 401'd one at a time and never rate-limited. The gateway-wide limiter now runs before auth (a
  token flood is shed with a 429); the per-key limiter still runs after auth, where the key id is known.
- **Budget enforcement reads the current config's pricing, not a stale flag.** The spend cap consulted a
  `priced` flag written at the *end* of the previous request, so for one request after a hot reload that
  toggled `cost_per_1k` it enforced (or skipped) the budget on stale state. It now derives priced-ness
  from the live config at decision time.
- **Parallel tool calls survive streaming through the Claude Code adapter** (WF-DESIGN-0011). When an
  upstream interleaved two streaming tool calls (index 0 opens, index 1 opens, index 0 continues), the
  Anthropic translator closed the wrong content block and emitted a third, synthetic `tool_use` block
  with an empty name and a placeholder id — silently dropping one call's real id and name. Tool calls
  are now buffered per index and emitted as complete, non-interleaved blocks, so each keeps its real
  id, name, and full arguments. Text streaming and non-streaming responses are unaffected.
- **The Configure UI saves back to the config it loaded.** Edits made through the web Configure tab
  were written to `wayfinder-router.toml` in the current directory, even when the config actually in
  use lived in a parent directory (the loaders walk up). Running the UI from a subdirectory could thus
  create a second, ignored file and silently lose the edit. Save now targets the same file the read
  resolved, only creating a new one when none exists up the tree.
- **An unknown onboarding arm returns a clean 400** instead of an opaque 500. The browser onboarding A/B
  endpoint crashed on a model name it didn't recognize (a stale client, or a hand-rolled request).
- **Thread ids resist collisions.** A saved conversation's id used a 1-second timestamp plus only two
  random bytes, so two chats created in the same second could collide and one silently overwrite the
  other's transcript. The random suffix is now eight bytes; ids still sort by creation time.
- **`service install` reports failure when it fails.** It printed "installed and loaded" / "installed and
  started" and exited 0 even when `launchctl`/`systemctl` errored. It now verifies the agent is actually
  loaded (launchd) / the unit enabled (systemd), surfaces the manager's error, and exits non-zero on
  failure — while still treating an already-loaded agent as success.
- **A malformed or older-schema savings ledger no longer crashes the next stats query.** A persisted
  bucket missing a field added in a later version (e.g. `estimated_n`), or any partial corruption, could
  raise a `KeyError` from `/v1/savings` — contradicting the ledger's "best-effort persistence, never raise
  into the request path." Loaded buckets are now normalized to the current schema on read.
- **`calibrate --l2 0` (or a negative value) gives a clean error.** A non-positive L2 term can make the
  classifier's Hessian singular and previously surfaced as a raw `ZeroDivisionError`; it is now a
  `CalibrationError` explaining that `--l2` must be positive (the default `0.01` is unaffected).
- **Non-UTF-8 input files give a usage error, not a traceback.** `route`, `calibrate`, `onboard`, and
  `judge` reading a binary/non-UTF-8 file now print a clear message and exit with the usage code (2),
  matching how a missing file is already handled.
- **`keys new` emits valid TOML for any `--id` / `--tag`.** A key id or tag containing a dot, quote,
  bracket, or space produced a malformed (or structure-injecting) paste-able config block; ids and tags
  are now TOML-escaped, so the snippet always round-trips to exactly what you passed.
- **The launchd unit generator resolves `~` on its own.** `service.launchd_plist` left a literal `~` in
  the log paths when called without an explicit `log_dir` (launchd doesn't expand it, so the agent failed
  to spawn); the default is now resolved to an absolute path for every caller, not just the CLI's.
- **`cross_validated_accuracy` rejects `k < 2`** with a clear error instead of silently returning `0.0`
  (which read as a genuine "no lift" verdict). Cross-validation needs at least two folds.
- **The terminal chat's `/scope` and `/sticky` commands now actually affect routing.** They set and
  displayed state but neither backend consulted it, so `/scope all` and `/sticky on 5` changed only the
  status bar. The in-process backend now applies the route-on scope (WF-ADR-0021) and the sticky
  conversation latch (WF-ADR-0022) by reusing the gateway's own `extract_prompt` /
  `conversation_high_water`, and the `--base-url` backend sends the matching `X-Wayfinder-Route-On` /
  `X-Wayfinder-Sticky` / `X-Wayfinder-Sticky-Cooldown` headers — so what the status bar shows is what
  routes, identically across both backends. Still pure and offline (WF-ADR-0001).

## v2026.6.10 — 2026-06-29

The **feedback release** — features driven by post-launch feedback.

### Added

- **Automated sufficiency judge for calibration** (WF-ADR-0037). `wayfinder-router judge
  prompts.jsonl --gold gold.jsonl` closes the calibration loop without a human grading every
  prompt: it runs each prompt through two tiers, asks an automated judge *"was the cheaper tier
  good enough to skip the dearer one?"*, and records the answer as a label that `calibrate`
  already consumes. The built-in `HeuristicJudge` is a pure, deterministic text comparator
  (refusal/error detection, agreement, similarity) that **abstains** rather than guess when it
  can't tell. Because a bad label silently degrades *live* routing, a config is **untrusted until
  it clears mandatory gates** — judge-vs-gold Cohen's κ (≥ 0.6), out-of-fold cross-validated lift
  over the majority baseline, and a degenerate-collapse check; on failure the command prints the
  confusion matrix and refuses to emit a config (the labels are still recorded). Emitted configs
  carry a provenance banner (judge version, dataset/gold hashes, the gates that passed). All
  judging is offline / calibration-time in the invocation layer — the deterministic decision path
  makes no model call (WF-ADR-0001). Saving raw prompts+responses (`--save-comparisons`) is off by
  default (a governed response-body store, WF-DESIGN-0008). An LLM-backed judge is a planned
  drop-in through the same `Judge` seam.

### Changed

- **Docs & positioning pass** (from post-launch feedback). Plainer, less marketing-flavored copy
  across the README, explainer, and demo. The "How it compares" table now lists **Bifrost**
  alongside LiteLLM and spells out the distinction: OpenRouter / Bifrost / LiteLLM are
  multi-provider gateways (they pick *which provider* serves a call), while Wayfinder routes by
  prompt *difficulty* (cheap vs expensive) and decides offline — and the two compose. Forcing a
  tier for one request (`local` / `cloud`, `prefer-local` / `prefer-hosted`, the
  `X-Wayfinder-Threshold` header, or a `/local` / `/cloud` chat directive) is now surfaced in the
  Quickstart. No behavior change.
- **README reframe & de-slop, expanded FAQ** (HN feedback, second pass). The README now leads with the
  one fast, offline *hard-or-easy* decision (composing a model-router behind it is the optional part),
  states "offline / no model call / deterministic" once instead of repeatedly, and moves the gateway
  operator settings to a dedicated [`docs/gateway-config.md`](docs/gateway-config.md). The FAQ gained
  honest answers to a second round of Show HN questions — context across model switches, multi-turn
  cost, routing inside agentic harnesses, behaviorally-different models, phased escalation, multi-model
  compare (it's the offline sufficiency judge above), using the routing decision without the proxy, and
  mid-task context loss. No behavior change.
- **Documented the release process** ([`RELEASE.md`](RELEASE.md)). The CalVer `YYYY.MM.MICRO` scheme,
  the single source of version truth (`wayfinder_router.__version__`), the cut checklist, and the
  tag-triggered PyPI publish are now written down.

## v2026.6.9 — 2026-06-25

### Added

- **In-message slash routing directives** (WF-ADR-0036). With `[gateway] slash_directives = true`
  (off by default), a recognized `/directive` at the start of the latest user message forces the
  route for that turn — `/local refactor this` pins to `local`, `/prefer-hosted …` to the top tier,
  `/auto …` back to scoring. It lets anyone steer routing from a plain chat box (or Claude Code,
  via `/v1/messages`) with no `model` field or header control. The directive is **stripped** before
  the prompt is scored or forwarded, so the model never sees it. Only a *known* directive (a
  configured model name, `prefer-*`, or `auto`) is acted on — a path like `/etc/...`, a `/help`, or
  any other slash-prefixed text is left untouched. An explicit `model`-field pin still wins; a
  slash-routed turn reports `mode: slash-pinned` and is still clamped by a virtual key's model
  allowlist. Deterministic, no model call (WF-ADR-0001).

## v2026.6.8 — 2026-06-25

A small hardening on top of v2026.6.7 (and the first build of all of v2026.6.7's gateway
work to reach PyPI).

### Added

- **Informational `X-RateLimit-*` response headers** (WF-ADR-0034). When a rate limit is
  configured, every response now carries `X-RateLimit-Limit`, `X-RateLimit-Remaining`, and
  `X-RateLimit-Reset` (seconds until the window rolls), so a well-behaved client can see its
  headroom and self-pace *before* hitting a `429`. The headers reflect the tightest applicable
  request cap — gateway-wide or the request's virtual key — and are absent when no limit is set.
  Purely additive; no change to routing or enforcement.

## v2026.6.7 — 2026-06-25

The gateway becomes a control plane. It now issues **virtual API keys** — authenticate callers,
attribute spend *and savings* per team, and scope each key's budget, rate limit, and allowed
models — and completes the **guardrails trilogy** by adding RPM/TPM **rate limiting** alongside
budgets and an exact-match **response cache** for instant, free repeats. All opt-in and additive;
the scored decision stays deterministic and offline (WF-ADR-0001).

### Added

- **Virtual API keys — auth, attribution, and per-key budgets/limits** (WF-ADR-0035,
  WF-ROADMAP-0006). Issue scoped gateway credentials per team/app: `[gateway.keys.<id>]` stores a
  **SHA-256 hash** of the key (never the plaintext), minted with `wayfinder-router keys new`. When
  any key is configured, `/v1/*` (including Claude Code's `/v1/messages`) requires a valid
  `Authorization: Bearer` token — `401` otherwise; with no keys configured the gateway stays open
  (backward compatible). Each request is attributed to its key: in the `/router/recent` feed, in
  `wayfinder_router_key_requests_total{key=…}`, and — the FinOps payoff no competitor offers — as
  per-key realized/**savings** under `by_key` in `/v1/savings`. A key can carry its own
  `[gateway.keys.<id>.budget]`, `[gateway.keys.<id>.rate_limit]`, and a `models` allowlist; the
  key's cap and the gateway-wide cap both apply, strictest wins. A **`models` allowlist** restricts
  which configured models a key may use — if routing picks one the key isn't allowed, the request
  **clamps to the nearest allowed tier** (preferring not to raise cost, reported as
  `mode: key-scoped`) rather than failing. Hashed-in-config, hot-reloaded, deterministic, no model
  call (WF-ADR-0001); provider keys still come from your environment (WF-ADR-0004). Runtime
  create/revoke is a deferred follow-up.

- **Gateway rate limiting** (WF-ADR-0034, WF-ROADMAP-0006). An optional `[gateway.rate_limit]`
  that caps requests-per-minute (`rpm`) and/or upstream-tokens-per-minute (`tpm`) over a fixed
  `window` (default 60s); on breach the gateway returns **HTTP 429** with a
  `Retry-After` header, an `x-wayfinder-router-rate-limit: rpm|tpm` header, and a
  `wayfinder_router_rate_limited` error. It's the outermost guardrail — checked before scoring —
  so a runaway client or retry storm can't flood an upstream. A cache hit still counts as a
  request (RPM) but spends no upstream tokens (TPM); `/metrics` gains
  `wayfinder_router_rate_limited_total`. Gateway-wide in v1 (per-key limits arrive with virtual
  keys); pure deterministic counters, no model call (WF-ADR-0001). Completes the guardrails
  trilogy with budgets (cost) and the cache (repeats).
- **Exact-match response cache** (WF-ADR-0033, WF-ROADMAP-0006). An optional `[gateway.cache]`
  that replays a stored answer for an identical, deterministic request — instant, free repeats
  for eval/CI, dev loops, and agentic tools (it covers `/v1/messages`/Claude Code too). The key
  is a SHA-256 of the normalized request (the prompt is hashed, never stored), keyed on the
  served upstream model; the cache is **in-memory only, off by default**, and bounded by
  `ttl` (default 300s), `max_entries` (1024), and `max_bytes` (64 MiB) — raise `max_bytes` to
  give it more memory. A hit is **free**: it records no realized cost and doesn't consume a
  budget, and the cost it avoided is reported separately (`wayfinder_router_cache_avoided_cost_total`,
  plus `…_cache_hits_total` / `…_cache_misses_total`); every response carries
  `x-wayfinder-router-cache: hit|miss`. Only deterministic requests are cached (no streaming,
  `temperature` > 0, `tools`, `seed`, or `n` > 1), and an HTTP-200 error-shaped body is never
  stored. The scored decision is never recomputed (WF-ADR-0001). As the project's first
  response-body store it follows WF-DESIGN-0008's opt-in framing — disabling it purges the
  in-memory bodies immediately, and cached content is never logged or surfaced.

## v2026.6.6 — 2026-06-24

The gateway reaches further and spends safer. Point **Claude Code** at it with one
environment variable — a new Anthropic `/v1/messages` adapter translates Messages ⇄ Chat
Completions (streaming and tool use included) — and cap spend with **budgets** that degrade
to the cheapest tier on breach rather than failing. Both are pure additions around the same
deterministic router: scoring, failover, and the savings ledger are reused unchanged, and
the scored decision stays offline (WF-ADR-0001).

### Added

- **Claude Code adapter — an Anthropic `/v1/messages` endpoint** (WF-DESIGN-0011, WF-ROADMAP-0006).
  Point `ANTHROPIC_BASE_URL` at the gateway and Claude Code (or any Anthropic-Messages-native
  client) routes through Wayfinder. The endpoint translates the Anthropic Messages format to the
  OpenAI Chat Completions the gateway already speaks and back again — both directions, buffered
  **and streaming**, including **tool use** (`tools`/`tool_choice`, `tool_use`/`tool_result`, and
  streamed tool calls). It is pure format translation: scoring, budgets, and failover are the
  *existing* router, reused unchanged, so the same `x-wayfinder-router-*` decision headers ride
  along and there is exactly one routing decision (WF-ADR-0001). Image/vision blocks, extended
  thinking, and prompt-caching controls are not translated yet. See the Claude Code recipe in
  [docs/integrations.md](docs/integrations.md).
- **Gateway spend budgets** (WF-ADR-0032, WF-ROADMAP-0006). An optional `[gateway.budget]`
  spend cap that, once the period's realized cost is reached, **degrades to the cheapest tier**
  rather than hard-failing — the same `degrade` primitive failover uses (WF-ADR-0031), so it
  keeps you working at lower cost. `limit` is the ceiling, `window` is `day` (default) | `month`
  | `all`, and `on_breach` is `degrade` (default) or `block` (refuse with HTTP 402,
  `wayfinder_router_budget_exhausted`). A breach is never silent: the response carries
  `x-wayfinder-router-budget: degraded` (or `blocked`) and a degrade reports
  `mode: budget-degraded` with the cheaper tier in `x-wayfinder-router-model` — while the
  `score` header still shows the true, unchanged complexity score (the cap reshapes routing,
  never the decision; WF-ADR-0001). Enforced only when real `cost_per_1k` prices are configured
  (`priced`); a relative-unit demo has no dollars to cap, so the budget is a no-op there.

## v2026.6.5 — 2026-06-23

The gateway grows up. It now **proves the savings** routing makes (a persisted,
per-period report vs always-frontier), is a **drop-in** for the tools you already use,
and is **production-reliable** — retries, same-tier fallback, a circuit breaker, and an
opt-in cross-tier failover policy. The terminal chat's `/cost` gains a cross-session
period view. All additive; routing stays deterministic and offline (WF-ADR-0001).

### Added

- **Savings report on the gateway** (WF-DESIGN-0007). `GET /v1/savings?period=today|7d|30d|all`
  returns realized vs always-frontier cost and the savings between them, with a per-route
  breakdown — computed deterministically from token counts (the upstream `usage` when present,
  else a labelled estimate) times your `cost_per_1k` price table; no model call. Figures are
  dollars when costs are configured (`priced: true`), else relative units. The report is
  persisted (best-effort, survives restarts) and pins a `price_table_version` so a number is
  auditable. `/metrics` gains `wayfinder_router_realized_cost_total`,
  `…_baseline_cost_total`, and `…_savings_cost_total`, and the `/router` decision feed now
  carries per-request cost metadata (dollars + token counts only — never prompt text).
- **Gateway path tolerance** (WF-DESIGN-0009). `/chat/completions` and `/models` now also
  answer without the `/v1` prefix, so a client whose `base_url` omits `/v1` still routes. New
  **[Integration recipes](docs/integrations.md)** cover chat UIs, editors, agent frameworks,
  and CLIs.
- **Gateway reliability — retries, same-tier fallback, circuit breaker** (WF-ADR-0031,
  WF-DESIGN-0010). A failed forward (transport error, or `429`/`5xx`) is retried with bounded
  backoff; on exhaustion it falls back to a model's configured `fallbacks` (same-tier
  alternate endpoints); a per-target circuit breaker skips a downed upstream until it cools
  down (then a `503` rather than hammering it). Ordinary `4xx` fails fast. Tunable via
  `[gateway] retries / breaker_threshold / breaker_cooldown` and per-model `fallbacks`.
  Responses carry `x-wayfinder-router-served-by` (and `x-wayfinder-router-failover` when it
  differs from the routed tier); cost is billed to the target that actually served. An
  opt-in **cross-tier `failover` policy** (`[gateway] failover = same-tier` (default) `|
  degrade | escalate`, per-request override via `X-Wayfinder-Failover`) can fall to a cheaper
  tier (`degrade`, never raises cost) or a dearer one (`escalate`, opt-in) once same-tier
  options are exhausted, and a **deterministic pre-call check** skips a target whose
  `context_window` can't fit the prompt before spending the call. The **scored decision is
  never recomputed** — this is delivery, not routing.
- **`/cost` period view in the terminal chat** (WF-DESIGN-0007). The chat now records turns
  into a persisted savings ledger, so `/cost` shows today / 7-day / 30-day / all-time savings
  (and route mix) that accrue across sessions, not just the current one.

## v2026.6.4 — 2026-06-23

More providers, safer keys. A one-command Google Gemini preset joins the `init`
lineup, and model keys can now be filled from your secret store at startup instead
of exporting raw secrets — keys are still read at request time and never written to
disk.

### Added

- **Model keys can come from your secret store.** A `[gateway.models]` entry may name an
  `api_key_cmd` (e.g. `op read op://Private/Anthropic/credential`) that fills its key
  **in memory** at startup when the environment variable is unset — so the secret can
  live in your password manager and never touch a shell file, config, or disk. An
  already-set variable always wins, so the command runs only when needed
  (WF-ADR-0004, WF-DESIGN-0006). `init` and `doctor` suggest a ready-to-edit command for
  whichever tools they find on your `PATH`: 1Password, macOS Keychain, Secret Service,
  `pass`, gopass, HashiCorp Vault, AWS Secrets Manager, Bitwarden, Doppler, and Google
  Secret Manager.
- **`/keys` in the terminal chat** re-resolves keys from your secret store and reports
  each model's status with fix-it hints, and the `/models` panel now notes
  command-resolved keys. A first-run nudge points you at `/keys` when a key is missing.
- **`init --preset gemini`** scaffolds a two-tier Google Gemini config
  (`gemini-2.5-flash` → `gemini-2.5-pro`) through Gemini's OpenAI-compatible endpoint,
  and the default `hybrid` preset gains a commented Gemini swap example. Gemini needs no
  special handling — it speaks the same OpenAI `/chat/completions` the gateway already
  forwards to (WF-ADR-0004).

## v2026.6.3 — 2026-06-22

Wayfinder moves to **calendar versioning** (`YYYY.M.MICRO`); this is the release that
the roadmap tracked as v0.3.0. It makes the terminal a first-class surface and adds
one-command setup.

### Added

- **`wayfinder-router chat` is a full-screen terminal app** (Textual): a scrolling
  transcript headed by the wordmark, inline `● LOCAL` / `◆ CLOUD` decisions with an
  expandable `/why` breakdown, a `/settings` panel, and streamed model replies. Two
  backends — in-process via `[gateway.models]`, or `--base-url` against a running
  gateway. Needs the `[tui]` extra (now rich **and** textual).
- **`wayfinder-router init`** scaffolds a starter `wayfinder-router.toml` (plus a
  `.env.example` of variable *names* only) from a preset (`hybrid` = keyless local
  Ollama → Anthropic cloud, or `openai` = gpt-4o-mini → gpt-4o) or interactively
  (`--interactive`), then reports which model keys resolve.
- **`wayfinder-router doctor`** checks the nearest config and whether each model's key
  is set (`✓ set` / `✗ not set` / `keyless`) — no server required.
- First-run nudges: `chat` and `webchat` point at `init` when no models are configured.

### Changed

- **Versioning is now CalVer (`YYYY.M.MICRO`).** The previous release was `v0.2.0`.
- Keys remain environment-only — `init`/`doctor` only ever name the variables to export;
  no secret is written, logged, or captured (WF-ADR-0004).

## v0.2.0 — 2026-06-19

This release adds cost-aware calibration and an opt-in lexical signal to the scorer.
Default routing is unchanged from v0.1.x — the new lexical features ship off.

### Added

- **Lexical difficulty signals in the scorer, opt-in / off by default**
  (WF-ADR-0016). The scorer computes and reports four new deterministic, offline
  features alongside the structural ones — `reasoning_term_count` (a curated lexicon
  of hard-task verbs and concepts — prove, derive, optimize, theorem, invariant,
  concurrency, …), `math_symbol_count` (math/logic glyphs and LaTeX-ish tokens),
  `constraint_term_count` (multi-constraint markers), and `question_count` — but they
  ship at **weight 0.0**, so they do not affect routing until you opt in. Why off: on
  the author's own prompts they lifted the cost-aware operating point from PGR 0.60 to
  0.80, but a [cross-provider double-blind test](benchmarks/blind-eval.md) showed the
  lift does not generalize — the lexicon fired on only ~20% of independently-authored
  hard prompts and lost to a plain word-count baseline. A curated keyword list detects
  an author's vocabulary, not difficulty in general. If your traffic uses a known
  vocabulary, raise these weights in your routing config and calibrate. Still no model
  call, no key, no network on the scored path (WF-ADR-0001).
- **Cost-aware routing** (WF-ADR-0017). Optional, informational cost metadata —
  `cost` on a `[[routing.tiers]]` entry and `cost_per_1k` on a `[gateway.models.*]`
  endpoint — surfaced on the `/metrics` endpoint as a gauge. A new calibration
  objective, `wayfinder-router calibrate --objective cost-quality --target-savings
  X [--costs local=0.2,cloud=1.0]`, picks the most accurate cut that still saves at
  least `X` against always-routing-high, and records the per-arm cost in the emitted
  config. Cost only moves *where the cut is placed* at calibration time and *what is
  reported*; it never enters the per-request decision, which stays deterministic and
  free. Live spend metering and token-level costing are explicitly out of scope.

### Changed

- **The JSON contract is now `schema_version` `"3"`** (was `"2"`). The `features`
  object gains the four lexical keys (reported, but weight 0.0 by default, so routing
  is unchanged); a tier in the JSON also carries `cost` when one is configured.
  Default routing decisions are **identical to v0.1.x** — the lexical features are
  off unless you opt in.

## v0.1.7 — 2026-06-19

### Added

- **A Prometheus `GET /metrics` endpoint** on the gateway (WF-ADR-0018): request
  counts by model and mode, decision-latency and upstream-latency histograms,
  upstream-error and config-reload-failure counters, and build info. Hand-rolled in
  the text exposition format with **no new dependency**, incremented at the same
  decision hook as the `/router` ring — so it carries **metadata only, never prompt
  text**, and stays off the scored path (no key, no model call, no network).

## v0.1.6 — 2026-06-18

### Added

- A **deterministic, offline benchmark** under `benchmarks/` (`make benchmark`,
  WF-ADR-0015) with metrics aligned to the routing literature (RouteLLM / RouterArena):
  quality, cost, call-fraction, performance-gap-recovered, cost savings, and decision
  latency, plus the full cost-quality curve. It reproduces byte-for-byte with no network
  or keys, ships honest baselines (always-local/cloud, stable-random, a tuned
  length-threshold, an oracle upper bound) and an illustrative dataset that **includes
  Wayfinder's failure mode**; point it at RouterBench / RouterArena for general numbers.
  Routers that need a model call to decide (RouteLLM, NotDiamond, …) get a pluggable
  adapter and a comparison citing their **published** numbers with provenance — never
  presented as ours.

### Changed

- README gains a **"How it compares"** section: the precise, defensible positioning (the
  only offline, zero-model-call, calibrate-on-your-data, self-hosted structural router),
  an honest comparison table, and a link to the reproducible benchmark.

## v0.1.5 — 2026-06-18

### Added

- **A read-only routing dashboard** (WF-ADR-0014). `GET /router` serves a tiny,
  self-contained page (no CDN, no build step) showing recent routing decisions, a
  per-model count, and scores at a glance; `GET /router/recent` is the JSON behind
  it. Decision **metadata only** — model, score, mode, request id, timestamp —
  never prompt text, kept in a bounded in-memory ring. It answers "is routing
  working?" without inspecting per-request headers, and is distinct from the
  off-path `wayfinder-router ui` operator console.
- **`X-Wayfinder-Debug: true`** (opt-in) surfaces the routing decision in the
  response — a `wayfinder` object in a non-streaming JSON body, or a trailing
  `wayfinder` SSE event on a stream — for clients that render the body but hide
  headers. The default response stays byte-clean.

## v0.1.4 — 2026-06-18

### Added

- **Streaming responses** (WF-ADR-0013). A request with `stream: true` is relayed back
  as Server-Sent-Events so chat clients (LibreChat, Open WebUI, …) render tokens
  progressively. The gateway now forwards asynchronously (`httpx.AsyncClient`), so
  concurrent requests no longer block one another.
- `wayfinder-router serve --dry-run` returns the routing decision (model, score, mode)
  without calling any upstream — try the router with no backends configured.
- A configurable upstream timeout via `WAYFINDER_ROUTER_TIMEOUT` or `serve --timeout`
  (default 60s), and an optional `WAYFINDER_ROUTER_FEEDBACK_TOKEN` that gates the
  `/v1/feedback` write behind a bearer token to prevent label-log poisoning.
- Every response carries an `x-wayfinder-router-request-id`; routing decisions, upstream
  errors, and config-reload failures are logged. `GET /healthz` reports `degraded` and
  lists `missing_keys` when a configured `api_key_env` is unset.

### Changed

- Upstream transport failures (timeout, connection refused) now return an OpenAI-shaped
  `wayfinder_router_upstream_error` (a `502`, or a terminal SSE error event for a stream)
  instead of a bare `500` with a traceback. Scoring and the WF-ADR-0001/0004 boundary are
  unchanged.

## v0.1.3 — 2026-06-18

### Added

- The gateway serves **`GET /v1/models`**, an OpenAI-compatible discovery list of
  the selectable routing options — `auto`, `prefer-local` / `prefer-hosted` (for a
  tiered/binary router), and each configured `[gateway.models]` endpoint. Any
  OpenAI-compatible client now auto-populates its model dropdown with the routing
  modes, so no hand-written model list is needed. Like `/healthz` it reads config
  only — no key, no model call, no network (WF-ADR-0012).
- Integration examples under `examples/` for putting a chat UI in front of the
  gateway with no fork: a LibreChat custom-endpoint config (`librechat.yaml`) and a
  Compose override that runs the gateway as a LibreChat sidecar, plus Open WebUI
  connection notes. They lean on the per-request override (WF-ADR-0011) so a UI's
  model dropdown becomes a per-conversation routing-mode picker.

### Changed

- The high-end routing directive is now **`prefer-hosted`** (was `prefer-cloud`),
  matching the local/hosted language used elsewhere and because the high end of a
  router is not always literally "cloud". `prefer-cloud` keeps working as a silent
  back-compat alias. `prefer-local` / `prefer-hosted` apply to a tiered/binary
  router; under a classifier (which has no ordered ladder) they now fall through to
  scoring rather than pinning (WF-ADR-0011 amendment).

## v0.1.2 — unreleased

### Added

- The gateway accepts a **per-request routing override** so a client can steer a
  single call without changing the deployment's `wayfinder-router.toml`
  (WF-ADR-0011). The OpenAI `model` field is a routing directive — `auto` (or any
  ordinary model id) scores per config, a configured endpoint name pins the call
  to that endpoint, and `prefer-local` / `prefer-cloud` pin to the low / high end
  of the router — and an `X-Wayfinder-Threshold` header (a number in `0.0`–`1.0`)
  re-cuts a binary router for that one request. Responses gain an
  `x-wayfinder-router-mode` header (`scored` / `pinned` / `threshold-override`)
  alongside the existing `-model` / `-score` headers. The override only changes
  which endpoint a request routes to; scoring stays deterministic and key-free
  (WF-ADR-0001/0004).

## v0.1.1 — 2026-06-18

### Added

- An `all` install extra that pulls in the gateway and the UI in one step:
  `pip install "wayfinder-router[all]"` (equivalent to `[gateway,ui]`). The
  deterministic core stays zero-dependency (WF-ADR-0001); `all` is only a
  convenience aggregate of the existing optional extras.

### Changed

- Redesigned the local UI (`wayfinder-router ui`) as a branded, modern surface:
  a teal-on-cream/navy palette derived from the project banners, automatic light
  and dark themes (`prefers-color-scheme`), a wordmark + tagline header, carded
  sections, primary/secondary buttons, a custom-styled threshold slider, a
  recommendation pill, and refined tables and contribution bars. Presentation
  only — no behavior, endpoint, or dependency change (WF-ADR-0005).
- README install guidance now leads with `[gateway]` — the extra you need to
  route traffic through the proxy — clarifies that the bare install is the
  zero-dependency scorer/CLI/library, and documents `[all]`. Install snippets use
  the published `pip install "wayfinder-router[...]"` form.
- README gains a **"Where Wayfinder sits"** section — a diagram and explanation
  that Wayfinder is transparent middleware behind any OpenAI-compatible client
  (e.g. Open WebUI), and that local and hosted are *backends*, not separate UIs.

## v0.1.0 — 2026-06-18

The first public release. **Wayfinder** is a deterministic prompt-complexity
router: it scores a prompt's *structure* and recommends a **local** or **cloud**
model — offline, reproducible, and with no model call to make the decision. It
ships as its own product, **fully independent of RAC**: no `rac` import, no
`.rac/` reads, stdlib-only core, Python 3.11+. It was prototyped inside
requirements-as-code and split out because routing is a runtime *inference*
concern rather than recorded knowledge (RAC ADR-069/ADR-064); the scoring
boundary it inherits is RAC ADR-070, carried over intact. Apache-2.0.

### Added

- **Deterministic structural scorer and recommendation** (WF-ADR-0001).
  `wayfinder-router route` takes a prompt (file or stdin) and returns a bounded
  `0.0–1.0` complexity score over text *structure* — word count, headings and
  their depth, list items, links, code blocks, table rows — plus a local/cloud
  recommendation, as human output or JSON (`schema_version 2`). The same prompt
  and threshold always give the same answer; there is no model call, no API key,
  and no network in the scored path. A small `score_complexity` Python API backs
  the CLI, and the package ships typed (`py.typed`).

- **Tiered routing, a fitted classifier, and offline calibration**
  (WF-ADR-0002, WF-ADR-0003). Beyond the default binary cut, route by ordered
  score **tiers** to any number of models, or by a **multinomial-logistic
  classifier**. `wayfinder-router calibrate` turns a labeled JSONL dataset into a
  `wayfinder-router.toml` fragment — threshold sweep, tier sweep, or a classifier
  fit by deterministic L2-regularized Newton/IRLS (pure Python, no dependency,
  converges in a handful of iterations). Config is layered: `wayfinder-router.toml`
  walk-up, `--threshold`, and `WAYFINDER_ROUTER_THRESHOLD`.

- **OpenAI-compatible routing gateway, bring-your-own-key** (WF-ADR-0004).
  `wayfinder-router serve` runs a proxy that scores each incoming prompt and
  forwards it to the chosen upstream — point any OpenAI-compatible client's
  `base_url` at it, no application code change. Responses carry
  `x-wayfinder-router-model` and `x-wayfinder-router-score`. Keys are read from
  the environment at request time (each model's `api_key_env`), never stored in
  config or the scored path. Ships behind the `gateway` extra, lazily imported.

- **Local calibration / explain / configure UI** (WF-ADR-0005).
  `wayfinder-router ui` serves a local web app — **Explain** (per-feature
  contribution bars and a live threshold slider), **Calibrate** (paste a dataset,
  see accuracy and the sweep curve), **Configure** (edit `wayfinder-router.toml`
  with live validation), and **Onboard**. A thin consumer of the same pure core;
  no secret ever appears in it. Behind the `ui` extra.

- **Feedback loop and A/B onboarding** (WF-ADR-0006). `wayfinder-router onboard`
  A/B-tests a local vs hosted model on sample prompts and records your
  good-enough judgment; the gateway's `/v1/feedback` endpoint captures
  steady-state judgments. The label log *is* the `calibrate` dataset, so feedback
  turns straight into an updated config — collect → calibrate → route.

- **Scheduled recalibration with live hot-reload** (WF-ADR-0007).
  `wayfinder-router recalibrate` re-fits the routing config from the feedback log
  (run it from cron or a k8s CronJob, or click *Recalibrate & save* in the UI);
  it rewrites only the `[routing]` section and **preserves** your `[gateway]`
  endpoints. A running gateway hot-reloads the new config with no restart, and a
  malformed mid-flight write keeps the last-good config.

- **Deployable packaging** (WF-ADR-0008). A slim `Dockerfile` runs the gateway as
  a sidecar or service (only the `gateway` extra), with a `docker-compose`
  example that persists config + the feedback log and shows the recalibrate
  one-shot. The library runs in-process; the CLI, UI, and onboarding are the
  operator/bootstrap surfaces. Install extras: `gateway`, `ui`, `dev`.

### Boundary

- Wayfinder scores deterministically and **recommends**; it never invokes a
  model, selects a provider, reads a credential, or tokenizes per a vendor model
  — the caller runs inference (WF-ADR-0001 / RAC ADR-070). The deterministic core
  imports no web or SDK code; only the optional gateway and UI layers touch the
  network or keys, and only from the environment.
