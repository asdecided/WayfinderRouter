# Hosted provider presets

Wayfinder can print an isolated destination fragment for hosted providers that
publish an OpenAI-compatible Chat Completions endpoint. Start with the catalog:

```sh
wayfinder-router provider presets
```

Then supply the exact upstream model ID you intend to use:

```sh
wayfinder-router provider preset groq --model openai/gpt-oss-120b
wayfinder-router provider preset openrouter \
  --model anthropic/claude-sonnet-4.5 \
  --id openrouter-sonnet
```

The command prints TOML; it never edits `wayfinder-router.toml`, reads a key,
or calls the provider. Review and merge the fragment, then set the named key
only in the Router service environment. The generated fragment deliberately
contains no routing tier, price, fallback, or context claim, so connecting a
provider does not silently change Automatic. Add those policy fields yourself
after checking the selected model's current capabilities and pricing.

## Catalog

The preset is a small transport contract: official base URL plus the
conventional environment-variable name. Model availability and feature parity
remain provider- and model-specific.

| Preset | OpenAI-compatible base URL | Key reference | Official contract |
| --- | --- | --- | --- |
| `openai` | `https://api.openai.com/v1` | `OPENAI_API_KEY` | [OpenAI API reference](https://platform.openai.com/docs/api-reference/chat) |
| `gemini` | `https://generativelanguage.googleapis.com/v1beta/openai` | `GEMINI_API_KEY` | [Gemini OpenAI compatibility](https://ai.google.dev/gemini-api/docs/openai) |
| `openrouter` | `https://openrouter.ai/api/v1` | `OPENROUTER_API_KEY` | [OpenRouter quickstart](https://openrouter.ai/docs/quickstart) |
| `groq` | `https://api.groq.com/openai/v1` | `GROQ_API_KEY` | [Groq OpenAI compatibility](https://console.groq.com/docs/openai) |
| `deepseek` | `https://api.deepseek.com` | `DEEPSEEK_API_KEY` | [DeepSeek API quickstart](https://api-docs.deepseek.com/) |
| `together` | `https://api.together.ai/v1` | `TOGETHER_API_KEY` | [Together OpenAI compatibility](https://docs.together.ai/docs/inference/openai-compatibility) |
| `fireworks` | `https://api.fireworks.ai/inference/v1` | `FIREWORKS_API_KEY` | [Fireworks OpenAI compatibility](https://docs.fireworks.ai/tools-sdks/openai-compatibility) |
| `cerebras` | `https://api.cerebras.ai/v1` | `CEREBRAS_API_KEY` | [Cerebras OpenAI compatibility](https://inference-docs.cerebras.ai/resources/openai) |
| `xai` | `https://api.x.ai/v1` | `XAI_API_KEY` | [xAI API overview](https://docs.x.ai/overview) |
| `mistral` | `https://api.mistral.ai/v1` | `MISTRAL_API_KEY` | [Mistral migration guide](https://docs.mistral.ai/resources/migration-guides) |

These presets cover Wayfinder's existing `openai-compatible` delivery kind.
They do not make provider-specific hosted tools or non-Chat-Completions
extensions portable. Unsupported request features continue to fail closed.

## Reversal

Remove only the generated `[gateway.models."..."]` table and unset its key
variable from the Router service environment. If you separately added the
destination to a routing tier, fallback, virtual-key model allowlist, or
project profile, reverse that explicit policy edit as a separate reviewed
change.
