#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
router_bin="${WAYFINDER_ROUTER_BIN:-$(command -v wayfinder-router || true)}"
aider_bin="${AIDER_BIN:-$(command -v aider || true)}"
router_port="${WAYFINDER_SMOKE_ROUTER_PORT:-18488}"
provider_port="${WAYFINDER_SMOKE_PROVIDER_PORT:-18489}"
smoke_timeout="${WAYFINDER_SMOKE_TIMEOUT:-90}"
smoke_root="$(mktemp -d "${TMPDIR:-/tmp}/wayfinder-aider-smoke.XXXXXX")"
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
  if [[ "${WAYFINDER_KEEP_SMOKE:-0}" == "1" ]]; then
    printf 'Aider smoke artifacts retained at %s\n' "$smoke_root" >&2
  else
    rm -rf -- "$smoke_root"
  fi
}
trap cleanup EXIT INT TERM

for command_name in node curl grep timeout git; do
  command -v "$command_name" >/dev/null 2>&1 || {
    printf 'Aider smoke requires %s.\n' "$command_name" >&2
    exit 1
  }
done
if [[ -z "$router_bin" || ! -x "$router_bin" ]]; then
  printf '%s\n' "Set WAYFINDER_ROUTER_BIN to the candidate Router executable." >&2
  exit 1
fi
if [[ -z "$aider_bin" || ! -x "$aider_bin" ]]; then
  printf '%s\n' "Set AIDER_BIN to Aider 0.86.1." >&2
  exit 1
fi
if ! "$aider_bin" --version 2>&1 | grep -Fx "aider 0.86.1" >/dev/null; then
  printf '%s\n' "Aider smoke requires version 0.86.1 exactly." >&2
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
  "$smoke_root/cache" \
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

printf 'before\n' > "$smoke_root/workspace/smoke.txt"
(
  cd "$smoke_root/workspace"
  git init --quiet
  git config user.name "Wayfinder smoke"
  git config user.email "smoke@wayfinder.local"
  git add smoke.txt
  git commit --quiet --message baseline
)

node "$repo_root/test/aider-mock-provider.mjs" "$provider_port" "$evidence" \
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
  env \
    HOME="$smoke_root/home" \
    XDG_CONFIG_HOME="$smoke_root/config" \
    XDG_CACHE_HOME="$smoke_root/cache" \
    OPENAI_API_BASE="http://127.0.0.1:${router_port}/v1" \
    OPENAI_API_KEY="wayfinder-local" \
    timeout --signal=TERM "${smoke_timeout}s" "$aider_bin" \
      --model openai/auto \
      --edit-format diff \
      --message "Replace the entire contents of smoke.txt with exactly WAYFINDER_AIDER_EDIT_OK." \
      --stream \
      --yes-always \
      --no-auto-commits \
      --no-gitignore \
      --no-add-gitignore-files \
      --no-check-update \
      --no-show-model-warnings \
      --no-analytics \
      --no-pretty \
      --no-fancy-input \
      --no-detect-urls \
      --no-suggest-shell-commands \
      --map-tokens 0 \
      smoke.txt \
      </dev/null
) > "$smoke_root/aider.out" 2> "$smoke_root/aider.err"; then
  cat "$smoke_root/aider.err" >&2
  cat "$smoke_root/aider.out" >&2
  cat "$smoke_root/router.log" >&2
  cat "$smoke_root/provider.log" >&2
  exit 1
fi

grep -Fx "WAYFINDER_AIDER_EDIT_OK" "$smoke_root/workspace/smoke.txt" >/dev/null
if [[ "$(wc -l < "$smoke_root/workspace/smoke.txt")" -ne 1 ]]; then
  printf '%s\n' "Aider did not produce the exact one-line edit." >&2
  exit 1
fi
node -e '
  const fs = require("node:fs");
  const evidence = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
  if (evidence.agent !== "aider"
      || evidence.client_version !== "0.86.1"
      || evidence.requests !== 1
      || evidence.edit_request_seen !== true
      || evidence.stream_requested !== true
      || evidence.final_marker !== "WAYFINDER_AIDER_EDIT_OK") {
    process.exit(1);
  }
' "$evidence"

printf '%s\n' "Aider 0.86.1 streamed an edit through Wayfinder and applied it successfully."
