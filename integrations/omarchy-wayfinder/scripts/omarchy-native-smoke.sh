#!/usr/bin/env bash
set -euo pipefail

plugin_id="io.github.asdecided.wayfinder"
script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
plugin_root="$(cd -- "$script_dir/.." && pwd)"
installed_plugin_dir="${XDG_CONFIG_HOME:-$HOME/.config}/omarchy/plugins/$plugin_id"
router_provenance_dir="${XDG_STATE_HOME:-$HOME/.local/state}/wayfinder"
router_provenance_file="$router_provenance_dir/omarchy-router-install"
endpoint="${WAYFINDER_SMOKE_ENDPOINT:-http://127.0.0.1:8088}"
output_file=""
confirm_widget=false
confirm_first_run=false
restart_shell=false

fail() {
  printf 'native smoke failed: %s\n' "$*" >&2
  exit 1
}

usage() {
  printf '%s\n' \
    "usage: $0 --restart-shell --confirm-widget-visible --confirm-first-run-complete [--output FILE]" \
    "" \
    "The confirmation flags are operator attestations. The smoke restarts the Omarchy shell," \
    "then verifies that the plugin, Router service, and loopback health endpoint survive."
}

while (( $# > 0 )); do
  case "$1" in
    --restart-shell)
      restart_shell=true
      shift
      ;;
    --confirm-widget-visible)
      confirm_widget=true
      shift
      ;;
    --confirm-first-run-complete)
      confirm_first_run=true
      shift
      ;;
    --output)
      [[ -n "${2:-}" ]] || { usage >&2; exit 2; }
      output_file="$2"
      shift 2
      ;;
    -h | --help)
      usage
      exit 0
      ;;
    *)
      usage >&2
      exit 2
      ;;
  esac
done

[[ "$restart_shell" == true && "$confirm_widget" == true && "$confirm_first_run" == true ]] || {
  usage >&2
  exit 2
}

for command_name in curl git jq omarchy omarchy-restart-shell omarchy-shell quickshell \
  sha256sum sort systemctl wayfinder-router; do
  command -v "$command_name" >/dev/null 2>&1 || fail "missing required command: $command_name"
done

[[ "$endpoint" =~ ^http://(127\.0\.0\.1|localhost)(:[0-9]{1,5})?$ ]] ||
  fail "WAYFINDER_SMOKE_ENDPOINT must be a loopback HTTP origin without a path"

[[ "$(realpath -m -- "$plugin_root")" == "$(realpath -m -- "$installed_plugin_dir")" ]] ||
  fail "run this script from the installed Omarchy plugin checkout"
[[ -d "$plugin_root/.git" ]] || fail "the installed plugin must remain a Git checkout"
[[ -z "$(git -C "$plugin_root" status --porcelain)" ]] ||
  fail "the installed plugin checkout has local changes"

compatibility_file="$plugin_root/compatibility.json"
[[ -f "$compatibility_file" ]] || fail "compatibility.json is missing"
expected_plugin_version="$(jq -er '.plugin.version' "$compatibility_file")"
expected_omarchy_version="$(jq -er '.omarchy.version' "$compatibility_file")"
expected_quickshell_floor="$(jq -er '.quickshell.versionFloor' "$compatibility_file")"
expected_router_version="$(jq -er '.router.release' "$compatibility_file")"
manifest_version="$(jq -er '.version' "$plugin_root/manifest.json")"
[[ "$manifest_version" == "$expected_plugin_version" ]] || fail "plugin version drift"

source_commit="$(git -C "$plugin_root" rev-parse HEAD)"
[[ "$source_commit" =~ ^[0-9a-f]{40}$ ]] || fail "plugin source commit is not immutable"

omarchy_version="$(omarchy version)"
case "$omarchy_version" in
  "$expected_omarchy_version" | "$expected_omarchy_version"-*) ;;
  *) fail "expected Omarchy $expected_omarchy_version, got $omarchy_version" ;;
esac

quickshell_output="$(quickshell --version)"
quickshell_version="$(grep -Eo '[0-9]+\.[0-9]+\.[0-9]+' <<< "$quickshell_output" | head -n 1)"
[[ -n "$quickshell_version" ]] || fail "could not parse the Quickshell version"
lowest_quickshell="$(printf '%s\n' "$expected_quickshell_floor" "$quickshell_version" | sort -V | head -n 1)"
[[ "$lowest_quickshell" == "$expected_quickshell_floor" ]] ||
  fail "Quickshell $quickshell_version is below $expected_quickshell_floor"

router_output="$(wayfinder-router --version)"
[[ "$router_output" == "wayfinder-router $expected_router_version" ]] ||
  fail "expected wayfinder-router $expected_router_version, got $router_output"

source "$plugin_root/scripts/router-lifecycle.sh"
router_load_current >/dev/null || fail "the active Router is not a verified plugin-owned binary"
[[ "$current_router_version" == "$expected_router_version" ]] || fail "Router provenance version drift"
resolved_router_command="$(realpath -m -- "$(command -v wayfinder-router)")"
[[ "$resolved_router_command" == "$(realpath -m -- "$current_router_path")" ]] ||
  fail "PATH resolves wayfinder-router to a different executable than plugin provenance"

omarchy plugin validate "$plugin_root" >/dev/null

assert_plugin_enabled() {
  local plugins
  plugins="$(omarchy plugin list --json)"
  jq -e --arg id "$plugin_id" '
    any(.[];
      .id == $id
      and .enabled == true
      and ((.kinds // []) | index("bar-widget") != null)
      and ((.kinds // []) | index("service") != null))
  ' <<< "$plugins" >/dev/null || fail "Wayfinder is not enabled with both declared plugin kinds"
}

assert_shell_ready() {
  omarchy-shell shell ping >/dev/null || fail "the Omarchy shell IPC is unavailable"
  assert_plugin_enabled
}

assert_router_ready() {
  local service_exec
  [[ "$(systemctl --user is-active wayfinder-router.service)" == "active" ]] ||
    fail "wayfinder-router.service is not active"
  service_exec="$(systemctl --user show --property=ExecStart --value wayfinder-router.service)"
  [[ "$service_exec" == *"path=$current_router_path ;"* ]] ||
    fail "wayfinder-router.service does not execute the plugin-owned binary"
  curl --fail --silent --show-error --max-time 5 "${endpoint%/}/healthz" >/dev/null ||
    fail "the Router health endpoint is unavailable"
}

assert_shell_ready
assert_router_ready

omarchy-restart-shell
shell_ready=false
for _ in $(seq 1 30); do
  if omarchy-shell shell ping >/dev/null 2>&1; then
    shell_ready=true
    break
  fi
  sleep 1
done
[[ "$shell_ready" == true ]] || fail "the Omarchy shell did not recover within 30 seconds"

assert_shell_ready
assert_router_ready

checked_at="$(date -u +'%Y-%m-%dT%H:%M:%SZ')"
evidence="$(jq -n \
  --arg checkedAt "$checked_at" \
  --arg architecture "$(uname -m)" \
  --arg pluginId "$plugin_id" \
  --arg pluginVersion "$manifest_version" \
  --arg sourceCommit "$source_commit" \
  --arg omarchyVersion "$omarchy_version" \
  --arg quickshellVersion "$quickshell_version" \
  --arg routerVersion "$current_router_version" \
  --arg routerTarget "$current_router_target" \
  --arg routerArchiveSha256 "$current_router_archive_sha256" \
  --arg routerBinarySha256 "$current_router_binary_sha256" '
  {
    schemaVersion: 1,
    checkedAt: $checkedAt,
    architecture: $architecture,
    plugin: {
      id: $pluginId,
      version: $pluginVersion,
      sourceCommit: $sourceCommit,
      cleanCheckout: true,
      enabledBeforeAndAfterRestart: true,
      widgetVisible: true,
      firstRunComplete: true
    },
    omarchy: { version: $omarchyVersion, shellRestartSurvived: true },
    quickshell: { version: $quickshellVersion },
    router: {
      version: $routerVersion,
      target: $routerTarget,
      archiveSha256: $routerArchiveSha256,
      binarySha256: $routerBinarySha256,
      pluginOwned: true,
      serviceActiveBeforeAndAfterRestart: true,
      healthBeforeAndAfterRestart: true
    }
  }
')"

if [[ -n "$output_file" ]]; then
  output_parent="$(dirname -- "$output_file")"
  install -d -m 0700 "$output_parent"
  temporary_output="$(mktemp "$output_parent/.wayfinder-native-smoke.XXXXXX")"
  printf '%s\n' "$evidence" > "$temporary_output"
  chmod 0600 "$temporary_output"
  mv -f -- "$temporary_output" "$output_file"
  printf 'Native smoke passed; evidence written to %s\n' "$output_file"
else
  printf '%s\n' "$evidence"
fi
