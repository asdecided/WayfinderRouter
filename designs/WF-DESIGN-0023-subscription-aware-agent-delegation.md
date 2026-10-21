---
schema_version: 1
id: WF-DESIGN-0023
type: design
tags: [rust, omarchy, coding-agents, subscriptions, authentication]
---

# Subscription-aware agent delegation on Omarchy

## Status

Proposed; isolated planning proof of concept. Inspected 20 October 2026 against
main commit `075f17c`. No live subscription execution or interoperability test
has been performed. This does not amend WF-DESIGN-0018 or WF-ADR-0084.

## Result

Use Wayfinder's existing deterministic planner to propose an eligible **agent**,
then hand the task to that agent's native, user-installed execution interface.
Authentication, model choice within that agent, approval prompts, sandboxing and
tool execution remain owned by the agent. This is a separate delivery mode from
proxying an agent's inference through the Wayfinder gateway.

An ordinary interactive CLI launch with an initial task is documented for both
Codex and Claude Code. It is technically feasible without extracting credentials.
It does **not** prove that the CLI will use the intended subscription: saved
provider configuration, environment overrides, account selection, extra-usage
settings and plan limits still matter. Therefore this proof of concept stops at
a reviewable plan rather than claiming subscription-only live dispatch.

## Existing contracts inspected

| Path / contract | What it supplies | What it does not establish |
| --- | --- | --- |
| `wayfinder-routing-core::score_complexity`, `assess_destination`, `plan_automatic_route` | Offline scoring, hard eligibility filters, stable ordering within the selected tier | Account access, executable discovery, credentials or permissions |
| `wayfinder-runtime-contracts` | Readiness, content boundaries, capabilities, billing classes and secret-free route plans | A subscription quota or portable coding-agent session protocol |
| `wayfinder-cli/src/codex_app_server.rs` | Opt-in managed Codex helper discovery and isolated storage | A supported Linux release helper: non-macOS production trust returns false |
| `wayfinder-codex-app-server/src/process.rs` and `src/lib.rs` | Child process lifecycle, isolated auth, bounded text turns, tool-action rejection | Coding tasks in a project, agent approvals or tool execution |
| `wayfinder-gateway/src/lib.rs` destination snapshots | Codex account provider is hosted, subscription classified, text-only and tool-ineligible | General model API access using that account |
| `wayfinder-cli/src/agent_exec.rs` / WF-ADR-0084 | Shell-free process replacement with bounded gateway probes and per-client endpoint overrides | Native subscription delivery: the purpose is to route inference through Wayfinder |
| `docs/coding-agent-quickstarts.md`, Omarchy integration `compatibility.json` | Existing client wire contracts and recorded gateway/tool smoke evidence | Native subscription execution evidence; that evidence used a different delivery path |

The macOS account provider accepts only the verified helper under
`/Applications/ChatGPT.app`; debug helper overrides are not Linux release support.
Its separate Codex home, empty workspace, strict configuration, disabled tools
and fail-closed event handling are deliberate. Do not relax them to turn Chat
into a coding agent. A Linux managed account-chat provider would need its own
runtime sourcing, version/trust and storage decision, as well as isolation tests.
That work is unnecessary for a native CLI handoff.

## Three delivery modes

| Mode | Authentication owner | Execution / billing boundary |
| --- | --- | --- |
| Model delivery through gateway | Wayfinder delivery adapter resolves the selected provider's credential | API-metered where configured, or a separately supported account provider |
| Managed `codex-app-server` account Chat | Codex in Wayfinder's isolated account store | Supported macOS account entitlement; text-only; no coding tools |
| Native agent task handoff | User-installed agent in its existing user context | Agent's own provider, account and billing rules; subscription only when established for that execution |

Do not run native subscription delegation through `wayfinder-router exec` or
inject its endpoint/model/placeholder-token overrides. Do not convert an OAuth
token to an OpenAI-compatible or Anthropic endpoint credential. An OpenCode,
Pi or Aider gateway integration is not evidence of supported access to another
vendor's consumer subscription. They remain outside this initial experiment.

## Smallest Rust boundary

The standalone `wayfinder-agent-delegation` crate is not a dependency of the
Router, gateway or Omarchy plugin. It implements only:

1. Receive an explicit routing policy, text task, caller-selected task kind and
   an ordered inventory of operator declarations for Codex / Claude Code.
2. Exclude candidates without declared subscription access or the requested
   review/edit/test capability. Require explicit automatic eligibility.
3. Map the remaining declarations to the existing secret-free destination
   snapshots and invoke `plan_automatic_route` unchanged. Both candidate types
   are conservatively hosted; running a CLI locally does not make its model
   execution on-device. No image or incremental-output capability is invented.
4. Emit a prompt-free preview with stable assessments, the core's selected
   tier and candidate ordering, and a proposed client. Every preview states
   `operator-declaration-only` and `blocked-pending-agent-auth-contract`.

`reported_readiness` and task capabilities are declarations, not live probes.
The sample inventory is synthetic, not a statement about the user's accounts.
The internal billing class stays `Unknown`; declaring a subscription does not
verify a charge, create a dollar estimate or produce a successful delivery receipt.
The preview's core `selected_destination_id` is a proposal, not an execution.
Fallback IDs are explanatory only and are never dispatched. An unavailable
selected tier remains unavailable; neither another tier nor API billing is
silently substituted. Inventory order is an operator preference, not a model
quality ranking. The existing complexity score is not evidence that a particular
agent is better at a task.

The CLI reads only its three explicit input files, bounded to 1 MiB each. It
does not discover or read agent configuration, tokens, auth stores or account
homes. It launches no process and performs no network calls. Parse diagnostics
omit input contents. A missing input or no eligible agent returns nonzero; an
ineligible preview remains inspectable on stdout. No production command,
capability handshake, release asset, default-agent selection or UI is changed.

## Reproduce the planning experiment

From the repository root with Rust installed:

```sh
cargo run --manifest-path rust/Cargo.toml \
  -p wayfinder-agent-delegation --locked -- \
  rust/crates/wayfinder-agent-delegation/fixtures/policy.toml \
  rust/crates/wayfinder-agent-delegation/fixtures/agents.json \
  rust/crates/wayfinder-agent-delegation/fixtures/task.json
```

The synthetic review proposes `personal-codex`; `personal-claude` is declared
usage-limited. Both use tier `coding`. Complex tasks selecting the intentionally
unpopulated `complex` tier fail visibly. Modify copies of these files to explore
other task, readiness or access declarations. The policy parser is the existing
strict-order parser with no environment threshold override or policy discovery.

```sh
cargo test --manifest-path rust/Cargo.toml \
  -p wayfinder-agent-delegation --locked
```

Coverage includes deterministic/core parity, stable candidate order, no implicit
tier crossing, API/unknown access exclusion, task kinds, signed-out/limited
readiness, disabled candidates, hosted privacy, unknown context limits, duplicate
identities, invalid task contracts, prompt omission, missing explicit policy and
the absence of a launch/permission-bypass command.

## Exact gap before live subscription dispatch

The missing capability is a **versioned, verified native-agent adapter** that can
bind the intended account and billing mode to the task launch, without reading
credentials or replacing the agent's own approvals. A generic executable name
and the existing gateway smoke tests cannot supply this evidence.

The smallest next implementation is one adapter first, rather than a general
multi-provider scheduler:

1. Pin a supported user-installed CLI version and validate its documented
   read-only auth/status interface in the same working directory and environment
   that will be used for launch. Claude Code documents JSON `claude auth status`
   and its `authMethod`; Codex documents `codex login status`, and app-server
   documents normalized account operations. These are different interfaces,
   not a verified common protocol. Reject missing fields, unknown versions,
   signed-out/API/third-party modes and stale status. Bound output and deadlines,
   sanitize errors and keep status data in memory.
2. Establish a non-secret identity for the user's explicitly selected account
   and show that identity plus the native execution mode before dispatch.
   Unknown identity is not authorization to choose another account. Status must
   come from the CLI, not from deserializing its credential files. Verify any
   provider/environment/configuration precedence through that agent's supported
   status interface. Do not blindly clear variables or rewrite its settings to
   manufacture a preferred account.
3. If subscription-only billing cannot be established, keep dispatch blocked.
   A claude.ai login or ChatGPT plan label alone does not prove remaining quota,
   disable extra usage or guarantee zero additional charges. Quota is unknown
   unless an agent supplies a supported status signal. Do not scrape account
   pages or estimate quota from prompts.
4. For the first live gate, hand off one task to the existing terminal/agent
   launcher with an argv vector, explicit workspace and interactive approvals.
   Use the CLI's documented initial-prompt surface; no shell interpolation,
   injected provider/model, `--yolo`, approval suppression, token forwarding or
   retries into a different account. Preserve terminal signals and exit status.
   Show launch failure, cancellation or usage-limit failure without fallback.
   A status check cannot eliminate an account-change race; revalidate immediately
   before launch and never describe the result as an atomic account guarantee.
5. Validate with fictional project data on Omarchy: intended account, signed-out,
   API overrides, conflicting provider configuration, usage limits, permission
   prompts, Ctrl-C, missing executable, errors and no implicit retry. Separate
   dispatch acceptance from task completion; record only bounded, secret-free
   status and selected identity, never an inferred successful edit.

This work extends Wayfinder's selection/delivery boundary; it does not introduce
an Agents screen, terminal selector or parallel default-agent registry. Let the
existing Omarchy Agents surface own installed agents, user selection, terminal
launch and approvals. Its host can supply the explicit inventory and consume
the proposed selection after an adapter passes the live gate. No current
subscription-delegation launcher API is assumed or claimed as verified.

Background jobs, result aggregation, durable cancellation, workspace isolation
and concurrent project mutation need a later job contract. Avoid launching
multiple writers in one worktree. A native interactive handoff is the initial
target, not evidence of an unattended multi-agent orchestrator.

## Evidence and limits

Repository evidence is at the inspected commit; online documentation was read
20 October 2026. Public CLI documentation establishes possible integration
surfaces, not tests of this branch or this user's subscriptions:

- [Codex CLI reference](https://learn.chatgpt.com/docs/cli/reference): interactive
  prompt and login/status commands.
- [Codex authentication](https://learn.chatgpt.com/docs/auth): ChatGPT subscription
  authentication and API usage-based authentication are distinct.
- [Claude Code CLI reference](https://code.claude.com/docs/en/cli-reference):
  interactive initial prompt and auth-status JSON / authentication methods.
- [Claude Code authentication](https://code.claude.com/docs/en/authentication):
  subscription, Console, cloud-provider and configured-account paths.

No credential import, native subscription task, provider interoperability,
subscription balance, live approval flow or physical Omarchy desktop acceptance
has been verified by this experiment. The production Router remains unchanged.

Implementation validation on Linux with Rust 1.99.1: all 604 workspace tests
passed, including 11 new planner/CLI tests; the shared JavaScript preview passed
all 21 parity cases. The fixture command produced the expected blocked preview.
Workspace formatting and all-target/all-feature Clippy with warnings denied
also passed. Workspace tests and Clippy used `CARGO_INCREMENTAL=0`, `CARGO_PROFILE_DEV_DEBUG=0` and
`CARGO_PROFILE_TEST_DEBUG=0` after the initial debug build exhausted disk space.
This is portable Linux evidence, not live Omarchy or account acceptance.

## Reversal

Remove this crate's workspace member, crate directory and lockfile entry to
remove the experiment. It creates no persistent settings, account state, service
or deployment. The existing Router and agent launch behavior are unaffected.
