# Rust migration discovery and capability matrix

Status: frozen discovery baseline, superseded by WF-ADR-0046 on 2026-07-24
Python baseline: `cea7d98833a4f54e3c2ebf2588fc2fc5adbad7cd` plus the preserved dirty worktree described below
Rust parity: Phases 1–3 complete; operations, CLI, and distribution gates remain

> This document preserves the 2026-07-11 discovery baseline and is not current runtime or
> distribution guidance. WF-ADR-0046 completes the Rust-only cutover, removes the legacy package
> and fallback, and sets `gateway_ready: true`. Historical rows below remain migration evidence.
>
> Mobile amendment (2026-07-24): WF-ADR-0048 and WF-ROADMAP-0016 govern pure-core extraction and
> native iOS embedding. No Python-era row in this matrix is a mobile requirement.
>
> Historical Desktop amendment (2026-07-19): this document preserves the 2026-07-11 discovery baseline.
> WF-ROADMAP-0015 supersedes its native packaging/default rows for Desktop v0.1.0. The Desktop app
> explicitly selects a bundled arm64 Rust gateway with authenticated credential and Foundation
> Models XPC services; Developer ID/notarization and clean-machine evidence remain. Its former
> Python rollback sentence is superseded by WF-ADR-0046; Intel and universal evidence remain future gates.
> Conservative `gateway_ready: false` migration metadata does not override Desktop's explicit
> verified-helper selection.
>
> Calibration erratum (2026-08-18): the historical `knee` parity target must not be ported
> unchanged. With fixed two-arm costs, the cost-ratio term is constant across candidate cuts, so
> changing only the configured prices cannot move the selected cut. WF-ADR-0015 reclassifies
> `knee` as a quality/call-fraction heuristic, and WF-ADR-0017 specifies `min-cost` for any future
> price-sensitive native calibrator. The current Rust CLI continues to reject `calibrate` and
> `recalibrate` under WF-ADR-0046.

This is the compatibility contract for the Python-to-Rust migration. It is intentionally
evidence-first: a Rust module is not considered compatible merely because it has an analogous
type or endpoint. Parity requires the relevant differential, golden, or contract tests to pass.
The pre-cutover statements below are historical evidence only. Python is no longer runnable,
packaged, tested, or supported by the repository (WF-ADR-0046).

## Repository and worktree baseline

- The repository has no root `AGENTS.md`. The only `AGENTS.md` is scoped to
  `macos/WayfinderMac/rac-core/`; that subtree is not part of the router rewrite and was not
  modified.
- The active branch is `native-menu-ux-overhaul` at `cea7d98`.
- The worktree was already substantially dirty before this migration. The dirty files belong to
  the user and must be preserved. In particular:
  - `wayfinder_router/bootstrap.py` adds a keyless, offline `local` preset.
  - `wayfinder_router/cli.py` creates missing `init --path` parents and adds
    `config read-routing` / `config apply-routing`.
  - `wayfinder_router/config.py` adds routing-family replacement and removes automatic tier
    sorting.
  - `tests/test_cli.py` adds contracts for those new behaviors.
  - the native macOS sources, tests, ADR/design/roadmap amendments, and setup work are also dirty
    or untracked.
- Migration work should prefer new `rust/`, compatibility, benchmark, and documentation paths.
  Any change to an already-dirty file requires a focused overlap review first.

## Baseline verification

| Gate | Result | Evidence / limitation |
|---|---|---|
| Python-generated scorer corpus matches the checked-in corpus | Pass | `python3 tools/golden.py \| diff - clients/shared/test/golden.json` |
| JavaScript mirror matches Python scorer | Pass, 21/21 score, route, and feature vectors | `node clients/shared/test/parity.mjs` |
| Native Swift package | Pass, 66 tests | `CLANG_MODULE_CACHE_PATH=/private/tmp/wayfinder-swift-module-cache swift test --disable-sandbox` |
| Full Python test suite | 651 pass, 1 pre-existing contract failure | `uv run python -m pytest -q`; `test_tiers_are_parsed_and_sorted` expects sorting while the preserved dirty `config.py` now rejects descending input |
| Rust toolchain | Available | `rustc`/`cargo` 1.96.0; `rustfmt` and `clippy` available |
| Rust security/license tools | Partial | `cargo-audit 0.22.2` is installed and reports no advisory in the 214-dependency lockfile with `--no-fetch`; `cargo-deny` remains unavailable, so its policy file is not yet validated |
| macOS Rust targets | Partial | `aarch64-apple-darwin` only; `x86_64-apple-darwin` is not installed |
| Signed/notarized universal app/helper | No current gate | No production app target, nested-helper signing, notarization, Intel test, or rollback harness exists |

## Current Rust progress

The first compatibility slice is additive and development-only:

- `wayfinder-core` implements feature extraction, Python-compatible rounding, scalar scoring,
  explanations, tiers, classifier inference, and schema-version-3 decisions.
- `wayfinder-config` implements routing semantic parsing/validation/emission and exposes both
  `StrictInput` and `CompatibilitySort` explicitly. Product paths use strict ascending input to
  match current Python; compatibility sorting is migration-only.
- `wayfinder-cli` implements `route`, a real bounded `serve`, launchd/systemd
  `service install|uninstall|status`, and a versioned `capabilities --json` handshake. It still
  reports `gateway_ready: false`; Python remains the selected default.
- `wayfinder-gateway` exposes the bounded health/metrics/models/profiles/recent/savings/config and
  chat skeleton, including real buffered delivery and a lazy, byte-exact OpenAI-compatible SSE
  relay. Request-atomic last-good snapshots, failed-version suppression, deterministic drain, and
  a real subprocess HTTP lifecycle harness complete Phase 2. Remaining operational and
  Anthropic-streaming gaps still prevent replacement.
- `wayfinder-providers` includes hardened buffered and streaming Reqwest clients, a bounded SSE
  parser, reliability primitives, and Anthropic buffered/streaming translation. The Anthropic
  streaming translator is wired incrementally to both Messages aliases.
- `wayfinder-service` includes deterministic pricing/ledger state, bounded legacy credential-command
  compatibility, and byte-exact launchd/systemd rendering; service-manager calls are isolated behind
  an injected CLI boundary.
- `wayfinder-compat-tests` passes ten integration tests covering 21 Python golden prompts, eight
  numeric/tier/classifier boundaries, 32 routing-config cases, 74 gateway-config cases, 20 ordered
  HTTP exchanges, and service-unit bytes. All generators reproduce the checked-in fixtures exactly.
- Rust JSON, human, and `--explain` route output are byte-identical to Python for the exercised
  current-config smoke cases.
- Workspace rustfmt and warning-denied Clippy pass offline. Focused compatibility, CLI, service,
  and provider suites pass; the full socket-backed suite requires loopback permission in restricted
  environments. `cargo audit --no-fetch` is clean. `cargo deny` and x86_64/universal builds remain
  unavailable for the reasons in the baseline table. The full Python result and its tier-order
  conflict are recorded above.

The rewrite is not complete. Virtual-key authentication, allowlist clamping, global/per-key RPM,
TPM, and spend budgets, success headers, key attribution, the bounded exact-response cache,
buffered retry/fallback/circuit-breaker delivery, buffered Anthropic Messages aliases, and raw
OpenAI SSE relay are now wired into the Rust chat path and CLI config. Major open gates include
Persistence and operational-state reload migration,
broader CLI and HTTP differentials, signed universal-helper
production evidence.

## Capability matrix

Every Rust parity status began as **Unstarted**. The progress snapshot above records the first
verified subset; the proposed owner is an architectural mapping, not permission to change behavior.

### Deterministic core, configuration, and calibration

| Capability | Python source | Tests / evidence | Proposed Rust owner | Compatibility contract and migration risk |
|---|---|---|---|---|
| Public package surface and version | `wayfinder_router/__init__.py`, `pyproject.toml` | `tests/test_packaging.py` | `wayfinder-core::lib`, Python compatibility package remains | Python exports and lazy-import behavior remain supported while both backends coexist. Version is `2026.7.0`. Medium risk. |
| Frontmatter stripping | `complexity.py:strip_frontmatter` | `test_complexity.py`, golden corpus | `wayfinder-core::features` | Only a first-line `---` begins frontmatter; `---` or `...` closes it; an unterminated block is scored intact. High exact-text risk. |
| Structural feature scan | `complexity.py:extract_features` | `test_complexity.py`, golden corpus | `wayfinder-core::features` | Eleven features in stable order; fenced contents do not count as headings/lists/tables/links; opening fences count once. Unicode/newline/regex parity is high risk. |
| Lexical features and profiles | `complexity.py`, `profiles.py` | `test_complexity.py`, `test_profiles.py`, golden corpus | `wayfinder-core::{features,profiles}` | Whole-word lower-case term matching, LaTeX/math regex, literal `?` count, stable profile IDs/order/provenance. High regex risk. |
| Normalization and scalar score | `complexity.py:normalized_features`, `scalar_score` | `test_complexity.py`, `test_explain.py`, golden corpus | `wayfinder-core::score` | Saturation per feature, weighted normalization, Python-compatible float accumulation, score rounded to two decimals before routing; zero total weight returns `0.0`. Critical numeric parity. |
| Explanation breakdown | `complexity.py:explain_score` | `test_explain.py`, CLI tests | `wayfinder-core::explain` | Stable feature order; normalized values and contributions round to four decimals. High numeric/output risk. |
| Tier routing | `complexity.py:Tier`, `recommend_tier` | `test_complexity.py`, `test_config.py` | `wayfinder-core::tier` | Highest inclusive `score >= min_score` band wins. Boundary `0.0` routes a zero-score prompt upward. Critical due to tier-order conflict below. |
| Classifier inference | `complexity.py:ClassifierModel` | `test_complexity.py`, `test_config.py` | `wayfinder-core::classifier` | Linear logits over normalized features; unspecified feature vectors are zero; strict `>` argmax yields first-model tie break. Critical numeric parity. |
| Decision JSON | `ComplexityScore.to_dict` | `test_cli.py`, golden corpus | `wayfinder-core::decision` | Schema version is the string `"3"`; stable mode/features; optional tier cost omitted when absent; classifier emits models. High wire-contract risk. |
| Config discovery | `config.py:find_config_file` | `test_config.py` | `wayfinder-config::discovery` | Explicit `WAYFINDER_CONFIG` suppresses ancestor discovery even when the path is missing; otherwise nearest ancestor wins. High undocumented-contract risk. |
| Routing config parse | `config.py:routing_config_from_toml` | `test_config.py` | `wayfinder-config::{schema,validate}` | Classifier > tiers > threshold; weights merge over defaults; booleans are not numbers; lexicon capped at 2,000 terms; classifier vectors have exact widths. Critical schema parity. |
| Threshold environment override | `config.py:_apply_env_threshold` | `test_config.py` | `wayfinder-config::environment` | Applies only in binary-threshold mode; empty means absent; invalid/out-of-range value is a config error. Medium risk. |
| Routing TOML emission | `config.py:dump_routing_toml` | `test_config.py`, calibration tests | `wayfinder-config::emit` | Deterministic six-decimal formatting; changed weights/lexicon only; full classifier; non-classifier output uses tier arrays. High escaping and byte-stability risk. |
| Boolean config edit | `config.py:set_toml_bool` | `test_config.py`, `test_cli.py` | `wayfinder-config::edit` | Only whitelisted keys; preserve every unrelated line; add under an existing table or append a missing table; reparse before write. High preservation risk. |
| Routing-family config edit | dirty `replace_routing_toml`, dirty CLI handlers | dirty `test_cli.py` | `wayfinder-config::edit` | `read-routing` emits JSON; `apply-routing` accepts a constrained stdin fragment and preserves non-routing sections. Unknown future routing fields/comments need an explicit preservation decision. Critical dirty-contract risk. |
| Dataset parsing and feedback labels | `calibrate.py`, `feedback.py` | `test_calibrate.py`, `test_feedback.py` | `wayfinder-core::dataset`, `wayfinder-service::feedback` | JSONL `{text,label}`, blank-line tolerance, append order, missing log as empty. Medium malformed-input/atomicity risk. |
| Threshold calibration | `calibrate.py` | `test_calibrate.py`, `test_cli.py` | `wayfinder-core::calibration::threshold` | Two labels; mean-score/name ordering; observed cuts rounded to four decimals plus zero; accuracy ties use upper median; accuracy/knee/cost-quality objectives. Critical tie and float parity. |
| Tier calibration | `calibrate.py` | `test_calibrate.py` | `wayfinder-core::calibration::tiers` | Explicit or mean-derived label order with adjacent sweeps. Critical because equal adjacent cuts can currently emit config the parser rejects. |
| Classifier fitting | `calibrate.py:fit_classifier` | `test_calibrate.py` | `wayfinder-core::calibration::irls` | Deterministic Newton/IRLS, zero initialization, L2 ridge, fixed feature/model order, partial-pivot Gaussian elimination. Critical cross-platform numeric parity. |
| Feedback-driven recalibration | `recalibrate.py` | `test_recalibrate.py` | `wayfinder-cli::recalibrate`, `wayfinder-config::merge` | Below minimum labels is a successful no-op; otherwise update routing while preserving gateway and, per product requirement, unrelated/unknown content. Current Python reconstruction loses formatting/comments/unknown sections. Critical migration risk. |
| Human onboarding | `onboard.py`, CLI handler | `test_onboard.py`, CLI tests | `wayfinder-cli::onboard` | Calls every arm, accepts known-arm judgment or abstention, records incrementally, optional calibration. High duplicate-call/partial-log risk. |
| Automated judge and trust gates | `judge.py`, `sufficiency.py` | `test_judge.py`, `test_sufficiency.py` | `wayfinder-core::{judge,sufficiency}` | Ordered deterministic refusal/agreement/similarity rules; `heuristic-2`; abstention; gold-set kappa, nondegeneracy, and cross-validation lift gates. High text-normalization parity and response-retention risk. |
| Pricing and ledger | `pricing.py` | `test_pricing.py` | `wayfinder-core::pricing`, `wayfinder-service::ledger` | Four chars/token fallback; real vs relative tables; 12-char table hash; UTC buckets; tolerant old persisted schema; per-route/key attribution; atomic replace. Critical persisted-state parity. |

### Gateway, providers, streaming, and operations

| Capability | Python source | Tests / evidence | Proposed Rust owner | Compatibility contract and migration risk |
|---|---|---|---|---|
| Gateway TOML schema | `gateway.py:GatewayConfig`, parse/dump helpers | extensive `test_gateway.py`, `docs/gateway-config.md` | `wayfinder-config::gateway` | Models, route scope/sticky/slash/offline, retries/breaker/failover, budget/cache/rate limits/keys. Unknown fields are currently ignored and lost on dump. Critical compatibility/security tension. |
| Hot reload | `gateway.py:_ConfigHolder` | reload tests in `test_gateway.py` | `wayfinder-gateway::reload` | Mtime-triggered; retain last-good config on parse failure and do not retry the same mtime. Cache/rate limits reconfigure; breaker/key-command state does not. High state-machine parity. |
| Health | `GET /healthz` | gateway tests, ADR-0025 | `wayfinder-gateway::routes::health` | Config/key-presence only; no provider probe. Zero models is `status=ok`; offline and `missing_keys` are exposed. High native-client schema importance. |
| OpenAI model discovery | `GET /v1/models`, `/models` | gateway tests, ADR-0012 | `routes::models` | Directives then configured models in insertion order; `created=0`, `owned_by=wayfinder`. High client compatibility. |
| Savings API | `GET /v1/savings`, `/savings` | gateway/pricing tests | `routes::savings` | today/7d/30d/all plus current unknown-period-as-all behavior; per-route/key fields and `priced` truthfulness. High schema/persistence risk. |
| Recent routes and dashboard | `/router/recent`, `/router` | gateway tests, ADR-0014, ADR-0074 | `routes::{recent,dashboard}` | Bounded 200-entry prompt-free metadata ring; newest first; limit clamped 1–200. Entries include request/model/score/mode/time and may include policy, key, cost, cache, actual served route, execution boundary, and bounded delivery outcome/error metadata. High privacy/schema risk. |
| Metrics | `GET /metrics`, `gateway.py:Metrics` | `test_metrics.py`, ADR-0018 | `wayfinder-gateway::metrics` | Prometheus counters/gauges/histograms for requests, latency, upstream errors, cache, limits, keys, reloads, cost/savings; no prompt text. High concurrency/cardinality/escaping risk. |
| Demo, profiles, model status | `/demo`, `/router/profiles`, `/router/models` | gateway and UI tests | `routes::{assets,profiles,model_status}` | Existing HTML and exact read-only model/key-ready schema; endpoint and env-var names are exposed but never secret values. Medium/high compatibility and metadata risk. |
| Config preview and feedback endpoints | `POST /router/config`, `/v1/feedback` | gateway/UI/feedback tests | `routes::{config_preview,feedback}` | Config preview is constrained/read-only; feedback may require an environment token but is open by default and persists raw prompt labels. Critical auth/storage risk. |
| OpenAI chat aliases | `POST /v1/chat/completions`, `/chat/completions` | `test_gateway.py` | `routes::chat` | Real upstream success is relayed, routing headers added, and upstream headers mostly discarded. Bare alias is a compatibility contract. Critical wire parity. |
| OpenAI Responses aliases | `POST /v1/responses`, `/responses` | gateway Responses and Codex tool-contract tests, WF-ADR-0066 | `wayfinder-gateway::responses` | Strict bounded text/multi-turn/function-tool adapter over the authenticated Chat Completions path; function, custom/freeform, and namespace identities are restored; normalized usage and SSE events; unsupported fields fail specifically. Critical compatibility/bounds contract. |
| Modality capability contracts | embeddings, image, audio, batch capability flags and hard exclusions | routing-core/FFI/gateway modality tests, WF-ADR-0067 | `wayfinder-runtime-contracts`, `wayfinder-routing-core`, `wayfinder-apple-ffi`, `wayfinder-gateway::modalities` | Non-text surfaces are independently gated and currently fail-closed before provider delivery; no surface is advertised without an adapter and parity fixtures. High compatibility/bounds risk. |
| Request IDs and routing headers | chat handler | gateway tests | `wayfinder-gateway::response_meta` | 12 lower-case hex ID; score is two decimals; model/mode and optional served-by/failover/budget/offline/cache/decision-only plus rate headers. Critical exact-header parity. |
| Route scope | `extract_prompt` | gateway tests, ADR-0021 | `decision_policy::scope` | `turn`, `last_user`, `user`, `all`; text parts joined; role-filter fallback to last message. High multi-turn parity. |
| Pin, threshold, sticky, tuning, slash overrides | gateway policy helpers | gateway tests, ADRs 0011/0022/0023/0036 | `decision_policy` | Pin > slash pin > threshold/scoring/sticky, with exact accepted aliases and modes. Unknown model IDs score normally. Critical precedence parity. |
| Offline delivery | gateway handler/policy | gateway tests, ADR-0039 | `wayfinder-gateway::offline_policy` | Decision remains unchanged; delivery uses cheapest tier; cache/budget interactions and offline header preserved. Production Rust must additionally prove the offline set is truly local before making a no-egress claim. Critical security decision. |
| OpenAI-compatible provider client | forward helpers | gateway tests | `wayfinder-providers::openai_compat` | `base_url.rstrip('/') + /chat/completions`, replace upstream model, optional Bearer auth, explicit timeouts. Critical provider/error parity and SSRF hardening. |
| Incoming Anthropic Messages adapter | `anthropic_adapter.py`, `/v1/messages`, `/messages` | `test_anthropic_adapter.py`, gateway tests, DESIGN-0011 | `wayfinder-providers::anthropic` | Buffered aliases reuse the Rust chat path with text/tool translation, model echo, decision headers, auth, cache, and error reshaping; streaming aliases incrementally translate bounded OpenAI SSE. |
| Gemini/local providers | presets plus generic relay | bootstrap/gateway tests | `wayfinder-providers::openai_compat` | Current Gemini and local support is through OpenAI-compatible endpoints, not a separate native adapter. Medium documentation/parity risk. |
| Native Anthropic upstream | intentionally not implemented | Phase 3 preset and bootstrap tests | future optional adapter | The default hybrid preset now uses OpenAI's compatible endpoint. Anthropic's native Messages URL is no longer falsely advertised as OpenAI-compatible; native outbound Anthropic can be added later as an explicit provider type. |
| Buffered provider errors | forward helpers and chat handler | gateway tests | `wayfinder-providers::error` | Transport exhaustion maps to Wayfinder 502, circuit open to 503; ordinary upstream 4xx body/status passes through; retryable set is exact. Critical error-envelope parity. |
| OpenAI SSE relay | `aforward_stream`, chat handler | stream tests, ADR-0013 | `wayfinder-providers::sse` | Rust now establishes upstream lazily, relays incremental bytes exactly, bounds SSE frames, accounts only clean completion, and appends a terminal error plus `[DONE]` on transport/parser failure. Upstream status parity and explicit disconnect/backpressure evidence remain open; streaming intentionally uses one plan target without retries. Critical correctness/security gap. |
| Anthropic SSE translation | `MessagesStreamTranslator` | adapter tests | `anthropic::sse_state` | Explicit event sequencing; fragmented frames; empty output; missing `[DONE]`; tool calls emitted complete in first-seen order. Critical parser/state parity. |
| Retry/backoff/breaker | `reliability.py`, gateway handler | `test_reliability.py`, gateway tests, ADR-0031 | `wayfinder-providers::reliability` | Default two retries means three attempts; exact retry status set; exponential capped full jitter; per-target breaker; auth failures count as failures, other 4xx reset. Critical concurrent-state parity. |
| Delivery planning and failover | `reliability.py` | reliability/gateway tests | `wayfinder-gateway::delivery_plan` | Same-tier default; optional degrade/escalate; deduped fallback plan; context-window precheck. Streaming currently uses only first plan entry. Critical behavior gap. |
| Exact-match cache | `cache.py`, gateway integration | `test_cache.py`, gateway tests, ADR-0033 | `wayfinder-gateway::cache` | Opt-in in-memory LRU, TTL 300s, 1,024 entries, 64 MiB; canonical key excludes model/stream; deterministic non-stream requests only; disabling purges. High privacy/bounds parity. |
| Budgets | gateway plus pricing ledger | gateway/pricing tests, ADR-0032 | `wayfinder-gateway::budget` | Prior realized spend `>= limit`; only real-priced tables; degrade or 402 block; offline softens; global and per-key policies. Critical race/accounting parity. |
| Rate limits | `ratelimit.py`, gateway integration | `test_ratelimit.py`, gateway tests, ADR-0034 | `wayfinder-gateway::limits` | Fixed monotonic windows; global admission before auth/scoring, per-key after auth; RPM reserve on admission; TPM accounts completed calls and may overshoot one request. High concurrency parity. |
| Virtual keys | `vkeys.py`, gateway integration | `test_vkeys.py`, gateway tests, ADR-0035 | `wayfinder-gateway::auth` | SHA-256 hashes, constant-time all-hash comparison, Bearer and current bare-token tolerance, per-key budget/rate/allowlist, cheaper-first clamp. Critical auth/timing parity. |
| Decision-only modes | gateway handler | gateway tests | `wayfinder-gateway::decision_only` | Explicit dry-run and zero-configured-model behavior both return HTTP 200 decisions but are distinguishable. High client compatibility. |

### CLI and service contracts

| Command / capability | Python source | Tests | Proposed Rust owner | Compatibility contract and migration risk |
|---|---|---|---|---|
| Global help/version/parsing | `cli.py:build_parser`, `main` | CLI/packaging tests | `wayfinder-cli::args` | Required subcommand, argparse-compatible exit 0/2 behavior, stdout/stderr placement and help text. High subprocess-contract risk. |
| `route` | `_cmd_route` | `test_cli.py` | `commands::route` | File/stdin, threshold/json/explain, exact exit 0/1/2 classification and deterministic output. High parity. |
| `calibrate` | `_cmd_calibrate` | CLI/calibration tests | `commands::calibrate` | All modes/objectives/costs/weights/solver knobs; TOML stdout/file and summary stderr. High numeric/output parity. |
| `recalibrate` | `_cmd_recalibrate` | recalibration tests | `commands::recalibrate` | Minimum-label successful no-op, config preservation, cron-friendly codes/messages. High persistence risk. |
| `serve` | `_cmd_serve` | CLI/gateway tests | `commands::serve` | Loopback/8088 defaults, timeout/config/dry-run, clear missing-component behavior. Medium/high. |
| `webchat` | `_cmd_webchat` | CLI tests | `commands::webchat` | Gateway launcher, banner/setup nudge, optional delayed browser open, no-open/dry-run. Medium. |
| `ui` | `_cmd_ui`, `ui.py` | `test_ui.py` | compatibility decision pending | Local calibration/explain/configure UI is a Python behavior but not a native gateway responsibility. Must remain available or be explicitly deprecated. High scope risk. |
| `chat` | `_cmd_chat`, `tui.py` | `test_tui.py`, CLI tests | compatibility decision pending | Terminal chat, themes, streaming/history/threads, dry run and remote base URL. Must remain available or be explicitly deprecated. High scope risk. |
| `onboard` | `_cmd_onboard` | onboarding/CLI tests | `commands::onboard` | Arms/log/calibrate/mode, comparison stderr and config stdout, partial progress persistence. High provider-call risk. |
| `judge` | `_cmd_judge` | judge/sufficiency/CLI tests | `commands::judge` | Gold/trust gates, cost cap, optional raw comparison body store, labels kept even when gates reject. High privacy/cost risk. |
| `init` | `_cmd_init`, `bootstrap.py` | bootstrap/CLI tests, dirty local tests | `commands::init` | Hybrid/OpenAI/Gemini plus dirty local preset, interactive wizard, force/print/keychain/path, byte-stable templates, no secret values. Critical native setup seam. |
| `doctor` | `_cmd_doctor`, bootstrap helpers | bootstrap/CLI tests | `commands::doctor` | Config/key readiness only, never provider uptime; exact exit 0/1/2 and remedies. High native readiness contract. |
| `config` | `_cmd_config`, config helpers | config/CLI tests, dirty tests | `commands::config` | Whitelisted set plus dirty read/apply routing verbs; CLI is sole config author. Critical dirty native seam. |
| `keys new` | `_cmd_keys`, `vkeys.py` | vkey/CLI tests | `commands::keys` | Plain key printed exactly once plus pasteable hashed TOML; IDs/tags safely encoded. Critical intentional secret-on-stdout seam. |
| `service install/uninstall/status` | `_cmd_service`, `service.py` | `test_service.py` | `wayfinder-service`, `commands::service` | launchd macOS primary and systemd-user Linux; exact unit text; idempotent/recoverable manager calls; status can be successful while absent/unreachable. Critical external-state risk. |
| launchd unit | `service.py:launchd_plist` | service tests, ADR-0038 | `wayfinder-service::launchd` | Stable label `com.wayfinder-router.gateway`, RunAtLoad/KeepAlive, absolute logs, XML escaping, explicit config path. Critical migration/rollback compatibility. |
| systemd-user unit | `service.py:systemd_unit` | service tests | `wayfinder-service::systemd` | Stable unit and shell quoting, restart behavior, per-user path. High cross-platform compatibility. |

### Native macOS, packaging, and distribution

| Capability | Current source / evidence | Proposed Rust or Swift owner | Compatibility contract and migration risk |
|---|---|---|---|
| Native data plane | `GatewayWayfinderClient.swift` and tests | Rust gateway preserves loopback HTTP; Swift client remains | `/healthz`, `/router/models`, `/router/recent`, `/v1/savings`, and debug chat schemas are source-of-truth contracts. Critical. |
| Native process ownership | ADR-0042/0038, `GatewayServiceController.swift` | launchd owns Rust helper; Swift renders/requests operations | App must not supervise the helper or compete for port 8088. Critical. |
| Native config seam | ADR-0044, dirty Python verbs, `RoutingConfigStore.swift` | Rust CLI is sole config author; Swift invokes fixed verbs | Existing config never overwritten implicitly; unknown content preserved. Critical committed/dirty mismatch. |
| Apple Foundation Models live path | `docs/apple-foundation-models-live-integration.md`; gated signed Apple Silicon run on 2026-07-15 | Rust gateway/helper plus separate Swift inference XPC service | Availability, bounded buffered generation, terminal-complete streaming, and cross-connection cancellation passed without content output. Test harness proves the signed protocol path; final app-closed production placement and clean-machine evidence remain Step 10 gates. Apple is not the default. |
| Provider credential setup | `KeychainCredentialStore.swift`, bootstrap key command | Swift Security.framework/XPC broker design; Rust typed secret references | No credential in argv/files/logs/persisted Rust state. Preserve legacy key-command configs without silent rewrite. Critical. |
| Native readiness language | gateway/native tests and roadmap | gateway truth, Swift rendering | Ready means configured and key-present, not verified provider uptime. Offline alone may claim no egress. High product-truth risk. |
| Signed helper embedding | not implemented | universal Rust executable nested and signed before app | Stable bundle path, nested signing, hardened runtime, notarization/stapling, update and rollback. Critical missing gate. |
| Universal macOS artifact | not implemented | release packaging | Build arm64 and x86_64 with deployment target compatible with the macOS 14 app, combine with `lipo`, verify both slices. Critical missing target/real-hardware gate. |
| Homebrew coexistence | roadmap only | installer/discovery contract | Formula, bundled helper, and Python service must not fight over the launchd label/port; explicit backend/version handshake. Critical. |
| Docker/Compose | Dockerfile, Compose examples, CI build | Rust Linux gateway image added while Python path remains | Existing environment/config/port contracts and rollback remain. High platform parity. |
| PyPI distribution | release workflow | Python path retained | Do not remove or repurpose current tag/PyPI lane until Rust packaging is proven. Medium/high rollback risk. |
| Performance gates | current benchmark is routing-quality oriented | Rust benchmark harness plus Python comparator | Cold start, idle memory, routing latency, HTTP overhead, streaming first-forward, concurrency, config parse. Critical missing evidence. |
| Security gates | `SECURITY.md`, current tests | all Rust crates plus compatibility harness | No secrets/prompts in logs by default; loopback binding; payload/queue/time bounds; URL/SSRF validation; constant-time auth; no user-input panics. Critical. |

## Status and error compatibility

The existing gateway uses these primary error contracts:

| HTTP status | Code / behavior |
|---|---|
| 400 | `wayfinder_router_bad_override` |
| 401 | `wayfinder_router_unauthorized`, with `WWW-Authenticate: Bearer` |
| 402 | `wayfinder_router_budget_exhausted` |
| 422 | FastAPI validation envelope for malformed JSON/shape; normally lacks Wayfinder decision headers |
| 429 | `wayfinder_router_rate_limited`, `Retry-After`, and rate-limit metadata |
| 500 | `wayfinder_router_misconfigured` |
| 502 | `wayfinder_router_upstream_error` after buffered delivery attempts fail |
| 503 | `wayfinder_router_circuit_open` |
| upstream ordinary 4xx | Pass through upstream status/body |

Any intentional improvement to these envelopes requires an ADR amendment, differential fixture,
and migration note. Security bounds that reject previously accepted pathological input must be
explicitly recorded as intentional changes rather than normalized away.

## Behavior encoded only in tests or weakly documented

- Score rounding occurs before the inclusive tier comparison.
- Classifier ties select the first configured model.
- Explicitly missing `WAYFINDER_CONFIG` disables ancestor discovery.
- Existing boolean edits preserve every unrelated byte; a missing `[gateway]` may be appended after
  its subtables.
- Environment credentials beat command resolution; command output is stripped and empty output is
  a failure; keyless models are ready.
- Request IDs are 12 lower-case hexadecimal characters.
- Bare endpoint aliases exist for models, savings, chat completions, and Messages.
- `/router/recent` clamps limits and returns newest first.
- Invalid failover overrides currently fall back silently; invalid offline tokens mean false.
- Unknown model IDs in the OpenAI `model` field are treated as automatic routing, not rejected.
- A zero-model gateway is healthy and returns decision-only responses.
- Hot-reload failure is attempted once per mtime and retains last-good state.
- Virtual-key authentication accepts a bare token as well as `Bearer`.
- Unknown savings periods currently behave as `all`.
- Anthropic streaming buffers fragmented lines and complete tool calls; a missing `[DONE]` still
  emits closing events.
- Streaming debug metadata currently follows upstream `[DONE]`, so many clients never see it.
- `keys new` deliberately prints the one plaintext virtual key to stdout exactly once.
- Service installation success is verified through manager status, not merely the first manager
  command's exit code.

## Open compatibility conflicts requiring resolution

1. **Tier ordering:** accepted ADR-0002 and committed `test_tiers_are_parsed_and_sorted` require
   unordered tiers to be sorted. The dirty `config.py` rejects descending tiers, and dirty CLI
   tests require `config apply-routing` to reject them. The least disruptive candidate is to keep
   tolerant sorting when loading an existing file while requiring ordered input on the new mutation
   seam, but this is not yet an accepted behavior decision.
2. **Missing explicit config:** the implementation falls back to defaults when an explicit
   `WAYFINDER_CONFIG` path is missing, while some documentation implies that this should be a hard
   error.
3. **Unknown fields:** current parsers mostly ignore unknown fields and dumpers may lose them. The
   migration requirement says existing configuration must stay compatible and never be overwritten
   implicitly, favoring lossless document retention even where semantic parsing ignores a field.
4. **Offline truth:** current offline mode selects the cheapest configured tier, not a proven local
   endpoint, and fallbacks can be remote. Rust cannot honestly guarantee no egress without a
   locality-enforced delivery set or an intentional change to the claim.
5. **Legacy secret commands:** `api_key_cmd` is an existing config contract but currently uses a
   shell and can expose command text/stderr. Rust must retain compatibility without reproducing
   insecure transport or silently rewriting config.
6. **Native Anthropic upstream:** incoming Anthropic Messages compatibility exists, but the default
   hybrid preset's Anthropic upstream is not actually supported by the generic OpenAI relay.
7. **CLI breadth:** the Python local web UI and terminal chat are real supported commands but are
   outside the preferred Rust workspace shape. They must remain on the Python path until ported or
   explicitly deprecated through a reviewed decision.

## Risk register

| Priority | Risk | Mitigation / required evidence |
|---|---|---|
| Resolved | Dirty tier-order contracts disagreed | Strict ascending input follows the current Python parser and prevents silent reordering; legacy sorting is explicit migration-only behavior. |
| P0 | Secret command execution and credential leakage | Typed redacted secret references, no shell by default, compatibility broker, adversarial argv/log/crash tests, real Keychain smoke test |
| P0 | Offline mode can still reach remote endpoints | Validate local delivery/fallback closure before no-egress claim; add fake DNS/redirect/remote endpoint tests |
| P0 | Bundled, Homebrew, and legacy helpers can collide | Explicit backend selection, capability/version handshake, one launchd label owner, tested rollback |
| P0 | Streaming treats upstream errors/cancellation weakly | Explicit parser/state machines, status check before 200, bounded frames/queues, disconnect/cancellation/backpressure tests |
| P1 | Arbitrary `base_url` enables SSRF and credential forwarding | Parse and validate URL/scheme, disable redirects/proxy surprises as policy dictates, test loopback/metadata/private targets |
| P1 | Unbounded request, response, SSE, and tool-call memory | Body/frame/accumulator/queue limits with intentional-change fixtures |
| P1 | Concurrent budgets/limits can overspend | Atomic synchronized state, deterministic clock tests, documented one-request TPM overshoot if retained |
| P1 | Unknown config/comments are lost by reconstructive writes | Use a document-preserving representation for mutation and test byte preservation |
| P1 | Python baseline cannot currently run | Obtain authorization for the locked dev environment; record full collection count and failures before claiming parity |
| P1 | Axum/CLI/security tools and Intel target are unavailable | Obtain dependency/toolchain authorization; pin versions and lockfile; verify offline/reproducible builds |
| P1 | macOS production packaging is absent | Add app target/helper embed/sign/notarize workflow and real Apple Silicon + Intel clean-machine gate |
| P1 | Native client and CLI seams exist partly only in dirty work | Treat current working tree as evidence, avoid overwriting, and add explicit capability-version probing |
| P2 | Process-local cache/breaker/budget state differs in multi-worker deployments | Keep one helper process in native path; document container limitations; shared-state work remains separate |
| P2 | Metrics/recent/savings expose operational metadata | Bind locally by default, scope virtual-key/control-plane policy explicitly, sanitize labels, test no prompt/secret leakage |
| P2 | Platform claims diverge | Publish an explicit matrix for macOS app/helper, Linux container/service, Windows CLI, and retained Python support |

## Proposed bounded workflows

The primary integration owner maintains architecture, compatibility policy, module boundaries,
shared fixtures, and final gate results. Parallel workflows must return code/tests/artifacts, not
general commentary:

1. **Routing core:** port feature extraction, scoring, tiers, classifier inference, explanations;
   generate Python/Rust golden vectors including Unicode, newline, fence, and rounding boundaries.
2. **Configuration:** lossless TOML document handling, semantic validation, discovery/environment
   precedence, config CLI seam, and valid/invalid differential corpus.
3. **Gateway HTTP:** endpoint/error/header schemas, request bounds, health/readiness, graceful
   shutdown, and local fake-provider contract tests.
4. **Providers/streaming:** OpenAI-compatible relay, incoming Anthropic adapter, explicit SSE state
   machines, cancellation/backpressure, fragmented/malformed/error streams.
5. **Operations:** cache, pricing/ledger, budgets, limits, virtual keys, failover/breakers, metrics,
   offline rules, secret wrappers, and security/property tests.
6. **CLI/service:** every command/option/code/stdout/stderr contract plus launchd/systemd text and
   idempotent manager seams.
7. **macOS embedding/packaging:** helper lifecycle, credential boundary, universal build, nested
   signing/notarization, Homebrew coexistence, update/rollback, clean-machine scripts.
8. **Compatibility/security review:** differential orchestration, fixture normalization review,
   fuzz/property tests, dependency policy, secret scans, SSRF/path/DoS analysis.

## Phased implementation plan and exit gates

| Phase | Deliverable | Exit gate |
|---|---|---|
| 0 — discovery | This matrix, risk register, workflow split, architecture and helper ADRs | Conflicts are explicit; no broad Rust scaffold predates the contract |
| 1 — deterministic kernel | `wayfinder-core`, `wayfinder-config`, initial CLI route surface, Python/Rust golden harness | All scorer vectors and valid/invalid routing config fixtures match; no user config is mutated |
| 2 — compatible gateway skeleton | Tokio/Axum service, health/models/read-only APIs, dry-run chat, exact error/header fixtures | Differential local HTTP suite passes without provider calls; bounded bodies and graceful shutdown proven |
| 3 — providers and streaming | OpenAI-compatible relay and Anthropic inbound adapter with fake providers | Buffered and fragmented streaming fixtures, provider errors, disconnect/cancellation/backpressure pass |
| 4 — operational controls | Retry/failover, offline, cache, ledger/budget, limits, vkeys, metrics/reload | Differential state tests and security invariants pass; no secret/prompt leakage |
| 5 — CLI and service | Full compatible commands or explicit retained-Python delegation, launchd/systemd | Subprocess code/stdout/stderr suite passes; install/uninstall/status are idempotent and rollback tested |
| 6 — macOS and packaging | Bundled universal helper, authenticated credential seam, signing/notarization/Homebrew design and implementation | Swift tests plus real-Mac arm64/x86_64 clean install/update/restart/rollback/key tests pass |
| 7 — evidence and opt-in | Benchmarks, cargo audit/deny, platform docs, Rust opt-in selector and shadow-safe fixtures | Python and Rust gates green; parity gaps zero or explicitly deprecated; Rust recommendation reviewed |
| 8 — default decision | Separate reviewed change selecting Rust by default | Signed artifacts, rollback, local/hosted/hybrid/offline and streaming parity all demonstrated |

Rust is **not ready to become the default gateway** at this discovery baseline.
