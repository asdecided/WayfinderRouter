# Lexical routing — the opt-in recipe that beats the structural default

> The broad legacy calibration surface referenced in this historical guide was
> removed by the Rust-only cutover (WF-ADR-0046). The native CLI now supports
> the bounded `threshold` + `min-cost` path documented below; legacy objectives,
> classifier fitting, and automatic recalibration remain unsupported.

Wayfinder's default scorer is **structural only** (length, headings, lists, code, tables).
On real frontier traffic that default does not beat stable-random: against RouterBench
(`mistralai/mistral-7b-chat` as `local` vs `gpt-4-1106-preview` as `cloud`) its historical
knee recovers a *negative* skill — see [`../benchmarks/routerbench-results.md`](../benchmarks/routerbench-results.md).

The one deterministic, **held-out** improvement is to opt in to the lexical features
(reasoning / math / constraint vocabulary) and cut at the historical benchmark knee. Measured
leakage-free (calibrate on train, score on held-out test;
[`../benchmarks/calibration-eval.md`](../benchmarks/calibration-eval.md)):

| router | skill (PGR − frac_cloud) | cost saved |
| --- | --: | --: |
| structural default (knee) | **−0.038** (loses to random) | 50% |
| **lexical opt-in (knee)** | **+0.057** (beats random) | 61% |

`skill` is the honest metric: a router that sends fraction *f* of prompts to cloud recovers
`PGR = f` by chance, so only `skill = PGR − frac_cloud > 0` means it ranks *which* prompts
need the strong model better than a coin flip.

## The recipe

Copy [`../examples/wayfinder-router.lexical.toml`](../examples/wayfinder-router.lexical.toml)
to `wayfinder-router.toml`:

```toml
[routing]
threshold = 0.09
weights = { reasoning_term_count = 5.0, math_symbol_count = 3.0, constraint_term_count = 1.5 }
```

Only the lexical weights are raised; the loader keeps the structural defaults, so this is
the shipped scorer with the lexicon switched on. Nothing else changes — the decision is
still pure, offline, sub-millisecond, with no model call (WF-ADR-0001).

## When it helps (and when it does not)

Lexical signals detect a **vocabulary**, not difficulty in general. They help only when your
traffic's hardness is *expressed in words the lexicon scans* — proofs, math notation,
multi-constraint instructions. RouterBench is math/reasoning-heavy, which is exactly that
case. On independently-authored prose where hardness is *not* in those words, the lexicon
fires on ~20% of hard prompts and loses to a length baseline
([`../benchmarks/blind-eval.md`](../benchmarks/blind-eval.md)) — which is why it ships **off
by default** (WF-ADR-0016). Turn it on only if your traffic looks like the former.

Two things the evidence is clear about:

- **`0.09` is RouterBench's knee, not a universal constant.** Recalibrate it to your traffic.
- **A ~20-prompt bootstrap is not enough** to find a stable cut: skill is noise-dominated
  until you have a few hundred labeled prompts (see the learning curve in
  `calibration-eval.md`). Treat 20 prompts as a smoke test, not a calibration.

## Calibrate the threshold to your traffic

Label a representative sample of *your* prompts (`{"text": ..., "label": "local"|"cloud"}` —
the model each prompt should have gone to). Then let the native `min-cost`
calibrator place a price-sensitive cut and emit a routing config:

```bash
wayfinder-router calibrate your-data.jsonl --mode threshold --objective min-cost \
  --costs local=0.0001,cloud=0.003 \
  --quality-penalty 0.003 \
  --config tuned-wayfinder-router.toml \
  --out wayfinder-router.toml
```

`--quality-penalty` is the cost of a cheap answer that should have used the high
arm. It defaults to the high-arm cost. Increase it when a wrong answer costs more
than one strong-model retry. `--config` is optional; when present, the calibrator
uses that file's weights and lexicon and preserves them in the generated fragment.

Aim for a few hundred labels before trusting the cut. `recalibrate` remains
unsupported; rerun this explicit command when your traffic or prices change.

## Bring your own lexicon (configurable trigger words)

The trigger words are configuration, not code (WF-ADR-0019). Supply your own under
`[routing.lexicon]` — e.g. the subject-matter-expertise vocabulary your traffic's hard
prompts actually use:

```toml
[routing.lexicon]
reasoning_terms = ["differential", "contraindication", "etiology", "pathophysiology"]
# constraint_terms = [...]   # omit a family to keep its built-in default
```

It stays off until you also weight it (`reasoning_term_count`), and it round-trips through
the config loader like everything else. Math symbols and the `?` count stay built-in (they
aren't vocabulary you curate).

### Mine the words from your own labels

Guessing a wordlist re-introduces author bias. `benchmarks/mine_lexicon.py` instead picks
the terms that, in *your* labeled data, appear far more in cloud-labeled prompts than
local-labeled ones (a deterministic smoothed log-odds on a held-out train split), and emits
a ready `[routing.lexicon]` config:

```bash
python -m benchmarks.mine_lexicon your-data.jsonl
```

Read the output with eyes open — on RouterBench it taught two honest lessons:

- **Global mining captures task-surface words, not difficulty.** The top cloud-signal terms
  came out as `homework, mile, preheat, flour, dough, laundry` — i.e. "this looks like a
  grade-school-math or hellaswag prompt," which overfits *which benchmark* a prompt is from,
  not how hard it is. **Mine per-domain** for sensible expert vocabulary (RouterBench's
  per-domain mine gives science → `hypertension, cardiac`; general → `legislative, voting`).
- **Mined words beat the built-in list but words alone aren't the signal.** Held-out, the
  mined reasoning words scored a touch above the built-in ones (+0.02 skill) yet both sat
  *below* random with reasoning-only weight — because the lexical win in the recipe above
  comes mostly from the **math symbols**, not the word list. So mine to *augment* the
  symbol/structure signal for your domain, and always re-check held-out before trusting it.

### Per-domain starter lists

[`benchmarks/seed/domain-lexicons.toml`](../benchmarks/seed/domain-lexicons.toml) ships the
per-domain term lists mined from RouterBench, one `reasoning_terms` block per domain. Copy
the block for your domain into your config and weight it:

```toml
[routing.lexicon]
# from the [science] block of benchmarks/seed/domain-lexicons.toml
reasoning_terms = ["hypertension", "cardiac", "pyruvate", "membrane", "anterior", "atoms", "orbit"]

[routing]
weights = { reasoning_term_count = 5.0 }
threshold = 0.09   # then recalibrate to your traffic
```

These are *starters*, and honestly uneven: the `science`, `general`, and `humanities` blocks
are real subject-matter vocabulary; `math`, `multilingual`, and `commonsense` skew to
task-surface nouns (RouterBench's tasks there are word-problems / templated). Treat them as a
worked example and regenerate from your own labelled traffic with `--emit-domains`.

### Stock profiles (packaged, selectable in the demo)

For a head-start without hand-copying, the library ships **lexicon profiles**
([`wayfinder_router/profiles.py`](../wayfinder_router/profiles.py), WF-ADR-0024), served at
`GET /router/profiles` and selectable in the demo's **Advanced** settings — pick one and it
fills the term lists, turns the lexical signal on, and you tune + **Export config** from there.
They come in two honestly-labelled flavours:

- **Curated** — hand-authored, defensible vocabulary (Proofs & mathematics, Law & compliance,
  Code & infrastructure, Science & medicine). A sensible start, but *unvalidated*.
- **RouterBench-mined** — the per-domain lists above, each carrying its quality note (the
  `math` / `commonsense` / `multilingual` ones are kept as cautionary examples, not recommendations).

A profile is a starting vocabulary, not a finished router: load it, then **calibrate on your own
labels**. The lexical caveat above (it reads vocabulary, not difficulty) applies to every profile.

## Verify it on your data

Point the benchmark harness at your labeled set to see the held-out skill for the structural
default vs the lexical opt-in before you deploy:

```bash
python -m benchmarks.routerbench_calibrate your-data.jsonl
```

It reports both, plus the random / length / oracle reference lines, all leakage-free.
