#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
router_bin="${WAYFINDER_ROUTER_BIN:-$(command -v wayfinder-router || true)}"
codex_bin="${CODEX_BIN:-$(command -v codex || true)}"
router_port="${WAYFINDER_SMOKE_ROUTER_PORT:-18088}"
provider_port="${WAYFINDER_SMOKE_PROVIDER_PORT:-18089}"
smoke_timeout="${WAYFINDER_SMOKE_TIMEOUT:-90}"
codex_sandbox="${WAYFINDER_CODEX_SANDBOX:-read-only}"
smoke_root="$(mktemp -d "${TMPDIR:-/tmp}/wayfinder-codex-smoke.XXXXXX")"
router_pid=""
provider_pid=""

cleanup() {
  set +e
  if [[ -n "$router_pid" ]]; then
    kill "$router_pid" >/dev/null 2>&1 || true
    wait "$router_pid" >/dev/null 2>&1 || true
  fi
  if [[ -n "$provider_pid" ]]; then
    kill "$provider_pid" >/dev/null 2>&1 || true
    wait "$provider_pid" >/dev/null 2>&1 || true
  fi
  rm -rf -- "$smoke_root"
}
trap cleanup EXIT INT TERM

for command_name in node curl grep timeout; do
  command -v "$command_name" >/dev/null 2>&1 || {
    printf 'Codex smoke requires %s.\n' "$command_name" >&2
    exit 1
  }
done
if [[ -z "$router_bin" || ! -x "$router_bin" ]]; then
  printf '%s\n' "Set WAYFINDER_ROUTER_BIN to the candidate Router executable." >&2
  exit 1
fi
if [[ -z "$codex_bin" || ! -x "$codex_bin" ]]; then
  printf '%s\n' "Set CODEX_BIN to Codex CLI 0.149.0." >&2
  exit 1
fi
if ! "$codex_bin" --version 2>&1 | grep -Fx "codex-cli 0.149.0" >/dev/null; then
  printf '%s\n' "Codex smoke requires codex-cli 0.149.0 exactly." >&2
  exit 1
fi
for port in "$router_port" "$provider_port"; do
  [[ "$port" =~ ^[0-9]+$ ]] && (( port >= 1024 && port <= 65535 )) || {
    printf 'Invalid smoke-test port: %s\n' "$port" >&2
    exit 1
  }
done
[[ "$smoke_timeout" =~ ^[0-9]+$ ]] && (( smoke_timeout >= 10 && smoke_timeout <= 300 )) || {
  printf 'Invalid smoke-test timeout: %s\n' "$smoke_timeout" >&2
  exit 1
}
if [[ "$codex_sandbox" != "read-only" && "$codex_sandbox" != "danger-full-access" ]]; then
  printf 'Invalid Codex smoke sandbox: %s\n' "$codex_sandbox" >&2
  exit 1
fi
if [[ "$router_port" == "$provider_port" ]]; then
  printf '%s\n' "Router and provider smoke ports must differ." >&2
  exit 1
fi

install -d -m 0700 "$smoke_root/codex-home" "$smoke_root/workspace"
router_config="$smoke_root/wayfinder-router.toml"
codex_config="$smoke_root/codex-home/config.toml"
evidence="$smoke_root/evidence.json"

cat > "$router_config" <<EOF
[gateway.models.local]
base_url = "http://127.0.0.1:${provider_port}/v1"
model = "smoke-model"

[gateway.models.cloud]
base_url = "http://127.0.0.1:${provider_port}/v1"
model = "smoke-model"
EOF

cat > "$codex_config" <<EOF
model_provider = "wayfinder"
model = "auto"
approval_policy = "never"
project_doc_max_bytes = 0
web_search = "disabled"

[features]
apps = false
plugins = false
recommended_plugins = false
tool_suggest = false
shell_snapshot = false

[skills]
include_instructions = false

[skills.bundled]
enabled = false

[model_providers.wayfinder]
name = "Wayfinder smoke"
base_url = "http://127.0.0.1:${router_port}/v1"
wire_api = "responses"
requires_openai_auth = false
EOF
chmod 0600 "$router_config" "$codex_config"

node "$repo_root/test/coding-agent-mock-provider.mjs" "$provider_port" "$evidence" \
  codex-0.149.0 \
  > "$smoke_root/provider.log" 2>&1 &
provider_pid="$!"

for _ in {1..50}; do
  if curl --fail --silent "http://127.0.0.1:${provider_port}/healthz" >/dev/null; then
    break
  fi
  sleep 0.1
done
curl --fail --silent "http://127.0.0.1:${provider_port}/healthz" >/dev/null || {
  cat "$smoke_root/provider.log" >&2
  exit 1
}

(
  cd "$smoke_root"
  "$router_bin" serve \
    --host 127.0.0.1 \
    --port "$router_port" \
    --config "$router_config"
) > "$smoke_root/router.log" 2>&1 &
router_pid="$!"

for _ in {1..100}; do
  if curl --fail --silent "http://127.0.0.1:${router_port}/healthz" >/dev/null; then
    break
  fi
  sleep 0.1
done
curl --fail --silent "http://127.0.0.1:${router_port}/healthz" >/dev/null || {
  cat "$smoke_root/router.log" >&2
  exit 1
}

if ! CODEX_HOME="$smoke_root/codex-home" timeout --signal=TERM "${smoke_timeout}s" "$codex_bin" exec \
  --strict-config \
  --ephemeral \
  --skip-git-repo-check \
  --sandbox "$codex_sandbox" \
  --cd "$smoke_root/workspace" \
  --color never \
  --json \
  --output-last-message "$smoke_root/final.txt" \
  "Use exec_command exactly once to run: printf WAYFINDER_TOOL_ROUNDTRIP. Then report success." \
  </dev/null > "$smoke_root/codex.jsonl" 2> "$smoke_root/codex.err"; then
  cat "$smoke_root/codex.err" >&2
  cat "$smoke_root/codex.jsonl" >&2
  cat "$smoke_root/router.log" >&2
  cat "$smoke_root/provider.log" >&2
  exit 1
fi

grep -Fx "WAYFINDER_CODEX_SMOKE_OK" "$smoke_root/final.txt" >/dev/null
node -e '
  const fs = require("node:fs");
  const evidence = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
  if (evidence.agent !== "codex"
      || evidence.client_version !== "0.149.0"
      || evidence.requests !== 2
      || evidence.tool_call_id !== "call_wayfinder_smoke"
      || evidence.tool_output_seen !== true
      || evidence.final_marker !== "WAYFINDER_CODEX_SMOKE_OK") {
    process.exit(1);
  }
' "$evidence"

printf '%s\n' "Codex 0.149.0 completed a tool round-trip through the candidate Wayfinder Router."
