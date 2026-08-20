# Calibration-time lexical distillation — can an offline teacher fix the short-hard blind spot?

> Evidence for [#171](https://github.com/asdecided/WayfinderRouter/issues/171). Answers the
> ruling's question — *can embeddings be distilled, at calibration time, into a static
> artifact the pure scorer evaluates?* — with held-out numbers rather than a proposal.
>
> **Nothing here runs at request time.** The distiller is an offline calibration tool; its
> only output is a `[routing.lexicon]` + `[routing.weights]` config the existing scorer
> already loads. WF-ADR-0001 is untouched: no model call, no network, no credential in the
> scored path.

## The blind spot, measured

The scorer is not blind to semantics — it extracts them and then discards them.

```console
$ echo "Prove the halting problem is undecidable" | wayfinder-router route - --json
  "score": 0.0,
  "recommendation": "local",
  "reasoning_term_count": 3      # <- detected, then multiplied by a 0.0 weight
```

`Weights::default()` is `[3.0, 1.5, 1.0, 2.0, 1.0, 1.5, 1.0, 0.0, 0.0, 0.0, 0.0]`: the four
semantic features (`reasoning_term_count`, `math_symbol_count`, `constraint_term_count`,
`question_count`) are computed every request and weighted to nothing. That is deliberate and
documented (WF-ADR-0016, `docs/lexical-routing.md`), and it is also the whole of the #171
blind spot.

Measured on `blind/openai-cross-provider.jsonl` (154 independently-authored prompts) with
the shipped structural default:

| population | n | mean score | median |
| --- | --: | --: | --: |
| hard / **plain** | 89 | 0.0086 | 0.0082 |
| easy / **structured** | 21 | 0.0185 | 0.0102 |

**78.1%** of (hard-plain, easy-structured) pairs are ranked backwards — an easy structured
prompt outranks a hard plain one. Structure dominates difficulty by better than 2x in mean
score.

Two related observations, both reproducible from this repo:

- The shipped `DEFAULT_THRESHOLD = 0.5` routes **0 / 154** of these prompts to cloud; so does
  the `0.09` cut in `examples/wayfinder-router.lexical.toml`. Structural scores here span
  0.0027–0.0838. Those cuts are RouterBench's, and they do not transfer.
- `docs/lexical-routing.md` still documents `benchmarks/mine_lexicon.py`, which the Rust-only
  cutover removed. The recipe it describes is no longer runnable.

## Method

**Cross-distribution, by construction.** Fit on 60 author-written prompts
(`dataset.jsonl` + `seed/domain-seed.jsonl`); test once on the 154 independently-authored
prompts in `blind/openai-cross-provider.jsonl`. Different authors, zero overlap — the
design `blind-eval.md` argues for.

**Metric.** `skill = PGR − frac_cloud`, per `calibration-eval.md`. Labels are by
construction (easy → `{local:1, cloud:1}`, hard → `{local:0, cloud:1}`), under which PGR
reduces exactly to *the fraction of hard prompts routed to cloud*.

**Thresholds** are selected on train only, never on test.

**Every arm is scored by the real binary** — `wayfinder-router route --json` against a
generated TOML artifact — not by a reimplementation of the scorer.

**The distiller** (`benchmarks/distill_lexicon.py`) embeds the train prompts, takes the
hardness direction `mean(hard) − mean(easy)`, embeds a candidate vocabulary sampled
deterministically from the system dictionary (15,102 words — *independent of both corpora*),
ranks candidates by projection onto that direction, and emits the top-K as
`[routing.lexicon] reasoning_terms`.

## Results

| arm | skill | recall (hard) | false fires (easy) | precision | terms |
| --- | --: | --: | --: | --: | --: |
| structural default | +0.048 | — | — | — | — |
| built-in lexicon + fitted weights | +0.098 | 15/94 = 16% | 0/60 = 0% | 1.00 | 52 |
| log-odds mined lexicon | +0.061 | 53/94 = 56% | 15/60 = 25% | 0.78 | 94 |
| distilled, 640 terms | +0.160 | 40/94 = 43% | 0/60 = 0% | 1.00 | 640 |
| distilled, 1240 terms | +0.180 | 48/94 = 51% | 2/60 = 3% | 0.96 | 1240 |
| **distilled, 1940 terms** | **+0.213** | 57/94 = 61% | 2/60 = 3% | 0.97 | 1940 |
| *oracle wordlist (peeks at test labels)* | *+0.390* | *94/94 = 100%* | *0/60 = 0%* | *1.00* | *646* |

Paired bootstrap, 2000 resamples over the same prompts:
**Δskill = +0.116, 95% CI [+0.068, +0.167]**; the distilled artifact wins in **100.0%** of
resamples. The oracle row is an upper bound, not an achievement — it was fitted on the test
labels to show the mechanism has headroom.

### Why it works — precision, not recall

This is the part that answers the ruling's qualification directly.

The curated lexicon is a **high-precision, low-recall** detector: 0% false fires, 16% recall.
Log-odds mining can only pick words that *occur in the training prompts*, so it buys recall
by spending precision — 56% recall at precision 0.78 against a 0.61 base rate — and net skill
**falls** (+0.098 → +0.061). That reproduces this repo's own documented finding that global
mining captures task-surface words.

An embedding generalises to words that appear nowhere in the 60 training prompts —
`asymptotic`, `axiomatic`, `conjecture`, `convexity`, `derivable`, `factorizing`. Recall rises
16% → 61% while precision stays ≈ 1.0. **That is the specific thing distillation contributes,
and it is a signal that remains visible at runtime** — per-prompt feature counts, not a
re-tuned threshold.

### Against the length baseline

`blind-eval.md`'s central result is that lexical signals lose to counting words. Both cuts
fitted on train, the honest comparison:

| router | skill | PGR | → cloud |
| --- | --: | --: | --: |
| length baseline (`word_count ≥ 13`, cut fitted on train) | +0.048 | 0.46 | 41% |
| **distilled, 1940 terms** | **+0.213** | 0.68 | 47% |
| length baseline (`word_count ≥ 10`, hand-picked in blind-eval) | +0.153 | **0.81** | 66% |

Distilled beats a *methodologically matched* length baseline decisively. Against the
hand-picked `≥ 10` rule it wins on skill (+0.061) but **loses on raw PGR** (−0.13) — because
that rule simply spends more, routing 66% of traffic to cloud against 47%. Read `skill`, not
PGR, when the cloud fractions differ.

### Budget behaviour

| budget | built-in baseline | distilled 1940 |
| --- | --- | --- |
| 25% cloud | PGR 0.35 | PGR 0.61 |
| 35% cloud | *unreachable* | PGR 0.61 |
| 50% cloud | *unreachable* | PGR 0.68 |

The baseline **saturates at 25%**: only a quarter of prompts carry any signal above the cut,
so a larger budget cannot be spent even when it exists. Raising recall makes larger budgets
usable at all.

### Cost

`cost_saved = frac_local × (1 − c_low/c_high)`, at RouterBench's ~72x ratio:

| arm | → cloud | cost saved | PGR |
| --- | --: | --: | --: |
| built-in baseline | 25% | 74% | 0.35 |
| distilled, 1940 | 47% | 53% | 0.68 |

The distilled artifact is **not** strictly cheaper — it spends more to recover more. `skill`
nets that out; an operator with a hard savings floor should pick the cut, not the arm.

## Runtime cost of the artifact

| property | value |
| --- | --- |
| artifact size | 27,110 bytes, 1,940 terms |
| loader ceiling | **2,000 terms** (hard error above it) |
| decision latency, default config | 4.48 ms |
| decision latency, distilled config | 5.35 ms |

Both latencies include process spawn *and* a one-time TOML parse; a long-running service
pays the parse once, not per request. Per-decision scoring stays sub-millisecond, and the
request path is byte-identical — `score_complexity` → `extract_features_with_lexicon`.

## Limits — read before trusting this

- **One corpus.** n = 154, single independent author, one provider. `blind-eval.md` argues
  exactly this point about single-author evidence; it applies to this result too.
- **By-construction labels**, which `blind-eval.md` itself calls "the acknowledged weak
  link". Not real graded labels. RouterBench is not in the repo and was not used.
- **Still climbing at the cap.** Skill rises monotonically 640 → 1240 → 1940 terms
  (+0.160 → +0.180 → +0.213) and hits the loader's 2,000-term ceiling before flattening. The
  measured gain is therefore **bounded by the artifact format, not by the method** — which is
  the most useful thing here for deciding what a distilled artifact should be allowed to be.
- **The fitted weights zero `math_symbol_count`**, contradicting this repo's finding that
  math symbols carry most of the RouterBench lexical win. That is a plausible sign the
  60-prompt author-written training set is too narrow.
- Closing 39% of the distance to the oracle still leaves most of it open.
