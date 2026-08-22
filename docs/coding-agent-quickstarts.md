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

The `hybrid` example expects Ollama at `http://localhost:11434/v1` and uses
OpenAI as the hosted destination. Edit the generated file if your local runtime
or hosted provider differs. `init` never overwrites an existing file.

Keep the server running while the client uses it. In another terminal, run the
matching command below to print the client configuration.

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
The third printed variable lets Claude Code discover Wayfinder's configured
model names. Select `auto` from `/model` after it starts.
The placeholder local token is not a provider credential; if you configure
Wayfinder virtual keys, replace it with a key minted by `wayfinder-router keys
new`.
These variables follow Claude Code's
[LLM gateway connection contract](https://code.claude.com/docs/en/llm-gateway-connect).

## OpenCode

```sh
wayfinder-router connect opencode
```

Review the JSON and merge its `provider.wayfinder` object into your project or
user `opencode.json`. Choose **Wayfinder Automatic** from `/models`.
The provider object follows OpenCode's
[custom provider contract](https://opencode.ai/docs/providers/).

## Check the result

Send a small request and a difficult request from the client. Then open the
local decision dashboard:

```sh
wayfinder-router open
```

The dashboard and response headers show the selected public model, routing
mode, score, and request identity. They do not expose provider credentials.
