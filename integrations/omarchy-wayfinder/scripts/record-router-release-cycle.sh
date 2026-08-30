#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
plugin_id="io.github.asdecided.wayfinder"
installer="${WAYFINDER_LIFECYCLE_INSTALLER:-$repo_root/install.sh}"
router_url="${WAYFINDER_ROUTER_URL:-http://127.0.0.1:8088}"
router_provenance_dir="${XDG_STATE_HOME:-$HOME/.local/state}/wayfinder"
router_provenance_file="$router_provenance_dir/omarchy-router-install"
source "$repo_root/scripts/router-lifecycle.sh"

usage() {
  printf '%s\n' \
    "usage: record-router-release-cycle.sh --from VERSION --to VERSION [--evidence PATH]" >&2
}

from_version=""
to_version=""
evidence_path=""
while (( $# > 0 )); do
  case "$1" in
    --from)
      [[ $# -ge 2 && -z "$from_version" ]] || { usage; exit 2; }
      from_version="$2"
      shift 2
      ;;
    --to)
      [[ $# -ge 2 && -z "$to_version" ]] || { usage; exit 2; }
      to_version="$2"
      shift 2
      ;;
    --evidence)
      [[ $# -ge 2 && -z "$evidence_path" ]] || { usage; exit 2; }
      evidence_path="$2"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      usage
      exit 2
      ;;
  esac
done

for version in "$from_version" "$to_version"; do
  [[ "$version" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]] || {
    printf 'Invalid Router release version: %s\n' "$version" >&2
    exit 2
  }
done
[[ "$from_version" != "$to_version" ]] || {
  printf '%s\n' "--from and --to must name different releases." >&2
  exit 2
}
router_url="${router_url%/}"
[[ "$router_url" =~ ^http://(127\.0\.0\.1|localhost|\[::1\])(:[0-9]{1,5})?$ ]] || {
  printf '%s\n' "WAYFINDER_ROUTER_URL must be an explicit loopback HTTP origin." >&2
  exit 2
}
[[ -f "$installer" && ! -L "$installer" ]] || {
  printf '%s\n' "The reviewed plugin installer is missing or linked." >&2
  exit 1
}
for command_name in curl jq omarchy omarchy-shell realpath sha256sum systemctl; do
  command -v "$command_name" >/dev/null 2>&1 || {
    printf 'Real lifecycle evidence requires %s.\n' "$command_name" >&2
    exit 1
  }
done

pinned_version="$(sed -n 's/^router_version="\([^"]*\)"$/\1/p' "$installer")"
[[ "$pinned_version" == "$to_version" ]] || {
  printf 'The reviewed plugin source pins Router %s, not requested target %s.\n' \
    "${pinned_version:-<missing>}" "$to_version" >&2
  exit 1
}

if [[ -z "$evidence_path" ]]; then
  evidence_path="$HOME/wayfinder-router-${from_version}-to-${to_version}-cycle.json"
fi
router_require_user_path "$evidence_path"
[[ ! -L "$evidence_path" ]] || {
  printf '%s\n' "The evidence path must not be a symbolic link." >&2
  exit 1
}
install -d -m 0700 "$(dirname -- "$evidence_path")"

cycle_failed() {
  local status=$?
  printf '%s\n' \
    "The release cycle stopped. Inspect the active Router provenance and service before retrying; no automatic recovery was attempted." >&2
  exit "$status"
}
trap cycle_failed ERR

check_omarchy() {
  omarchy-shell shell ping >/dev/null
}

check_health() {
  local response=""
  for _ in {1..100}; do
    if response="$(curl --fail --silent --show-error --max-time 2 "$router_url/healthz" 2>/dev/null)" \
      && jq -e '.status == "ok"' <<<"$response" >/dev/null; then
      return 0
    fi
    sleep 0.1
  done
  printf 'Wayfinder Router did not become healthy at %s.\n' "$router_url" >&2
  return 1
}

restart_and_check() {
  systemctl --user restart wayfinder-router.service
  systemctl --user is-active --quiet wayfinder-router.service
  check_health
  check_omarchy
}

capture_state() {
  local expected_version="$1"
  router_load_current
  [[ "$current_router_version" == "$expected_version" ]] || {
    printf 'Expected Router %s but plugin provenance reports %s.\n' \
      "$expected_version" "$current_router_version" >&2
    return 1
  }
  [[ "$current_router_target" =~ ^(x86_64|aarch64)-unknown-linux-gnu$ ]]
  [[ "$current_router_archive_sha256" =~ ^[0-9a-f]{64}$ ]]
  [[ "$current_router_binary_sha256" =~ ^[0-9a-f]{64}$ ]]
  [[ "$("$current_router_path" --version)" == "wayfinder-router $expected_version" ]]
  printf '%s|%s|%s\n' \
    "$current_router_target" "$current_router_archive_sha256" "$current_router_binary_sha256"
}

check_omarchy
systemctl --user is-active --quiet wayfinder-router.service
check_health
IFS='|' read -r initial_target initial_archive initial_binary \
  < <(capture_state "$from_version")

bash "$installer" --upgrade-router
restart_and_check
IFS='|' read -r upgraded_target upgraded_archive upgraded_binary \
  < <(capture_state "$to_version")

bash "$installer" --rollback-router
restart_and_check
IFS='|' read -r rollback_target rollback_archive rollback_binary \
  < <(capture_state "$from_version")

bash "$installer" --rollback-router
restart_and_check
IFS='|' read -r final_target final_archive final_binary \
  < <(capture_state "$to_version")

[[ "$initial_target" == "$upgraded_target" \
    && "$initial_target" == "$rollback_target" \
    && "$initial_target" == "$final_target" ]]
[[ "$initial_archive" == "$rollback_archive" && "$initial_binary" == "$rollback_binary" ]]
[[ "$upgraded_archive" == "$final_archive" && "$upgraded_binary" == "$final_binary" ]]

evidence_temp="$(mktemp "$(dirname -- "$evidence_path")/.wayfinder-cycle.XXXXXX")"
jq -n \
  --arg checked_at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
  --arg from "$from_version" \
  --arg to "$to_version" \
  --arg target "$initial_target" \
  --arg from_archive "$initial_archive" \
  --arg from_binary "$initial_binary" \
  --arg to_archive "$upgraded_archive" \
  --arg to_binary "$upgraded_binary" \
  '{
    schema_version: "wf-omarchy-router-cycle-v1",
    checked_at: $checked_at,
    plugin_id: "io.github.asdecided.wayfinder",
    target: $target,
    sequence: [$from, $to, $from, $to],
    releases: {
      ($from): {archive_sha256: $from_archive, binary_sha256: $from_binary},
      ($to): {archive_sha256: $to_archive, binary_sha256: $to_binary}
    },
    service_restart: "passed",
    health: "passed",
    omarchy_shell: "passed",
    final_version: $to
  }' > "$evidence_temp"
chmod 0600 "$evidence_temp"
mv -f -- "$evidence_temp" "$evidence_path"
trap - ERR

printf 'Recorded real Omarchy Router cycle evidence at %s\n' "$evidence_path"
