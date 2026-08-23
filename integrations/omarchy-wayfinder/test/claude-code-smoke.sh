#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
router_bin="${WAYFINDER_ROUTER_BIN:-$(command -v wayfinder-router || true)}"
claude_bin="${CLAUDE_CODE_BIN:-$(command -v claude || true)}"
router_port="${WAYFINDER_SMOKE_ROUTER_PORT:-18188}"
provider_port="${WAYFINDER_SMOKE_PROVIDER_PORT:-18189}"
smoke_timeout="${WAYFINDER_SMOKE_TIMEOUT:-90}"
smoke_root="$(mktemp -d "${TMPDIR:-/tmp}/wayfinder-claude-code-smoke.XXXXXX")"
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

for command_name in node curl grep timeout env; do
  command -v "$command_name" >/dev/null 2>&1 || {
    printf 'Claude Code smoke requires %s.\n' "$command_name" >&2
    exit 1
  }
done
if [[ -z "$router_bin" || ! -x "$router_bin" ]]; then
  printf '%s\n' "Set WAYFINDER_ROUTER_BIN to the candidate Router executable." >&2
  exit 1
fi
if [[ -z "$claude_bin" || ! -x "$claude_bin" ]]; then
  printf '%s\n' "Set CLAUDE_CODE_BIN to Claude Code 2.1.241." >&2
  exit 1
fi
if ! "$claude_bin" --version 2>&1 | grep -Fx "2.1.241 (Claude Code)" >/dev/null; then
  printf '%s\n' "Claude Code smoke requires version 2.1.241 exactly." >&2
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
if [[ "$router_port" == "$provider_port" ]]; then
  printf '%s\n' "Router and provider smoke ports must differ." >&2
  exit 1
fi

install -d -m 0700 \
  "$smoke_root/claude-config" \
  "$smoke_root/config" \
  "$smoke_root/home" \
  "$smoke_root/workspace"
router_config="$smoke_root/wayfinder-router.toml"
evidence="$smoke_root/evidence.json"

cat > "$router_config" <<EOF
[gateway.models.local]
base_url = "http://127.0.0.1:${provider_port}/v1"
model = "smoke-model"

[gateway.models.cloud]
base_url = "http://127.0.0.1:${provider_port}/v1"
model = "smoke-model"
EOF
chmod 0600 "$router_config"

node "$repo_root/test/coding-agent-mock-provider.mjs" "$provider_port" "$evidence" \
  claude-code-2.1.241 \
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

if ! (
  cd "$smoke_root/workspace"
  env -u ANTHROPIC_API_KEY \
    HOME="$smoke_root/home" \
    XDG_CONFIG_HOME="$smoke_root/config" \
    CLAUDE_CONFIG_DIR="$smoke_root/claude-config" \
    ANTHROPIC_BASE_URL="http://127.0.0.1:${router_port}" \
    ANTHROPIC_AUTH_TOKEN="wayfinder-local" \
    ANTHROPIC_MODEL="auto" \
    CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY=1 \
    CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1 \
    timeout --signal=TERM "${smoke_timeout}s" "$claude_bin" -p \
      --safe-mode \
      --disable-slash-commands \
      --no-session-persistence \
      --tools Bash \
      --allowedTools Bash \
      --output-format text \
      "Use Bash exactly once to run: printf WAYFINDER_CLAUDE_TOOL_ROUNDTRIP. Then report success." \
      </dev/null
) > "$smoke_root/final.txt" 2> "$smoke_root/claude.err"; then
  cat "$smoke_root/claude.err" >&2
  cat "$smoke_root/final.txt" >&2
  cat "$smoke_root/router.log" >&2
  cat "$smoke_root/provider.log" >&2
  exit 1
fi

grep -Fx "WAYFINDER_CLAUDE_SMOKE_OK" "$smoke_root/final.txt" >/dev/null
node -e '
  const fs = require("node:fs");
  const evidence = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
  if (evidence.agent !== "claude-code"
      || evidence.client_version !== "2.1.241"
      || evidence.requests !== 2
      || evidence.tool_name !== "Bash"
      || evidence.tool_call_id !== "call_wayfinder_smoke"
      || evidence.tool_output_seen !== true
      || evidence.final_marker !== "WAYFINDER_CLAUDE_SMOKE_OK") {
    process.exit(1);
  }
' "$evidence"

printf '%s\n' "Claude Code 2.1.241 completed a tool round-trip through the candidate Wayfinder Router."
