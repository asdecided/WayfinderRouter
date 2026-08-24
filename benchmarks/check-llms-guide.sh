#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
router="${1:-$repo_root/rust/target/debug/wayfinder-router}"
guide="$repo_root/llms.txt"

if [[ ! -x "$router" ]]; then
  echo "LLM guide check needs an executable Router: $router" >&2
  exit 1
fi

grep -Fxq '# Wayfinder' "$guide"
grep -Fq 'wayfinder-router init --preset hybrid' "$guide"
grep -Fq 'wayfinder-router doctor' "$guide"
grep -Fq 'wayfinder-router connect codex' "$guide"
grep -Fq 'wayfinder-router calibrate prompts.jsonl' "$guide"
grep -Fq -- '--distill-lexicon' "$guide"
grep -Fq './install.sh --rollback-router' "$guide"

for path in \
  README.md \
  docs/coding-agent-quickstarts.md \
  docs/lexical-routing.md \
  docs/managed-gateway-deployment.md \
  decisions/WF-ADR-0001-standalone-deterministic-router.md \
  decisions/WF-ADR-0003-calibration-and-classifier.md \
  decisions/WF-ADR-0078-evidence-backed-developer-starter-threshold.md \
  CONTRIBUTING.md \
  SECURITY.md; do
  test -f "$repo_root/$path"
done

work_dir="$(mktemp -d)"
trap 'rm -rf -- "$work_dir"' EXIT
config="$work_dir/wayfinder-router.toml"
prompt="$work_dir/prompt.txt"

"$router" init --preset hybrid --path "$config" > "$work_dir/init.txt"
before="$(sha256sum "$config")"
if "$router" init --preset hybrid --path "$config" >/dev/null 2>&1; then
  echo "documented init unexpectedly replaced an existing policy" >&2
  exit 1
fi
test "$(sha256sum "$config")" = "$before"
printf '%s' 'Prove the halting problem is undecidable.' > "$prompt"
test "$(WAYFINDER_CONFIG="$config" "$router" route --json "$prompt" | jq -r '.recommendation')" = cloud
OPENAI_API_KEY=guide-contract "$router" doctor --config "$config" --json \
  | jq -e '.schema_version == "1" and .routing_distribution.status == "info"' >/dev/null

printf '%s\n' "llms.txt native setup contract passed"
