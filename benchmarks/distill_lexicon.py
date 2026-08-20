#!/usr/bin/env python3
"""Distil a semantic teacher into a static Wayfinder lexicon artifact.

CALIBRATION-TIME ONLY. This script is not part of the runtime and is not
invoked by the router, the gateway, or CI. Its sole output is a TOML fragment
(`[routing.lexicon]` + `[routing]` weights) that the existing pure Rust scorer
loads. Request-time routing therefore stays offline, deterministic and keyless
(WF-ADR-0001): the embedding model is consulted here and never again.

Method
------
1. Embed labelled TRAIN prompts. Compute the hardness direction
   `unit(mean(hard) - mean(easy))`.
2. Embed a candidate vocabulary drawn from a general word list -- deliberately
   NOT from the evaluation corpus, so selected terms are not memorised test
   vocabulary.
3. Rank candidates by cosine projection onto the hardness direction and emit
   the top K as `reasoning_terms`.

Step 2 is what a supervised log-odds miner cannot do: it can only select words
that already occur in the training prompts, which buys recall at the cost of
precision. Projecting a general vocabulary reaches semantically adjacent words
that never appear in training.

Determinism: for a fixed embedding model, vocabulary file and K, the emitted
artifact is byte-identical across runs. Ties break on the term string.

Usage
-----
    python3 benchmarks/distill_lexicon.py \
        --train benchmarks/dataset.jsonl \
        --vocab /usr/share/dict/words \
        --top-k 1900 \
        --out examples/wayfinder-router.distilled.toml

Each train row needs a `prompt` and a label: either `difficulty` in
{easy,hard} or `label` in {local,cloud}.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import re
import sys
import urllib.request

DEFAULT_ENDPOINT = os.environ.get("WAYFINDER_EMBED_URL", "http://127.0.0.1:11434/api/embeddings")
DEFAULT_MODEL = os.environ.get("WAYFINDER_EMBED_MODEL", "nomic-embed-text")

# The config loader rejects lexicons above this size; keep the artifact bounded.
MAX_TERMS = 2000
WORD_RE = re.compile(r"\A[a-z]{5,14}\Z")


def embed(text: str, endpoint: str, model: str, retries: int = 3) -> list[float]:
    payload = json.dumps({"model": model, "prompt": text}).encode()
    last: Exception | None = None
    for _ in range(retries):
        try:
            request = urllib.request.Request(
                endpoint, data=payload, headers={"Content-Type": "application/json"}
            )
            with urllib.request.urlopen(request, timeout=30) as response:
                return json.load(response)["embedding"]
        except Exception as error:  # noqa: BLE001 - calibration tool, surface and retry
            last = error
    raise SystemExit(f"embedding failed for {text[:40]!r}: {last}")


def unit(vector: list[float]) -> list[float]:
    norm = math.sqrt(sum(x * x for x in vector)) or 1.0
    return [x / norm for x in vector]


def is_hard(row: dict) -> bool:
    if "difficulty" in row and row["difficulty"] is not None:
        return str(row["difficulty"]).startswith("hard")
    if "label" in row:
        label = row["label"]
        if isinstance(label, dict):
            return label.get("local", 1) == 0
        return str(label) == "cloud"
    raise SystemExit(f"row has no difficulty/label: {row.get('prompt', '')[:40]!r}")


def sample_vocabulary(path: str, limit: int) -> list[str]:
    """Deterministic hash sample, so a rerun selects the same candidates."""
    words = set()
    with open(path, encoding="utf-8", errors="ignore") as handle:
        for line in handle:
            word = line.strip().lower()
            if WORD_RE.match(word):
                words.add(word)
    ordered = sorted(words)
    if limit <= 0 or limit >= len(ordered):
        return ordered
    keep = []
    for word in ordered:
        bucket = int(hashlib.sha256(word.encode()).hexdigest()[:8], 16) % len(ordered)
        if bucket < limit:
            keep.append(word)
    return keep


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--train", required=True, help="labelled JSONL of training prompts")
    parser.add_argument("--vocab", default="/usr/share/dict/words", help="candidate word list")
    parser.add_argument("--vocab-sample", type=int, default=15000, help="candidates to embed")
    parser.add_argument("--top-k", type=int, default=1900, help="terms to emit")
    parser.add_argument("--out", required=True, help="TOML artifact to write")
    parser.add_argument("--endpoint", default=DEFAULT_ENDPOINT)
    parser.add_argument("--model", default=DEFAULT_MODEL)
    parser.add_argument("--threshold", type=float, default=None, help="optional routing cut")
    args = parser.parse_args()

    if args.top_k > MAX_TERMS:
        raise SystemExit(f"--top-k {args.top_k} exceeds the loader ceiling of {MAX_TERMS}")

    rows = [json.loads(line) for line in open(args.train, encoding="utf-8") if line.strip()]
    hard = [unit(embed(r["prompt"], args.endpoint, args.model)) for r in rows if is_hard(r)]
    easy = [unit(embed(r["prompt"], args.endpoint, args.model)) for r in rows if not is_hard(r)]
    if not hard or not easy:
        raise SystemExit("training data needs both hard and easy rows")
    dims = len(hard[0])
    direction = unit([
        sum(v[i] for v in hard) / len(hard) - sum(v[i] for v in easy) / len(easy)
        for i in range(dims)
    ])
    print(f"hardness direction from {len(hard)} hard / {len(easy)} easy prompts", file=sys.stderr)

    candidates = sample_vocabulary(args.vocab, args.vocab_sample)
    print(f"embedding {len(candidates)} candidate terms", file=sys.stderr)
    ranked = []
    for index, word in enumerate(candidates, 1):
        vector = unit(embed(word, args.endpoint, args.model))
        ranked.append((sum(a * b for a, b in zip(vector, direction)), word))
        if index % 1000 == 0:
            print(f"  {index}/{len(candidates)}", file=sys.stderr)
    ranked.sort(key=lambda pair: (-pair[0], pair[1]))
    terms = sorted({word for _, word in ranked[: args.top_k]})

    lines = [
        "# Generated by benchmarks/distill_lexicon.py (calibration-time only).",
        "# The scored request path loads this and stays offline, deterministic and keyless.",
        "",
        "[routing]",
    ]
    if args.threshold is not None:
        lines.append(f"threshold = {args.threshold}")
    lines.append("weights = { reasoning_term_count = 5.0 }")
    lines += ["", "[routing.lexicon]",
              "reasoning_terms = [" + ", ".join(json.dumps(t) for t in terms) + "]"]
    with open(args.out, "w", encoding="utf-8") as handle:
        handle.write("\n".join(lines) + "\n")
    print(f"wrote {args.out}: {len(terms)} terms", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
