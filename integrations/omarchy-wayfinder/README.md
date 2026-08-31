# Wayfinder for Omarchy

Wayfinder is the model router built for Omarchy. One plugin installs the native
Rust Router, gives supported coding agents a shared local endpoint, and exposes
health, delivery receipts, local-versus-hosted distribution, model readiness,
global savings, and honest per-project value evidence in the Omarchy bar.

The plugin is the flagship product surface; the independently supervised
`wayfinder-router` process remains the only routing authority. Reloading or
disabling the Omarchy shell does not interrupt requests.

Wayfinder is Omarchy-primary, not Omarchy-only. The portable Router and the
governing [Omarchy-first strategy](https://github.com/asdecided/WayfinderRouter/blob/main/decisions/WF-ADR-0073-omarchy-first-portable-core.md)
live in `asdecided/WayfinderRouter`.

## Requirements

- Omarchy Quattro with third-party plugin support.
- An x86_64 or ARM64 glibc-based Linux system.
- `curl`, `tar`, `sha256sum`, `flock`, `systemd --user`, and `wl-copy` (provided by a
  standard Omarchy installation).

The plugin never stores provider credentials. It reads only the gateway's
prompt-free local status surfaces.

See the exact [supported versions and evidence](docs/compatibility.md) before
installing on a moving Quattro system. Support claims are pinned to reviewed
Omarchy, Quickshell, Router, and coding-agent contracts rather than inferred
from generic compatibility.

## Install

```sh
omarchy plugin add https://github.com/asdecided/omarchy-wayfinder.git --enable
```

Open the Wayfinder bar item and choose **Set up Wayfinder**. When the Router is
missing, that explicit action runs the plugin's bounded bootstrap mode: it
downloads the native
`router-v1.0.0` archive for the current architecture and verifies its pinned
SHA-256 digest before extraction. It does not require Rust or Cargo. An existing
`wayfinder-router` executable is never replaced.

The same explicit, resumable first-run flow then:

1. installs the checksum-verified Router only when no Router is already available;
2. checks for an existing policy without changing it;
3. creates a no-clobber `local` policy at
   `${XDG_CONFIG_HOME:-$HOME/.config}/wayfinder/wayfinder-router.toml` only
   when no policy exists;
4. validates that policy with `wayfinder-router doctor --json`; and
5. installs and starts `~/.config/systemd/user/wayfinder-router.service`
   through the native Router CLI.

An existing policy is never overwritten. Invalid policy stops setup with a
specific error and can be rechecked after repair. Missing provider environment
is listed by variable name but does not prevent the local service from being
installed. Interrupted setup is safe to resume from the bar.

For bounded diagnostic commands, source update steps, and current rollback
limits, use the [troubleshooting guide](docs/troubleshooting.md).
Release candidates also require the operator-run [native Omarchy smoke](docs/native-smoke.md);
hosted contract tests cannot attest that a real Quickshell session rendered the widget.

The Router binary is installed to `~/.local/bin` by default. A shell that starts
Omarchy may set `WAYFINDER_BIN_DIR` when another user-owned binary directory is
required. Router upgrades remain explicit plugin changes: the
release version and both architecture digests are reviewed before these pins
move. The installer records the exact release, target, archive digest, binary
digest, and user-owned path for a binary that it installs. Router `1.0.0` is the
current reviewed pin; `2026.8.1` is the immutable final DateVer release.
Lifecycle operations treat exact versions and reviewed checksums as identities
and do not compare the two numbering schemes.

When a later reviewed plugin pin changes, upgrade only that plugin-owned binary
with:

```sh
./install.sh --upgrade-router
```

The installer verifies the active digest, stages the new binary on the target
filesystem, preserves the previous verified binary as last known good, and
atomically promotes it. Roll back with `./install.sh --rollback-router`.
Interrupted transactions recover automatically; `./install.sh --recover-router`
also makes that recovery explicit. Independently installed or modified Router
binaries are never replaced. Promotion changes the on-disk executable; restart
`wayfinder-router.service` from the panel after upgrade or rollback to run it.
The first rollback after upgrading from `2026.8.1` restores that exact verified
binary; running the explicit upgrade again returns to the reviewed `1.0.0`
archive. The recorded archive and installed-binary digests are checked on every
transition.

Without `--enable`, Omarchy clones third-party plugins disabled so their source
can be reviewed before enablement. The standard command above requests enablement
through Omarchy's own confirmation and placement flow.

## Configure

Use **Setup → Plugins → Wayfinder** to change:

- **Gateway endpoint** — defaults to `http://127.0.0.1:8088`.
- **Refresh interval** — defaults to 15 seconds and is bounded to 5–300.
- **Router config** — optional path passed to `wayfinder-router service install`.
- **Repository root** — optional local Git root managed by the project-profile
  panel. Wayfinder never edits the repository.

Only a loopback HTTP endpoint can be installed or restarted by the widget.
Remote HTTPS Wayfinder gateways are observed but never managed.

Point compatible applications at the endpoint shown in the panel:

```sh
export OPENAI_BASE_URL=http://127.0.0.1:8088/v1
```

Provider keys remain in the environment or the existing reviewed credential
boundary referenced by the Wayfinder configuration.

## Project profiles

Choose a **Repository root** in the plugin settings to inspect one local
project. When setup is required, the panel asks for a project token and invokes
`wayfinder-router project setup --prompt-token --json`. The token remains only
in the password field until submission; it is then held only until the child
process starts, written once over stdin, and cleared. It never enters process
arguments, output, Router state, the repository, or the main Router TOML. Keep
the original token in your own credential manager and supply that same
capability when launching a supported coding agent.

The Router canonicalizes the Git root, resolves its GitHub origin, and creates
only an ownership-marked profile under
`${XDG_CONFIG_HOME:-$HOME/.config}/wayfinder/projects`. The supervised service
hot reloads that directory through its existing last-known-good path. The panel
shows the canonical repository, whether the transparent routing scaffold was
edited, and whether an explicitly loaded token matches.

**Roll back** uses a two-click confirmation and delegates deletion to the
Router. The Router removes only the exact ownership-verified project directory;
it does not touch the repository or unrelated project profiles.

These controls are capability-gated through
`wayfinder-router capabilities --json`. The pinned
`router-v1.0.0` release exposes authenticated local project profiles; when
that capability is absent or a different Router is on `PATH`, the panel fails
closed instead of editing QML or project state.

### Project value

When the selected Router exposes `wf-project-value-v1`, an active project
profile gains a 30-day value card. It renders Router-owned facts rather than
recalculating them in QML:

- durable successful-request accounting, with accounted and estimated-token
  denominators;
- actual on-device, local-network, hosted, and unknown delivery boundaries
  from the selected project's bounded recent receipts;
- retained terminal successes, failures, cancellations, cache hits, and the
  resulting delivery failure rate;
- the exact real-or-relative price unit, current price-table fingerprint, and
  current `dearest-configured-rate` counterfactual; historical totals retain
  their recorded baseline amounts but not every prior table fingerprint; and
- explicit evidence gaps. User corrections are not collected by this schema,
  so correction rate remains unavailable rather than appearing as zero.

The accounting and delivery windows remain separate: costs use the durable
daily ledger, while delivery health uses the shared 200-entry process-local
ring. Requests recorded before workspace attribution are not guessed into a
project. The card also filters displayed receipts to the verified workspace;
global Router savings remain separately labelled **All savings**.

Older Router builds fail closed with “project value report unavailable.” The
plugin does not create a parallel store or derive a fallback score. The
checksum-pinned bootstrap remains on its reviewed release until a later Router
release containing this schema has its version and both architecture digests
reviewed.

## Verified coding agents

Wayfinder currently release-gates these client contracts against the same
candidate Router build:

| Agent | Client contract | Wayfinder endpoint | Model selection | Real smoke evidence |
| --- | --- | --- | --- | --- |
| Codex | 0.149.0 | `/v1/responses` | `model = "auto"` in reviewed TOML | Streaming `exec_command` call and returned tool output |
| Claude Code | 2.1.241 | `/v1/messages` | `ANTHROPIC_MODEL=auto` | Streaming `Bash` call and returned tool output |
| OpenCode | 1.18.21 | `/v1/chat/completions` | `wayfinder/auto` in reviewed JSON | Streaming `bash` round-trip, upstream error, and disconnect cancellation |
| Pi | 0.84.3 | `/v1/chat/completions` | `wayfinder/auto` in reviewed JSON | Streaming `bash` round-trip, structured upstream error, and disconnect cancellation |
| Aider | 0.86.1 | `/v1/chat/completions` | `openai/auto` with reviewed shell variables | Streaming one-file edit applied through the Router |

Print the bounded connection recipe for any supported client with the Router CLI:

```sh
wayfinder-router connect codex
wayfinder-router connect claude-code
wayfinder-router connect opencode
wayfinder-router connect pi
wayfinder-router connect aider
```

The Codex recipe is a manual addition to `~/.codex/config.toml`; remove its
`wayfinder` provider and model selection to reverse it. The Claude Code recipe
sets shell environment variables only; reverse it without changing a file:

```sh
unset ANTHROPIC_BASE_URL ANTHROPIC_AUTH_TOKEN ANTHROPIC_MODEL
unset CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY
```

The OpenCode recipe is a manual merge of the `provider.wayfinder` object into
the project or user `opencode.json`. Remove that provider object and any
`wayfinder/auto` model selection to reverse it. Wayfinder never reads or
imports OpenCode's provider credentials.

The Pi recipe is a manual merge of the `providers.wayfinder` object into
`~/.pi/agent/models.json`. Remove that object and any saved `wayfinder/auto`
selection to reverse it. Wayfinder never reads or imports Pi's account or
provider credentials.

The Aider recipe exports `OPENAI_API_BASE` and the loopback placeholder
`OPENAI_API_KEY`, then selects `openai/auto`. Unset both variables and stop
selecting that model to reverse it. Wayfinder never reads or imports Aider's
provider credentials.

The printed `wayfinder-local` token is only a loopback placeholder and is not a
provider credential.

All five release-gate smokes use Server-Sent Events and require one real client
action through the Router. Codex, Claude Code, OpenCode, and Pi prove a tool
round-trip; Aider proves an actual file edit. The OpenCode and Pi contracts additionally
prove that an upstream error reaches the real client and terminating that
client closes the provider stream through the Router. The harness terminates a
stuck client at a bounded timeout and prints client, Router, and provider
diagnostics on failure. Router unit and integration contracts continue to cover
the wider disconnect-cancellation and Anthropic/OpenAI error-envelope matrix.

## Controls

- Left click: open or close the status panel.
- Right click: refresh status.
- Middle click: copy the endpoint.
- **Set up Wayfinder**: install the checksum-pinned Router when missing, create
  and validate a no-clobber local policy, then install the systemd user service.
- **Check policy / Install / Start / Restart service**: resume from the next
  incomplete or failed step without repeating successful work.
- **Set up project**: pass a one-time project token over stdin and create the
  owned repository profile without changing the repository or main policy.
- **Roll back**: confirm twice, then remove only the selected Router-owned
  project state.
- **Project value**: inspect prompt-free workspace accounting, actual delivery
  boundaries, failure evidence, baseline, and known evidence gaps.

When operator endpoints are protected by OIDC, the plugin continues to show
health but hides unavailable model, recent-route, savings, and project-value metadata. It does
not ask for or persist an operator token.

Each recent route shows the destination that actually served it, its execution
boundary, resolved policy profile, routing mode and score, delivery outcome,
and observed HTTP status. A failed receipt includes direct remediation based on
the Router's stable error type. Selected-versus-served failover is called out
explicitly; the plugin does not infer provider locality or duplicate routing
policy. These prompt-free receipts are the Router's bounded in-memory state,
not a durable audit log.

## Remove

```sh
omarchy plugin remove io.github.asdecided.wayfinder
```

Removal intentionally leaves the Wayfinder gateway running because other
applications may still use it. Remove that independently only when desired:

```sh
wayfinder-router service uninstall
```

Owned project profiles are also left in place. Select each repository in the
plugin and use **Roll back**, or run `wayfinder-router project rollback --json`
from that repository, before removal when those profiles are no longer wanted.

The plugin also leaves the Router executable in place by default. To remove a
binary that this plugin installed, use:

```sh
./uninstall.sh --remove-owned-router
```

Removal succeeds only when the recorded path is inside the current user's home
directory and the executable still matches its installation digest. A missing,
modified, symlinked, or independently installed Router is never deleted. An
explicit owned-Router removal also removes its plugin-owned last-known-good
backup and transaction metadata.

## Validate

```sh
node scripts/validate.mjs
node test/model.test.mjs
bash test/install.test.sh
bash test/router-lifecycle.test.sh
bash test/record-router-release-cycle.test.sh
bash test/codex-smoke.sh        # Codex 0.149.0 + candidate Router
bash test/claude-code-smoke.sh  # Claude Code 2.1.241 + candidate Router
bash test/opencode-smoke.sh     # OpenCode 1.18.21 + candidate Router
bash test/pi-smoke.sh           # Pi 0.84.3 + candidate Router
bash test/aider-smoke.sh        # Aider 0.86.1 + candidate Router
bash -n install.sh uninstall.sh scripts/router-lifecycle.sh
omarchy plugin validate  # when run on Omarchy Quattro
```

The coding-agent smokes are release-gate harnesses, not installer pins. Set
`WAYFINDER_ROUTER_BIN` to a candidate Router build; each starts the same bounded
local provider and runs the real client through Wayfinder. Four contracts
require one read-only shell tool and verify the returned output; Aider applies
one exact edit in an isolated disposable Git repository.
CI pins each client contract version and one reviewed Router commit, then
builds that Router once for all five agents. Codex explicitly disables Responses
server-side web search because a generic Chat Completions backend cannot provide
that hosted capability. The plugin's downloadable Router version and checksums move only in reviewed
coordinated release PRs.

The harness defaults to Codex's `read-only` sandbox. GitHub's hosted runner
cannot create the Bubblewrap loopback interface used by that sandbox, so CI
sets `WAYFINDER_CODEX_SANDBOX=danger-full-access` for the fixed, no-write
`printf` command only. Run the default locally on Omarchy to cover the native
sandbox as well as the Router transport.

Claude Code runs with only its built-in `Bash` tool enabled and allowlisted,
inside a temporary workspace and temporary home/config directories. The smoke
uses the same `ANTHROPIC_BASE_URL`, `ANTHROPIC_AUTH_TOKEN`, and
`ANTHROPIC_MODEL=auto` values printed by `wayfinder-router connect claude-code`.

OpenCode runs in an isolated home, cache, data directory, and workspace with
automatic updates, external skills, default plugins, and LSP downloads
disabled. Its project configuration enables only the exact no-write `printf`
used by the smoke, selects `wayfinder/auto`, and points the custom
`@ai-sdk/openai-compatible` provider at the candidate Router. The harness also
handles OpenCode's auxiliary title request separately so it cannot be mistaken
for the two-request tool round-trip.

Pi runs in JSON mode with an isolated home and workspace. Its custom-provider
configuration uses the documented `openai-completions` API and disables
unsupported `developer` role and `reasoning_effort` fields. The harness asserts
Pi's structured error event because Pi intentionally exits successfully after
reporting an upstream model error, then separately proves disconnect
cancellation reaches the provider.

Aider runs non-interactively with an isolated home, config, cache, and one-file
Git repository. Its official OpenAI-compatible environment contract points to
the Router, model warnings, updates, analytics, repository maps, auto-commits,
and URL detection are disabled, and the harness requires Aider to apply one
exact streamed search/replace edit before it reports success.

## License

Apache-2.0. See `LICENSE`.
