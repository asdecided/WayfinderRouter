# FAQ

> Runtime and installation answers written for the retired package are
> historical. Current Wayfinder uses the native Rust gateway embedded in the
> Swift desktop app; see the repository README and WF-ADR-0046.

Straight answers, including where Wayfinder loses. The benchmark docs back every claim here:
[`results.md`](../benchmarks/results.md) (illustrative set), [`blind-eval.md`](../benchmarks/blind-eval.md)
(double-blind), [`routerbench-results.md`](../benchmarks/routerbench-results.md) and
[`calibration-eval.md`](../benchmarks/calibration-eval.md) (real graded labels).

## Does Wayfinder call a model to make the routing decision?

No. The decision is a deterministic, offline scan of the prompt's structure (length, headings, lists,
code, tables), turned into a 0–1 score and compared to a threshold. No model, key, or network is in the
scored path ([WF-ADR-0001](../decisions/WF-ADR-0001-standalone-deterministic-router.md)). The model only
gets called *after* routing, by whichever endpoint was chosen.

## Isn't this just a length threshold?

Close, honestly. On RouterBench a tuned word-count threshold roughly ties the full structural score —
both land near chance on that mix. Length ships as one of the scored features *and* as an explicit
baseline in the harness (next to stable-random and a perfect-oracle ceiling), so you can see this for
yourself. Where structure adds anything over raw length is formatted, multi-part prompts — lists, code
blocks, tables, headings — that are heavy without being long. Either way it's a proxy for *heaviness*,
not meaning. See [`routerbench-results.md`](../benchmarks/routerbench-results.md).

## Your own benchmark says it's no better than random on RouterBench. Why use it?

True, and it's in the repo on purpose: at the historical benchmark knee, structural routing lands a few points
below random at *ranking which prompts need the big model* on RouterBench — whose hardest items are
short multiple-choice questions with no structural tell. Structure isn't difficulty.

What Wayfinder gives you regardless is the combination nothing else does at once: a routing decision
that is deterministic, offline, sub-millisecond, explainable, and *yours* — calibrated on your own
labels, with no key or network in the path. It's a useful first-pass cost filter **when your hard
prompts really are the long/complex ones** (a lot of agentic and document traffic looks like that), and
the harness lets you check whether your traffic fits before you trust it. If your hard prompts are short
and semantic, you want a semantic router — and the model call that comes with it. See
[`calibration-eval.md`](../benchmarks/calibration-eval.md).

## Why not just always use the cheap model, or always the big one?

Those are the two baselines the harness reports (always-local, always-cloud), and for many workloads one
of them is the right answer. Routing only matters in the middle: some traffic is fine on the cheap model,
some genuinely needs the big one, and you can separate them for less than the call you're saving. The
harness also prints a perfect-oracle ceiling, so you can see the actual money on the table for your data
before adopting anything. If your traffic has no middle, pin one model — the benchmark will show you that.

## What about the lexical "difficulty" keywords (prove, derive, theorem, ∑)?

They ship **off by default**, and that decision is the project's cautionary tale. The lexicon looked
like a clear win on the author's own prompts, but under a cross-provider double-blind test it collapsed:
it fired on only ~20% of independently-written hard prompts, false-positived on easy ones that used a
trigger word, and lost to a plain word-count baseline. It was detecting an author's *vocabulary*, not
difficulty ([WF-ADR-0016](../decisions/WF-ADR-0016-lexical-difficulty-signals.md),
[`blind-eval.md`](../benchmarks/blind-eval.md)). They're opt-in: enable and calibrate them only when your
own traffic's difficulty lives in its vocabulary. See [lexical-routing.md](lexical-routing.md).

## How fast is the decision?

It's a pure-Python text scan on one core, so the number is machine- and prompt-length dependent — run
`python -m benchmarks.run` to measure it on your hardware. Roughly tens of microseconds on short/medium
prompts, up to ~200µs on very long ones, always sub-millisecond. Latency grows with prompt length (it
scans the text), so cap or sample if you have pathological inputs. The point isn't a throughput record:
the decision is negligible next to the inference it gates, it's CPU-only, and there's no network in the
path — so it never becomes the bottleneck, a dependency, or an outage surface.

## Do my prompts or keys leave the machine?

No. The scorer is offline and never opens a socket. Your keys live only in the gateway's model config,
referenced by environment-variable *name* (`api_key_env`) — never written into the routing config and
never touched by the scorer. The gateway is the only component that holds a key or makes a network call;
the decision layer structurally cannot. It's self-hosted and Apache-2.0, and the scored path is small
enough to audit.

## What models or providers can I route between?

Any two OpenAI-compatible endpoints. The gateway maps the two routed tiers (`local` / `cloud`) to
upstreams in `[gateway.models.*]`, forwards an OpenAI-style request, and sends the key as
`Authorization: Bearer $<api_key_env>` — so it works with local servers (Ollama, vLLM, LM Studio…)
and hosted APIs that implement OpenAI Chat Completions (including OpenAI and Gemini's documented
compatibility endpoint) alike. Anthropic's native Messages API is not an OpenAI-compatible endpoint;
use Wayfinder's incoming Anthropic aliases with a configured OpenAI-compatible upstream. Keys are set as environment
variables named by `api_key_env` — never written into the config. The two tiers don't even have to be
two *different* models: pointing both at the same model with reasoning on for the dear tier and off for
the cheap one (a **think / no-think** split) is a valid two-tier setup — the router just decides which
mode a turn is worth. Copy-paste examples (Ollama+OpenAI and two-tier Anthropic) are in
[`examples/wayfinder-router.lexical.toml`](../examples/wayfinder-router.lexical.toml).

## Can I route between models that behave very differently?

Cautiously, and it's on you to pick substitutable tiers — Wayfinder routes a prompt to a *tier*, it
doesn't reconcile what the models do once they're there. Two models with different context windows,
tool-calling conventions, or formatting habits won't behave identically just because a request was routed
cleanly, and a harness tuned to compensate for one model's quirks can fight a mid-stream switch to
another. So the decision is most defensible when the tiers are close substitutes for your traffic: a
small and a large model in the *same* family (Haiku↔Sonnet, gpt-4o-mini↔gpt-4o), or a local and a hosted
model of similar shape — not two models you'd hand-prompt differently.

The gateway guards the two divergences it can see structurally: it skips a target whose `context_window`
can't fit the prompt ([WF-ADR-0031](../decisions/WF-ADR-0031-gateway-failover-policy.md)), and the
conversation latch (`[gateway] sticky`, [WF-ADR-0022](../decisions/WF-ADR-0022-conversation-latch.md))
stops a thread from ping-ponging once a turn has needed the stronger model. What it can't do is normalize
tool-use semantics or response shapes across models — for tool-heavy agent loops that's exactly why you'd
pin one model (see *Should I route inside an agentic coding harness?* below).

## Why not an LLM-as-judge router instead?

An LLM-as-judge will likely *rank* difficulty better than a structural scan — for fuzzy tasks it almost
certainly does. The trade-off is that it's a model call per request to decide: latency, cost, a new
dependency, and its own failure modes (judge bias, reward-hacking). For routing whose whole purpose is
saving money, paying a call to decide can eat the savings on exactly the cheap requests you're protecting.
A judge shines as a *training* signal (e.g. OpenPipe's RULER) or when you're happy to pay to decide;
Wayfinder is the deterministic, free, offline option for the serving path.

## Can it route the same prompt to several models, compare the answers, and learn which one each needs?

Yes — but offline, at calibration time, never on the live request. Calling several models to compare
them *is* a model call, which by [WF-ADR-0001](../decisions/WF-ADR-0001-standalone-deterministic-router.md)
can't enter the routing decision. So Wayfinder runs that compare-and-grade loop *once, out of band*:
`wayfinder-router judge` ([WF-ADR-0037](../decisions/WF-ADR-0037-automated-sufficiency-judge.md)) sends a
sample of your prompts through both tiers, has an automated **sufficiency judge** decide whether the cheap
tier's answer was good enough, and mints `{text, label}` rows behind trust gates. `calibrate` then bakes
those labels into the deterministic cut. The heuristic — *which prompts actually need the big model* — is
learned from real outcomes, then frozen into a free, offline decision; the comparison happens once, not on
every request.

Contrast running a *cheap classifier model* to pick the tier per turn: that still pays a model call on the
serving path, on exactly the cheap requests you're trying to protect. Wayfinder pays that cost once at
calibration and scores the live turn structurally for free — and it scores only the **current turn**, not
the whole transcript ([`route_on`](../decisions/WF-ADR-0021-multi-turn-routing-scope.md)), so the router
never has to ingest the full context to decide (which would be the expensive part).

## Can I make the routing decision without running the gateway?

Yes — the decision is decoupled from the proxy by design, and that separation is the whole of
[WF-ADR-0001](../decisions/WF-ADR-0001-standalone-deterministic-router.md). The entire decision is a pure
function:

```python
from wayfinder_router import score_complexity, RoutingConfig

result = score_complexity(prompt, config=RoutingConfig.binary(threshold=0.7))
# result.score -> 0–1 float; result.recommendation -> the chosen model. No proxy, no key, no socket.
```

You dispatch however you like. The HTTP gateway is just one consumer of that function — you can call the
router in-process and route yourself, vendor it as a library, or put your own transport (a queue, a gRPC
service, an SSH-tunnelled call) in front of it. The OpenAI-compatible proxy is a convenience for HTTP
traffic, not a requirement for routing.

## If a conversation routes to different models on different turns, how is the context kept?

Your client keeps it, not Wayfinder. The OpenAI chat API is stateless: your app sends the full message
history on every turn, and Wayfinder forwards that whole `messages` array to whichever model it routes
to. So when one turn goes local and the next goes cloud, the second model still receives the entire
conversation so far — including the first model's replies. There is nothing to hand off between models;
the transcript travels with each request, so ordinary multi-turn chats work, not just one-off prompts.

Two things keep that predictable: by default Wayfinder *scores* only the current turn (so the score
doesn't drift toward cloud as the transcript grows, [WF-ADR-0021](../decisions/WF-ADR-0021-multi-turn-routing-scope.md))
but always *sends* the full history to the chosen model; and if you'd rather a thread not switch models
at all, the conversation latch (`[gateway] sticky`, [WF-ADR-0022](../decisions/WF-ADR-0022-conversation-latch.md))
keeps it on the strongest model any turn has needed.

One caveat on savings: because the whole transcript travels to whichever model serves a turn, routing
saves the most on **short or independent requests** (and varied streams where difficulty differs from
one request to the next). On a *long single-model conversation*, switching to the dear tier mid-chat
sends the entire history there, so the per-turn savings shrink as the transcript grows — for those,
pin one model (or use `sticky`) rather than routing every turn.

A side effect: because a growing conversation is a different request every turn, it won't hit the
exact-match response cache — and that's by design. The cache (WF-ADR-0033) is for byte-identical
*repeats* (eval/CI runs, agent tools re-asking the same thing), not evolving chats; multi-turn
correctness comes from forwarding the full context, not from caching.

## Should I route inside an agentic coding harness (Claude Code, Codex)?

Usually not — pin one model there. Agentic, tool-heavy harnesses are tuned to a *specific* model's
tool-calling and quietly compensate for its quirks (response shapes, tool-call truncation, context
handling). Swapping models between turns of one session can fight those compensations and make the
harness flakier, so for a single agent run, pin a model (`model="cloud"` or a configured endpoint) or
turn on the `sticky` latch ([WF-ADR-0022](../decisions/WF-ADR-0022-conversation-latch.md)) so the whole
session stays on one model. Routing across the turns of a single tool-using loop is exactly where a
high-level proxy can get in the way.

Where Wayfinder fits instead: **per-request routing of heterogeneous traffic** — chat, summarize,
classify, extract, and other requests that are independent or tolerate a per-request model choice — and
**quota-stretching** (send a share of the easy requests to a cheaper model). Reach for it on a stream
of varied, mostly-independent requests; pin a model for one long tool-using agent session. (See also
the structural-vs-semantic limit above: a short-but-hard prompt has no structural tell.)

## If a task routes to different models partway through, does the new model lose what the first one was doing?

Partly — and it's an honest cost of switching mid-task, separate from the cost question. The *transcript*
always travels (every model sees the full `messages` history, as above), but a different model
re-deriving from that text doesn't inherit the first model's *latent* state: the half-formed plan, the
reason it chose one approach over another, the parts of a codebase it had paged in. So a switch mid-task
can re-open a settled decision or quietly regress, and it bites hardest in long agentic edit loops —
especially when you've changed files *outside* the AI flow and the next model re-reads them, doesn't see
why, and "corrects" your edit back. The transcript carries *what was said*, not *what the model was
thinking*.

This is the same reason the agentic-harness answer above says pin one model (or use `sticky`): routing's
sweet spot is independent or loosely-coupled requests, where each turn stands on its own — not a single
evolving task where continuity of the *model's own* reasoning is what you're paying for. If your workload
is one long task, pin a model; if it's a stream of varied requests, route. And if you do route a thread,
the conversation latch ([WF-ADR-0022](../decisions/WF-ADR-0022-conversation-latch.md)) at least keeps it
from ping-ponging once a turn has needed the stronger model.

## Can I use a cheap model for the mechanical phases and escalate on failure?

Yes, and it's a good pattern — but you orchestrate it; the scorer doesn't watch task outcomes. Pin the
mechanical, structurally-easy steps (formatting, linting, a test run, a boilerplate edit) to the cheap
tier with `model="local"` (or `prefer-local`), and when a step's *result* is bad — tests fail, the patch
won't apply — retry that request pinned to `model="cloud"`. The per-request `model` directive plus the
`x-wayfinder-router-*` response headers make that a one-line decision in your own loop.

Don't confuse this with the built-in **failover**: `failover = escalate`
([WF-ADR-0031](../decisions/WF-ADR-0031-gateway-failover-policy.md)) reacts to *transport* failures — a
timeout, a `5xx`, a `429` — not to a low-quality-but-successful answer. Quality- or phase-driven
escalation lives in your harness, which is the only thing that knows whether the cheap model's output was
actually good enough. And the cost math favors trying cheap first: because the whole transcript travels
to whichever tier serves a turn, sending the *full* context to the cheap model is often still cheaper than
sending even a little to the dear one — which is why the default leans local and escalates only when
difficulty (or your own retry) calls for it.

## Does it handle streaming, chat, and multi-turn?

The routing decision is made once, up front, then the request is proxied to the chosen model — so
streaming passes straight through (it's the upstream's stream; routing doesn't buffer it). For multi-turn
chat the gateway scores the **current turn** by default — the system prompt plus the latest user message,
not the whole transcript — so the score doesn't drift toward cloud as a conversation grows, and the
model's own (often long) replies are never fed back into the decision
([WF-ADR-0021](../decisions/WF-ADR-0021-multi-turn-routing-scope.md)). The scope is a `[gateway] route_on`
knob (`turn` (default) / `last_user` / `user` / `all`) if you'd rather score every user turn or the entire
payload. What a structural scan still can't do on its own is notice that a *short* follow-up
("now prove that's lossless") is semantically hard — it reads structure, not meaning. For an ongoing hard
chat, turn on the **conversation latch** (`[gateway] sticky`, or the `X-Wayfinder-Sticky` header): it
routes by the hardest turn the conversation has seen — a max over turns, so it doesn't drift with length —
so once any turn crosses over, the thread stays on the capable model
([WF-ADR-0022](../decisions/WF-ADR-0022-conversation-latch.md)). Set `sticky_cooldown` (or
`X-Wayfinder-Sticky-Cooldown`) to let the latch decay back to local after N calm turns, so a chat
that goes hard then quiet drifts back to the cheap model. The latch can't help the *cold-start*
case (a first message that's short but hard), where the deterministic answer is the opt-in **lexical
signals** — raise the lexical feature weights so difficulty *vocabulary* (`prove`, `theorem`, `∑`)
scores, with the caveats in [lexical-routing.md](lexical-routing.md) — or an explicit pin (the `model`
field `auto` / `prefer-local` / `prefer-hosted`, or `X-Wayfinder-Threshold`; see the README,
["Steer a single request"](../README.md#steer-a-single-request)). The demo's **Advanced** settings let
you turn the lexical signals on, tune feature weights, edit the trigger words live, and export the
result as config ([WF-ADR-0023](../decisions/WF-ADR-0023-in-demo-scoring-overrides.md)).

## Is it production-ready? Who maintains it? What are the dependencies?

It's early and largely a solo project, built to be boring on purpose. The pure Rust routing core has no
network, process, filesystem, async-runtime, platform-framework, or secret dependencies. It is
deterministic, fully tested, and Apache-2.0. The gateway is a separate bounded layer, and release builds
ship as self-contained native binaries.

## How do I tune it to my own traffic?

Label a representative sample of your prompts (`{"text": ..., "label": "local"|"cloud"}`). Then
give the native calibrator the cost of each arm:

```bash
wayfinder-router calibrate your-data.jsonl \
  --mode threshold \
  --objective min-cost \
  --costs local=0.0001,cloud=0.003 \
  --out wayfinder-router.toml
```

The command minimizes expected inference cost plus a penalty when a prompt labelled `cloud` routes
to `local`. `--quality-penalty` sets that penalty and defaults to the high-arm cost. The command writes
TOML to stdout when `--out` is absent, and it refuses to replace an existing output file.

To switch the lexical signals on for your domain, raise their weights and (optionally) supply your own
trigger words — ideally mined from your labels rather than hand-picked. Full walkthrough, including the
honest caveats, in [lexical-routing.md](lexical-routing.md). A ~20-prompt bootstrap is only a smoke test;
trust the cut once you have a few hundred labels, and re-check held-out.
