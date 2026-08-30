# Coding-agent quick starts

Wayfinder can sit between a coding agent and the destinations in
`wayfinder-router.toml`. The agent keeps its normal API shape. Wayfinder makes
the routing decision on the same machine, then resolves credentials only for
the selected destination.

## Start Wayfinder

From a directory that does not already contain `wayfinder-router.toml`:

```sh
wayfinder-router init --preset hybrid
export OPENAI_API_KEY="..."
wayfinder-router doctor
wayfinder-router serve
```

`init` never overwrites an existing policy. Automatic two-arm presets run the
native min-cost calibrator over Wayfinder's bundled independent developer
corpus and record the corpus SHA-256, objective, and measured result in the new
TOML. This is a reproducible starter, not personalized learning: no user prompt
is retained and no model or network is used during calibration.

The `hybrid` example expects Ollama at `http://localhost:11434/v1` and uses
OpenAI as the hosted destination. Edit the generated file if your local runtime
or hosted provider differs. `init` never overwrites an existing file.

Keep the server running while the client uses it. In another terminal, run the
matching command below to print the client configuration.

## No-write launch from Omarchy

Codex, Claude Code, and OpenCode also have a launch-only path that leaves their
configuration and authentication stores untouched:

```sh
wayfinder-router exec codex -- codex
wayfinder-router exec claude-code -- claude
wayfinder-router exec opencode -- opencode
```

Arguments after the program are preserved, so a non-interactive Codex launch
can use `wayfinder-router exec codex -- codex exec ...`. Before replacing the
process, Wayfinder requires a loopback Router with at least one ready
destination, the Wayfinder-owned `auto` model, and the client's required wire
endpoint. A failed check stops visibly and never launches the client against
its direct provider.

This path injects only the loopback endpoint, `auto`, and a non-provider
placeholder token into the child process. It does not read or write a client
file, credential, prompt, or repository path. The versioned contract is
available in `wayfinder-router capabilities --json` under `agent_exec`.

Pi is intentionally absent: its current CLI has no verified launch-time custom
endpoint override. `wayfinder-router exec pi -- pi` fails before launch; use
the reviewable `connect pi` recipe below until Pi can satisfy the same no-write
contract.

## Codex

```sh
wayfinder-router connect codex
```

Review the TOML, then add it to `~/.codex/config.toml`. It defines a Wayfinder
model provider at `http://127.0.0.1:8088/v1` and uses the Responses API. It
selects Wayfinder's reserved `auto` model, which applies the local policy.
The bounded adapter accepts Codex's function, custom, and namespaced tool
contract and restores tool calls to their Responses shape after routing through
an eligible OpenAI-compatible destination. Hosted Responses-only tools,
background jobs, and non-text inputs still fail closed.
The fields follow the current
[Codex configuration reference](https://developers.openai.com/codex/config-reference/).

## Claude Code

```sh
wayfinder-router connect claude-code
```

Review and export the printed variables in the shell that starts Claude
Code. Wayfinder accepts the Anthropic Messages request at its loopback address.
`ANTHROPIC_MODEL=auto` selects Wayfinder's reserved automatic-routing directive.
The discovery variable also lets Claude Code discover configured model names
that use its supported `claude` or `anthropic` prefixes. Wayfinder's routing
directives do not use those prefixes, so the explicit model variable is what
makes `auto` available to Claude Code.
The placeholder local token is not a provider credential; if you configure
Wayfinder virtual keys, replace it with a key minted by `wayfinder-router keys
new`.
These variables follow Claude Code's
[LLM gateway connection contract](https://code.claude.com/docs/en/llm-gateway).

## OpenCode

```sh
wayfinder-router connect opencode
```

Review the JSON and merge its `provider.wayfinder` object into your project or
user `opencode.json`. Choose **Wayfinder Automatic** from `/models`.
The provider object follows OpenCode's
[custom provider contract](https://opencode.ai/docs/providers/).

## Pi

```sh
wayfinder-router connect pi
```

Review the JSON and merge its `providers.wayfinder` object into
`~/.pi/agent/models.json`. Select **Wayfinder Automatic** from `/model`, or run
Pi with `--provider wayfinder --model auto`. The recipe uses Pi's documented
`openai-completions` custom-provider contract and disables the optional
`developer` role and `reasoning_effort` fields so the client sends only the
bounded Chat Completions surface Wayfinder verifies. The `wayfinder-local`
value is a loopback placeholder, not a provider credential; replace it with a
Wayfinder virtual key when the local gateway requires one.

To reverse the connection, remove only the `wayfinder` provider object and any
saved `wayfinder/auto` model selection. Wayfinder does not read Pi's account or
provider authentication files.

## Aider

```sh
wayfinder-router connect aider
```

Review and export the printed variables in the shell that starts Aider, then
run the printed `aider --model openai/auto` command. `OPENAI_API_BASE` uses
Wayfinder's loopback `/v1` endpoint, as required by Aider's documented
OpenAI-compatible API contract; the `openai/` model prefix selects that
contract and `auto` remains Wayfinder's reserved routing directive. The
`wayfinder-local` value is a loopback placeholder, not a provider credential.
Replace it with a Wayfinder virtual key only when the local gateway requires
one.

To reverse the connection, unset `OPENAI_API_BASE` and `OPENAI_API_KEY` in that
shell and stop selecting `openai/auto`. Wayfinder writes no Aider configuration
and does not read Aider's provider credentials or project files.

## Low-level project profiles

Project-aware launch integration is built on authenticated local keys, not a
caller-supplied repository header. The transparent core configuration looks
like this:

```toml
[gateway.profiles.coding]
routing_toml = '''
[routing]
threshold = 0.35
'''

[gateway.workspaces.wayfinder-router]
profile = "coding"
models = ["local", "cloud"]

[gateway.keys.wayfinder-router]
hash = "<SHA-256 printed by keys new>"
workspace = "wayfinder-router"
```

Mint the local capability with:

```sh
wayfinder-router keys new --id wayfinder-router --workspace wayfinder-router
```

Add only the printed hashed TOML entry to the Router configuration. Keep the
one-time plaintext token in the reviewed launch environment for that project
and use it in place of the placeholder client token. An authenticated key with
no profiled workspace continues to use the top-level `[routing]` default.
Profile selection never trusts prompt content, working-directory strings, or a
public HTTP header. The project command owns canonical repository discovery and
no-clobber setup:

```sh
cd /path/to/repository
export WAYFINDER_PROJECT_TOKEN="$(openssl rand -hex 32)"
wayfinder-router project setup --json
wayfinder-router project status --json
```

`setup` accepts either the Git origin it discovers or an explicit
`--repository owner/name` / `https://github.com/owner/name`. GitHub's repository
API supplies the canonical identity. The token is accepted only through
`WAYFINDER_PROJECT_TOKEN` or `--prompt-token`; only its SHA-256 hash is stored.
Generated state lives under
`${XDG_CONFIG_HOME:-$HOME/.config}/wayfinder/projects`, not in the repository or
the user's main Router TOML. The supervised Router watches the owned directory
and reloads it through the last-known-good path. Launch the coding agent from
that repository with the same project token in the client's reviewed
authentication environment.

Inspect the exact owned directory and whether its generated profile has been
edited with `project status`. Remove only that repository's owned state with:

```sh
wayfinder-router project rollback --json
```

Rollback refuses directories without the Wayfinder ownership marker and never
touches files outside the matching project directory.

## Check the result

Send a small request and a difficult request from the client. Then open the
local decision dashboard:

```sh
wayfinder-router open
```

The dashboard and response headers show the selected public model, routing
mode, score, and request identity. They do not expose provider credentials.
After at least 20 scored requests, `wayfinder-router doctor --json` also checks
the prompt-free route distribution. A warning that every request used one arm
is evidence to review or calibrate the policy, not permission to lower a cut
blindly.
