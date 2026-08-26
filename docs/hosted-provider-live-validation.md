# Hosted provider live validation

The preset catalog proves configuration shape, not a current provider/model
contract. Before a provider is called **verified**, run the opt-in live harness
through an already configured loopback Router:

```sh
WAYFINDER_LIVE_PROVIDER_SMOKE=1 \
  tools/hosted-provider-live-smoke.sh \
  --provider groq \
  --model groq
```

`--model` is the configured Wayfinder destination id from
`[gateway.models."..."]`, not the upstream provider model name. If the Router
uses virtual-key authentication, set `WAYFINDER_ROUTER_VIRTUAL_KEY` in the
environment; the harness passes it through a mode-0600 header file and never
places it on the process command line.

The harness sends exactly three fixed, low-output requests:

1. buffered text with usage;
2. streamed text with incremental output, usage, and a terminal `[DONE]`;
3. one forced, empty-argument `wayfinder_smoke` function call.

Every response must identify the requested destination in
`x-wayfinder-router-served-by`. The harness emits only a prompt-free
`wf-provider-live-v1` summary; provider response text, request bodies, and keys
remain in a temporary mode-0700 directory that is removed on exit.

## Safety and evidence boundary

- The gate is disabled unless `WAYFINDER_LIVE_PROVIDER_SMOKE=1` is set.
- The Router URL must be an explicit loopback HTTP origin.
- The three fixed prompts are public compatibility text, but they leave the
  machine and may incur provider charges.
- Use an account budget cap and a current tool-capable model before running it.
- A failure means that exact provider/model combination is unverified; it does
  not change the generic preset or Automatic.
- Deterministic Router fixtures remain the authority for malformed errors,
  fragmentation, response bounds, disconnect cancellation, and retry policy.
  The live harness does not claim that a remote provider observed a disconnect.

Native Anthropic delivery uses the same harness with `--provider anthropic`
after an explicit `provider = "anthropic"` destination is configured.
