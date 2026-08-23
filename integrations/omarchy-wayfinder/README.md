# Wayfinder for Omarchy

Wayfinder is the model router built for Omarchy. One plugin installs the native
Rust Router, gives supported coding agents a shared local endpoint, and exposes
health, delivery receipts, local-versus-hosted distribution, model readiness,
and savings in the Omarchy bar.

The plugin is the flagship product surface; the independently supervised
`wayfinder-router` process remains the only routing authority. Reloading or
disabling the Omarchy shell does not interrupt requests.

Wayfinder is Omarchy-primary, not Omarchy-only. The portable Router and the
governing [Omarchy-first strategy](https://github.com/asdecided/WayfinderRouter/blob/main/decisions/WF-ADR-0073-omarchy-first-portable-core.md)
live in `asdecided/WayfinderRouter`.

## Requirements

- Omarchy Quattro with third-party plugin support.
- An x86_64 or ARM64 glibc-based Linux system.
- `curl`, `tar`, `sha256sum`, `systemd --user`, and `wl-copy` (provided by a
  standard Omarchy installation).

The plugin never stores provider credentials. It reads only the gateway's
prompt-free local status surfaces.

## Install

```sh
omarchy plugin add https://github.com/asdecided/omarchy-wayfinder.git
cd ~/.config/omarchy/plugins/io.github.asdecided.wayfinder
./install.sh
```

When the Router is missing, the installer downloads the native
`router-v2026.8.0` archive for the current architecture and verifies its pinned
SHA-256 digest before extraction. It does not require Rust or Cargo. An existing
`wayfinder-router` executable is never replaced.

The installer rescans plugins and enables the widget. It does not start the
Router service, create a gateway configuration, or add a provider credential.

Open the Wayfinder bar item and choose **Set up Wayfinder**. The explicit,
resumable first-run flow:

1. checks for an existing policy without changing it;
2. creates a no-clobber `local` policy at
   `${XDG_CONFIG_HOME:-$HOME/.config}/wayfinder/wayfinder-router.toml` only
   when no policy exists;
3. validates that policy with `wayfinder-router doctor --json`; and
4. installs and starts `~/.config/systemd/user/wayfinder-router.service`
   through the native Router CLI.

An existing policy is never overwritten. Invalid policy stops setup with a
specific error and can be rechecked after repair. Missing provider environment
is listed by variable name but does not prevent the local service from being
installed. Interrupted setup is safe to resume from the bar.

The Router binary is installed to `~/.local/bin` by default. Set
`WAYFINDER_BIN_DIR` before running the installer when another user-owned binary
directory is required. Router upgrades remain explicit plugin changes: the
release version and both architecture digests are reviewed before these pins
move. The installer records the exact release, target, archive digest, binary
digest, and user-owned path for a binary that it installs.

Omarchy clones third-party plugins disabled so their source can be reviewed
before enablement. The script performs the explicit enable step.

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
`wayfinder-router capabilities --json`. This source change deliberately leaves
the downloadable Router pinned
to `router-v2026.8.0`; that release reports project profiles as unavailable.
The binary and plugin pin move together only in the later coordinated release.

## Verified coding agents

Wayfinder currently release-gates these client contracts against the same
candidate Router build:

| Agent | Client contract | Wayfinder endpoint | Model selection | Real smoke evidence |
| --- | --- | --- | --- | --- |
| Codex | 0.149.0 | `/v1/responses` | `model = "auto"` in reviewed TOML | Streaming `exec_command` call and returned tool output |
| Claude Code | 2.1.241 | `/v1/messages` | `ANTHROPIC_MODEL=auto` | Streaming `Bash` call and returned tool output |

Print the bounded connection recipe for either client with the Router CLI:

```sh
wayfinder-router connect codex
wayfinder-router connect claude-code
```

The Codex recipe is a manual addition to `~/.codex/config.toml`; remove its
`wayfinder` provider and model selection to reverse it. The Claude Code recipe
sets shell environment variables only; reverse it without changing a file:

```sh
unset ANTHROPIC_BASE_URL ANTHROPIC_AUTH_TOKEN ANTHROPIC_MODEL
unset CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY
```

The printed `wayfinder-local` token is only a loopback placeholder and is not a
provider credential.

Both release-gate smokes use Server-Sent Events and require one real client
tool round-trip through the Router. The harness terminates a stuck client at a
bounded timeout and prints client, Router, and provider diagnostics on failure.
Router unit and integration contracts cover disconnect cancellation and
Anthropic/OpenAI error-envelope translation; those are not claimed as separate
real-client smoke scenarios yet.

## Controls

- Left click: open or close the status panel.
- Right click: refresh status.
- Middle click: copy the endpoint.
- **Set up Wayfinder**: create and validate a no-clobber local policy, then
  install the systemd user service.
- **Check policy / Install / Start / Restart service**: resume from the next
  incomplete or failed step without repeating successful work.
- **Set up project**: pass a one-time project token over stdin and create the
  owned repository profile without changing the repository or main policy.
- **Roll back**: confirm twice, then remove only the selected Router-owned
  project state.

When operator endpoints are protected by OIDC, the plugin continues to show
health but hides unavailable model, recent-route, and savings metadata. It does
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
modified, symlinked, or independently installed Router is never deleted.

## Validate

```sh
node scripts/validate.mjs
node test/model.test.mjs
bash test/install.test.sh
bash test/codex-smoke.sh        # Codex 0.149.0 + candidate Router
bash test/claude-code-smoke.sh  # Claude Code 2.1.241 + candidate Router
bash -n install.sh uninstall.sh
omarchy plugin validate  # when run on Omarchy Quattro
```

The coding-agent smokes are release-gate harnesses, not installer pins. Set
`WAYFINDER_ROUTER_BIN` to a candidate Router build; each starts the same bounded
local provider, runs the real client through Wayfinder, requires one read-only
shell tool, and verifies that the tool result returns before the final response.
CI pins both client contract versions and one reviewed Router commit, then
builds that Router once for both agents. Codex explicitly disables Responses
server-side web search because a generic Chat Completions backend cannot provide
that hosted capability. The plugin's downloadable Router version and checksums
move only in the later coordinated release PR.

The harness defaults to Codex's `read-only` sandbox. GitHub's hosted runner
cannot create the Bubblewrap loopback interface used by that sandbox, so CI
sets `WAYFINDER_CODEX_SANDBOX=danger-full-access` for the fixed, no-write
`printf` command only. Run the default locally on Omarchy to cover the native
sandbox as well as the Router transport.

Claude Code runs with only its built-in `Bash` tool enabled and allowlisted,
inside a temporary workspace and temporary home/config directories. The smoke
uses the same `ANTHROPIC_BASE_URL`, `ANTHROPIC_AUTH_TOKEN`, and
`ANTHROPIC_MODEL=auto` values printed by `wayfinder-router connect claude-code`.

## License

Apache-2.0. See `LICENSE`.
