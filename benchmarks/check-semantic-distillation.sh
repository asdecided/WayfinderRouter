#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
router="${1:-$repo_root/rust/target/debug/wayfinder-router}"
fixture="$repo_root/benchmarks/fixtures/short-hard-long-easy.jsonl"

if [[ ! -x "$router" ]]; then
  echo "semantic-distillation check needs an executable Router: $router" >&2
  exit 1
fi
if ! command -v jq >/dev/null 2>&1; then
  echo "semantic-distillation check needs jq" >&2
  exit 1
fi

work_dir="$(mktemp -d)"
trap 'rm -rf -- "$work_dir"' EXIT
train="$work_dir/train.jsonl"
config="$work_dir/semantic.toml"
prompt="$work_dir/prompt.txt"

jq -c 'select(.split == "train") | {text, label}' "$fixture" > "$train"
"$router" calibrate "$train" \
  --costs local=0,cloud=1 \
  --quality-penalty 2 \
  --distill-lexicon \
  --out "$config" 2> "$work_dir/calibration.txt"

baseline_hard=0
semantic_hard=0
baseline_easy=0
semantic_easy=0
heldout=0
while IFS= read -r row; do
  [[ "$(jq -r '.split' <<<"$row")" == "heldout" ]] || continue
  ((heldout += 1))
  jq -r '.text' <<<"$row" > "$prompt"
  label="$(jq -r '.label' <<<"$row")"
  baseline="$(env -u WAYFINDER_CONFIG -u WAYFINDER_ROUTER_THRESHOLD \
    "$router" route --json "$prompt" | jq -r '.recommendation')"
  semantic="$(env WAYFINDER_CONFIG="$config" -u WAYFINDER_ROUTER_THRESHOLD \
    "$router" route --json "$prompt" | jq -r '.recommendation')"
  if [[ "$label" == "cloud" ]]; then
    [[ "$baseline" == "cloud" ]] && ((baseline_hard += 1))
    [[ "$semantic" == "cloud" ]] && ((semantic_hard += 1))
  else
    [[ "$baseline" == "local" ]] && ((baseline_easy += 1))
    [[ "$semantic" == "local" ]] && ((semantic_easy += 1))
  fi
done < "$fixture"

expected="8:0:4:0:4"
actual="$heldout:$baseline_hard:$semantic_hard:$baseline_easy:$semantic_easy"
if [[ "$actual" != "$expected" ]]; then
  echo "semantic distillation drifted: expected $expected, got $actual" >&2
  echo "fields: heldout:baseline-hard:semantic-hard:baseline-easy:semantic-easy" >&2
  exit 1
fi
if ! grep -q 'semantic_terms=' "$work_dir/calibration.txt"; then
  echo "semantic calibration receipt is missing" >&2
  exit 1
fi
if (( $(wc -c < "$config") > 16384 )); then
  echo "semantic config exceeded the 16 KiB evidence bound" >&2
  exit 1
fi

printf '%s\n' \
  "held-out short-hard recovery: 0/4 -> 4/4" \
  "held-out long-easy local routing: 0/4 -> 4/4" \
  "static config bytes: $(wc -c < "$config")"
