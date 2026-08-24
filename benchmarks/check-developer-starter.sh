#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
router="${1:-$repo_root/rust/target/debug/wayfinder-router}"
corpus="$repo_root/benchmarks/blind/openai-cross-provider.jsonl"

if [[ ! -x "$router" ]]; then
  echo "developer-starter check needs an executable Router: $router" >&2
  exit 1
fi
if ! command -v jq >/dev/null 2>&1; then
  echo "developer-starter check needs jq" >&2
  exit 1
fi

work_dir="$(mktemp -d)"
trap 'rm -rf -- "$work_dir"' EXIT
prompt_file="$work_dir/prompt.txt"

total=0
hard=0
easy=0
cloud=0
hard_cloud=0
easy_cloud=0

while IFS= read -r row; do
  prompt="$(jq -er '.prompt | strings' <<<"$row")"
  difficulty="$(jq -er '.difficulty | select(. == "easy" or . == "hard")' <<<"$row")"
  printf '%s' "$prompt" > "$prompt_file"
  decision="$({
    env -u WAYFINDER_CONFIG -u WAYFINDER_ROUTER_THRESHOLD \
      "$router" route --json "$prompt_file"
  })"
  recommendation="$(
    jq -er '.recommendation | select(. == "local" or . == "cloud")' <<<"$decision"
  )"

  ((total += 1))
  if [[ "$difficulty" == "hard" ]]; then
    ((hard += 1))
  else
    ((easy += 1))
  fi
  if [[ "$recommendation" == "cloud" ]]; then
    ((cloud += 1))
    if [[ "$difficulty" == "hard" ]]; then
      ((hard_cloud += 1))
    else
      ((easy_cloud += 1))
    fi
  fi
done < "$corpus"

expected="154:94:60:122:90:32"
actual="$total:$hard:$easy:$cloud:$hard_cloud:$easy_cloud"
if [[ "$actual" != "$expected" ]]; then
  echo "developer-starter routing drifted: expected $expected, got $actual" >&2
  echo "fields: total:hard:easy:cloud:hard-cloud:easy-cloud" >&2
  exit 1
fi

printf '%s\n' \
  "developer starter: 122/154 cloud, 32/154 local" \
  "hard recovery: 90/94 (0.9574); cost savings: 32/154 (0.2078)"
