#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
router_bin="${WAYFINDER_ROUTER_BIN:-$(command -v wayfinder-router || true)}"
pi_bin="${PI_BIN:-$(command -v pi || true)}"
router_port="${WAYFINDER_SMOKE_ROUTER_PORT:-18388}"
provider_port="${WAYFINDER_SMOKE_PROVIDER_PORT:-18389}"
smoke_timeout="${WAYFINDER_SMOKE_TIMEOUT:-90}"
smoke_root="$(mktemp -d "${TMPDIR:-/tmp}/wayfinder-pi-smoke.XXXXXX")"
router_pid=""
provider_pid=""
pi_pid=""

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
  if [[ -n "$pi_pid" ]]; then
    kill "$pi_pid" >/dev/null 2>&1 || true
    wait "$pi_pid" >/dev/null 2>&1 || true
  fi
  if [[ "${WAYFINDER_KEEP_SMOKE:-0}" == "1" ]]; then
    printf 'Pi smoke artifacts retained at %s\n' "$smoke_root" >&2
  else
    rm -rf -- "$smoke_root"
  fi
}
trap cleanup EXIT INT TERM

for command_name in node curl grep timeout; do
  command -v "$command_name" >/dev/null 2>&1 || {
    printf 'Pi smoke requires %s.\n' "$command_name" >&2
    exit 1
  }
done
if [[ -z "$router_bin" || ! -x "$router_bin" ]]; then
  printf '%s\n' "Set WAYFINDER_ROUTER_BIN to the candidate Router executable." >&2
  exit 1
fi
if [[ -z "$pi_bin" || ! -x "$pi_bin" ]]; then
  printf '%s\n' "Set PI_BIN to Pi 0.84.3." >&2
  exit 1
fi
if ! "$pi_bin" --version 2>&1 | grep -Fx "0.84.3" >/dev/null; then
  printf '%s\n' "Pi smoke requires version 0.84.3 exactly." >&2
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
  "$smoke_root/home/.pi/agent" \
  "$smoke_root/workspace"
router_config="$smoke_root/wayfinder-router.toml"
pi_config="$smoke_root/home/.pi/agent/models.json"
evidence="$smoke_root/evidence.json"

cat > "$router_config" <<EOF
[gateway.models.local]
base_url = "http://127.0.0.1:${provider_port}/v1"
model = "smoke-model"

[gateway.models.cloud]
base_url = "http://127.0.0.1:${provider_port}/v1"
model = "smoke-model"
EOF

cat > "$pi_config" <<EOF
{
  "providers": {
    "wayfinder": {
      "baseUrl": "http://127.0.0.1:${router_port}/v1",
      "api": "openai-completions",
      "apiKey": "wayfinder-local",
      "compat": {
        "supportsDeveloperRole": false,
        "supportsReasoningEffort": false
      },
      "models": [
        { "id": "auto", "name": "Wayfinder Automatic" }
      ]
    }
  }
}
EOF
chmod 0600 "$router_config" "$pi_config"

node "$repo_root/test/coding-agent-mock-provider.mjs" "$provider_port" "$evidence" \
  pi-0.84.3 \
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

run_pi() {
  local prompt="$1"
  cd "$smoke_root/workspace"
  exec env \
    HOME="$smoke_root/home" \
    timeout --signal=TERM "${smoke_timeout}s" "$pi_bin" \
      --mode json \
      --no-session \
      --no-approve \
      --tools bash \
      --provider wayfinder \
      --model auto \
      "$prompt" \
      </dev/null
}

if ! (run_pi \
  "Use bash exactly once to run: printf WAYFINDER_PI_TOOL_ROUNDTRIP. Then report success.") \
  > "$smoke_root/pi.jsonl" 2> "$smoke_root/pi.err"; then
  cat "$smoke_root/pi.err" >&2
  cat "$smoke_root/pi.jsonl" >&2
  cat "$smoke_root/router.log" >&2
  cat "$smoke_root/provider.log" >&2
  exit 1
fi

grep -F "WAYFINDER_PI_SMOKE_OK" "$smoke_root/pi.jsonl" >/dev/null
node -e '
  const fs = require("node:fs");
  const evidence = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
  if (evidence.agent !== "pi"
      || evidence.client_version !== "0.84.3"
      || evidence.requests !== 2
      || evidence.auxiliary_requests !== 0
      || evidence.tool_name !== "bash"
      || evidence.tool_call_id !== "call_wayfinder_smoke"
      || evidence.tool_output_seen !== true
      || evidence.final_marker !== "WAYFINDER_PI_SMOKE_OK") {
    process.exit(1);
  }
' "$evidence"

set +e
(run_pi "Return the text WAYFINDER_PI_ERROR_PROBE without using a tool.") \
  > "$smoke_root/error.jsonl" 2> "$smoke_root/error.err"
error_status="$?"
set -e
if (( error_status == 124 )); then
  printf 'Pi error probe exited with unexpected status %s.\n' "$error_status" >&2
  cat "$smoke_root/error.err" >&2
  cat "$smoke_root/error.jsonl" >&2
  exit 1
fi
grep -F "WAYFINDER_PI_UPSTREAM_ERROR" \
  "$smoke_root/error.err" "$smoke_root/error.jsonl" >/dev/null
node -e '
  const fs = require("node:fs");
  const events = fs.readFileSync(process.argv[1], "utf8")
    .trim().split("\n").map(line => JSON.parse(line));
  if (!events.some(event => event.type === "message_end"
      && event.message?.stopReason === "error"
      && event.message?.errorMessage?.includes("WAYFINDER_PI_UPSTREAM_ERROR"))) {
    process.exit(1);
  }
' "$smoke_root/error.jsonl"
node -e '
  const fs = require("node:fs");
  const evidence = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
  if (evidence.error_request_seen !== true || evidence.auxiliary_requests !== 0) {
    process.exit(1);
  }
' "$evidence"

(run_pi "Return the text WAYFINDER_PI_CANCELLATION_PROBE without using a tool.") \
  > "$smoke_root/cancellation.jsonl" 2> "$smoke_root/cancellation.err" &
pi_pid="$!"
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
if ! kill -0 "$pi_pid" >/dev/null 2>&1; then
  printf '%s\n' "Pi exited before the cancellation probe reached the provider." >&2
  cat "$smoke_root/cancellation.err" >&2
  cat "$smoke_root/cancellation.jsonl" >&2
  exit 1
fi
kill -TERM "$pi_pid"
set +e
wait "$pi_pid"
cancellation_status="$?"
set -e
if (( cancellation_status == 0 )); then
  printf '%s\n' "Pi cancellation probe exited successfully instead of being interrupted." >&2
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
      || evidence.auxiliary_requests !== 0
      || evidence.tool_output_seen !== true
      || evidence.error_request_seen !== true
      || evidence.cancellation_started !== true
      || evidence.cancellation_observed !== true) {
    process.exit(1);
  }
' "$evidence"

printf '%s\n' \
  "Pi 0.84.3 completed streaming, tool, error, and cancellation contracts through the candidate Wayfinder Router."
