#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
router_bin="${WAYFINDER_ROUTER_BIN:-$(command -v wayfinder-router || true)}"
opencode_bin="${OPENCODE_BIN:-$(command -v opencode || true)}"
router_port="${WAYFINDER_SMOKE_ROUTER_PORT:-18288}"
provider_port="${WAYFINDER_SMOKE_PROVIDER_PORT:-18289}"
smoke_timeout="${WAYFINDER_SMOKE_TIMEOUT:-90}"
smoke_root="$(mktemp -d "${TMPDIR:-/tmp}/wayfinder-opencode-smoke.XXXXXX")"
router_pid=""
provider_pid=""
opencode_pid=""

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
  if [[ -n "$opencode_pid" ]]; then
    kill "$opencode_pid" >/dev/null 2>&1 || true
    wait "$opencode_pid" >/dev/null 2>&1 || true
  fi
  if [[ "${WAYFINDER_KEEP_SMOKE:-0}" == "1" ]]; then
    printf 'OpenCode smoke artifacts retained at %s\n' "$smoke_root" >&2
  else
    rm -rf -- "$smoke_root"
  fi
}
trap cleanup EXIT INT TERM

for command_name in node curl grep timeout; do
  command -v "$command_name" >/dev/null 2>&1 || {
    printf 'OpenCode smoke requires %s.\n' "$command_name" >&2
    exit 1
  }
done
if [[ -z "$router_bin" || ! -x "$router_bin" ]]; then
  printf '%s\n' "Set WAYFINDER_ROUTER_BIN to the candidate Router executable." >&2
  exit 1
fi
if [[ -z "$opencode_bin" || ! -x "$opencode_bin" ]]; then
  printf '%s\n' "Set OPENCODE_BIN to OpenCode 1.18.21." >&2
  exit 1
fi
if ! HOME="$smoke_root" XDG_DATA_HOME="$smoke_root/data" \
  "$opencode_bin" --version 2>&1 | grep -Fx "1.18.21" >/dev/null; then
  printf '%s\n' "OpenCode smoke requires version 1.18.21 exactly." >&2
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
  "$smoke_root/home" \
  "$smoke_root/config" \
  "$smoke_root/data" \
  "$smoke_root/cache" \
  "$smoke_root/workspace"
router_config="$smoke_root/wayfinder-router.toml"
opencode_config="$smoke_root/workspace/opencode.json"
evidence="$smoke_root/evidence.json"
models_catalog="$smoke_root/models.json"

cat > "$router_config" <<EOF
[gateway.models.local]
base_url = "http://127.0.0.1:${provider_port}/v1"
model = "smoke-model"

[gateway.models.cloud]
base_url = "http://127.0.0.1:${provider_port}/v1"
model = "smoke-model"
EOF

cat > "$opencode_config" <<EOF
{
  "\$schema": "https://opencode.ai/config.json",
  "model": "wayfinder/auto",
  "small_model": "wayfinder/auto",
  "share": "disabled",
  "permission": {
    "*": "deny",
    "bash": {
      "*": "deny",
      "printf WAYFINDER_OPENCODE_TOOL_ROUNDTRIP": "allow"
    }
  },
  "provider": {
    "wayfinder": {
      "npm": "@ai-sdk/openai-compatible",
      "name": "Wayfinder smoke",
      "options": {
        "baseURL": "http://127.0.0.1:${router_port}/v1",
        "apiKey": "wayfinder-local"
      },
      "models": {
        "auto": {
          "name": "Wayfinder Automatic",
          "limit": {
            "context": 200000,
            "output": 65536
          }
        }
      }
    }
  }
}
EOF
chmod 0600 "$router_config" "$opencode_config"
printf '{}\n' > "$models_catalog"
chmod 0600 "$models_catalog"

node "$repo_root/test/coding-agent-mock-provider.mjs" "$provider_port" "$evidence" \
  opencode-1.18.21 \
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

run_opencode() {
  local prompt="$1"
  cd "$smoke_root/workspace"
  exec env \
    HOME="$smoke_root/home" \
    XDG_CONFIG_HOME="$smoke_root/config" \
    XDG_DATA_HOME="$smoke_root/data" \
    XDG_CACHE_HOME="$smoke_root/cache" \
    OPENCODE_MODELS_PATH="$models_catalog" \
    OPENCODE_DISABLE_AUTOUPDATE=1 \
    OPENCODE_DISABLE_DEFAULT_PLUGINS=1 \
    OPENCODE_DISABLE_EXTERNAL_SKILLS=1 \
    OPENCODE_DISABLE_LSP_DOWNLOAD=1 \
    timeout --signal=TERM "${smoke_timeout}s" "$opencode_bin" run \
      --pure \
      --print-logs \
      --log-level DEBUG \
      --model wayfinder/auto \
      --format json \
      "$prompt" \
      </dev/null
}

if ! (run_opencode \
  "Use bash exactly once to run: printf WAYFINDER_OPENCODE_TOOL_ROUNDTRIP. Then report success.") \
  > "$smoke_root/opencode.jsonl" 2> "$smoke_root/opencode.err"; then
  cat "$smoke_root/opencode.err" >&2
  cat "$smoke_root/opencode.jsonl" >&2
  cat "$smoke_root/router.log" >&2
  cat "$smoke_root/provider.log" >&2
  exit 1
fi

grep -F "WAYFINDER_OPENCODE_SMOKE_OK" "$smoke_root/opencode.jsonl" >/dev/null
node -e '
  const fs = require("node:fs");
  const evidence = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
  if (evidence.agent !== "opencode"
      || evidence.client_version !== "1.18.21"
      || evidence.requests !== 2
      || evidence.auxiliary_requests !== 1
      || evidence.tool_name !== "bash"
      || evidence.tool_call_id !== "call_wayfinder_smoke"
      || evidence.tool_output_seen !== true
      || evidence.final_marker !== "WAYFINDER_OPENCODE_SMOKE_OK") {
    process.exit(1);
  }
' "$evidence"

set +e
(run_opencode \
  "Return the text WAYFINDER_OPENCODE_ERROR_PROBE without using a tool.") \
  > "$smoke_root/error.jsonl" 2> "$smoke_root/error.err"
error_status="$?"
set -e
if (( error_status == 0 || error_status == 124 )); then
  printf 'OpenCode error probe exited with unexpected status %s.\n' "$error_status" >&2
  cat "$smoke_root/error.err" >&2
  cat "$smoke_root/error.jsonl" >&2
  exit 1
fi
grep -F "WAYFINDER_OPENCODE_UPSTREAM_ERROR" \
  "$smoke_root/error.err" "$smoke_root/error.jsonl" >/dev/null
node -e '
  const fs = require("node:fs");
  const evidence = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
  if (evidence.error_request_seen !== true || evidence.auxiliary_requests !== 2) {
    process.exit(1);
  }
' "$evidence"

(run_opencode \
  "Return the text WAYFINDER_OPENCODE_CANCELLATION_PROBE without using a tool.") \
  > "$smoke_root/cancellation.jsonl" 2> "$smoke_root/cancellation.err" &
opencode_pid="$!"
for _ in {1..100}; do
  if node -e '
    try {
      const value = JSON.parse(require("node:fs").readFileSync(process.argv[1], "utf8"));
      process.exit(value.cancellation_started === true ? 0 : 1);
    } catch {
      process.exit(1);
    }
  ' "$evidence"; then
    break
  fi
  sleep 0.1
done
if ! kill -0 "$opencode_pid" >/dev/null 2>&1; then
  printf '%s\n' "OpenCode exited before the cancellation probe reached the provider." >&2
  cat "$smoke_root/cancellation.err" >&2
  cat "$smoke_root/cancellation.jsonl" >&2
  exit 1
fi
kill -TERM "$opencode_pid"
set +e
wait "$opencode_pid"
cancellation_status="$?"
set -e
if (( cancellation_status == 0 )); then
  printf '%s\n' "OpenCode cancellation probe exited successfully instead of being interrupted." >&2
  exit 1
fi
for _ in {1..100}; do
  if node -e '
    try {
      const value = JSON.parse(require("node:fs").readFileSync(process.argv[1], "utf8"));
      process.exit(value.cancellation_observed === true ? 0 : 1);
    } catch {
      process.exit(1);
    }
  ' "$evidence"; then
    break
  fi
  sleep 0.1
done
node -e '
  const fs = require("node:fs");
  const evidence = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
  if (evidence.requests !== 2
      || evidence.auxiliary_requests !== 3
      || evidence.tool_output_seen !== true
      || evidence.error_request_seen !== true
      || evidence.cancellation_started !== true
      || evidence.cancellation_observed !== true) {
    process.exit(1);
  }
' "$evidence"

printf '%s\n' \
  "OpenCode 1.18.21 completed streaming, tool, error, and cancellation contracts through the candidate Wayfinder Router."
