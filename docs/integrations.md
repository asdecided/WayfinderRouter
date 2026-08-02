# Integration recipes

Wayfinder's gateway speaks the OpenAI API, so almost anything that takes a custom
`base_url` works with **one line of config** — no SDK, no plugin. Two rules cover
nearly every tool:

1. **Point `base_url` at the gateway** — `http://localhost:8088/v1` (or your host).
   The gateway is path-tolerant, so the bare host (`http://localhost:8088`, no `/v1`)
   also works.
2. **Use `auto` as the model** — that's the routing directive that means "let Wayfinder
   decide." (You can also pin `prefer-local` / `prefer-hosted`, or a configured endpoint
   name, per request.) The API key can be any non-empty string unless you've put auth in
   front of the gateway; Wayfinder injects the real upstream key from its own config.

The most portable setup is the canonical env pair, which a large fraction of frameworks
and CLIs read automatically:

```bash
export OPENAI_BASE_URL="http://localhost:8088/v1"
export OPENAI_API_KEY="unused"   # any non-empty value; the real key lives in the gateway
```

---

## Chat UIs

**Open WebUI** — Admin Settings → Connections → OpenAI → Add Connection, or via env:

```bash
OPENAI_API_BASE_URL="http://localhost:8088/v1"
OPENAI_API_KEY="unused"
```

**LibreChat** — add a custom endpoint in `librechat.yaml`:

```yaml
endpoints:
  custom:
    - name: "Wayfinder"
      baseURL: "http://localhost:8088/v1"
      apiKey: "unused"
      models:
        default: ["auto"]
        fetch: true   # auto-discover via /v1/models
```

**Jan** — Settings → Model Providers → "+", set Base URL `http://localhost:8088/v1`, API
key `unused`, format **OpenAI**, model `auto`.

**AnythingLLM** — LLM provider "Generic OpenAI": Base URL `http://localhost:8088/v1`,
API key `unused`, model `auto`.

---

## Editors / IDE assistants

**Continue.dev** — `config.yaml`:

```yaml
models:
  - name: Wayfinder
    provider: openai
    model: auto
    apiBase: http://localhost:8088/v1
    apiKey: unused
```

**Cline** — provider **OpenAI Compatible**: Base URL `http://localhost:8088/v1` (note: the
`/v1`, not the full `/chat/completions` path), API key `unused`, Model ID `auto`.

**Zed** — `settings.json`:

```json
{ "language_models": { "openai_compatible": { "Wayfinder": {
  "api_url": "http://localhost:8088/v1"
} } } }
```

**JetBrains AI Assistant** — Settings → Tools → AI Assistant → Providers & API keys → set a
custom OpenAI-compatible Base URL `http://localhost:8088/v1` and Test Connection.

> **Cursor / VS Code Copilot caveat.** Both honor a custom OpenAI base URL only for their
> **chat/plan** panels — autocomplete, inline edit, Composer, and "apply" stay on the
> vendor's own backend and cannot be routed through Wayfinder. Use them for chat; don't
> expect inline-completion traffic to flow through the gateway.

---

## Agent frameworks

**OpenAI SDK (Python / JS)** — set `base_url` / `baseURL`, or just the env pair above:

```python
client = openai.OpenAI(base_url="http://localhost:8088/v1", api_key="unused")
```

**LangChain** — `ChatOpenAI(model="auto", base_url="http://localhost:8088/v1", api_key="unused")`
(or the `OPENAI_BASE_URL` env var).

**LlamaIndex** — use `OpenAILike` (the base `OpenAI` class is pinned to GPT model names):

```python
from llama_index.llms.openai_like import OpenAILike
llm = OpenAILike(model="auto", api_base="http://localhost:8088/v1", api_key="unused")
```

**CrewAI** — `LLM(model="openai/auto", base_url="http://localhost:8088/v1", api_key="unused")`
(the `openai/` prefix is required).

**AutoGen** — custom endpoints need an explicit capability dict:

```python
OpenAIChatCompletionClient(
    model="auto", base_url="http://localhost:8088/v1", api_key="unused",
    model_info={"function_calling": True, "vision": False, "json_output": True, "family": "unknown"},
)
```

**OpenAI Agents SDK (Python)** — `set_default_openai_client(AsyncOpenAI(base_url="http://localhost:8088/v1", api_key="unused"))`, or the `OPENAI_BASE_URL` env var.

**Vercel AI SDK** — `createOpenAICompatible({ name: "wayfinder", baseURL: "http://localhost:8088/v1", apiKey: "unused" })` from `@ai-sdk/openai-compatible`.

---

## Terminal / CLI agents

**aider**:

```bash
export OPENAI_API_BASE="http://localhost:8088/v1"
export OPENAI_API_KEY="unused"
aider --model openai/auto
```

**GitHub Copilot CLI** (BYOK):

```bash
export COPILOT_PROVIDER_BASE_URL="http://localhost:8088/v1"
export COPILOT_PROVIDER_API_KEY="unused"
export COPILOT_MODEL="auto"
```

**Claude Code** — Claude Code speaks Anthropic's Messages API, so it can't use the OpenAI
`base_url` rule above. Instead the gateway exposes a first-class `POST /v1/messages` adapter
(WF-DESIGN-0011) that translates Anthropic ⇄ OpenAI in both directions, including streaming
and tool use. Point `ANTHROPIC_BASE_URL` at the gateway *root* (no `/v1` suffix — the client
appends `/v1/messages`):

```bash
export ANTHROPIC_BASE_URL="http://localhost:8088"
export ANTHROPIC_API_KEY="unused"   # the gateway uses each upstream's own configured key
claude
```

Wayfinder scores each turn and routes it to the configured tier; the inbound Claude model id
is ignored in favour of the routing decision (send a configured endpoint name to pin). The
same `x-wayfinder-router-*` decision headers and budget/failover behaviour apply as on the
OpenAI endpoint — it is the one router (WF-ADR-0001). Image/vision blocks and extended
thinking are not translated yet (WF-DESIGN-0011).

---

## Notes

- **Streaming** works end to end: send `stream: true` and the gateway relays Server-Sent
  Events as they arrive.
- **Responses API** clients may use `POST /v1/responses` (or `/responses`) with the bounded
  `model`, `input`, `instructions`, `stream`, `max_output_tokens`, and `temperature` contract.
  Unsupported fields fail specifically instead of being dropped; see
  [WF-ADR-0066](../decisions/WF-ADR-0066-bounded-responses-api.md).
- **Non-text surfaces** are currently fail-closed. Embeddings, image, audio, and batch payloads
  are not advertised or forwarded until their provider adapter, request/response bounds, usage
  accounting, and parity fixtures are enabled; known modality fields sent to the text endpoint
  return `wayfinder_router_unsupported_modality` before provider delivery. See
  [WF-ADR-0067](../decisions/WF-ADR-0067-bounded-modality-contracts.md).
- **Tool calling** depends on the *upstream* model you route to, while vision/audio/image
  surfaces remain disabled until their reviewed adapters are enabled; known non-text payloads
  are rejected by the modality gate rather than forwarded unchanged.
- **Per-request overrides** travel as headers (e.g. `X-Wayfinder-Threshold`), so you can
  tune routing without changing client config.

See WF-DESIGN-0009 (integration recipes & OpenAI-compatibility) and WF-ROADMAP-0006 for the
roadmap this is part of.
