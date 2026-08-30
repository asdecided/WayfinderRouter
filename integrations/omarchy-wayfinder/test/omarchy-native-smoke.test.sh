#!/usr/bin/env bash
set -euo pipefail

repository_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
temporary_dir="$(mktemp -d)"
trap 'rm -rf -- "$temporary_dir"' EXIT

export HOME="$temporary_dir/home"
export XDG_CONFIG_HOME="$HOME/.config"
export XDG_STATE_HOME="$HOME/.local/state"
mock_bin="$temporary_dir/mock-bin"
plugin_dir="$XDG_CONFIG_HOME/omarchy/plugins/io.github.asdecided.wayfinder"
router_path="$HOME/.local/bin/wayfinder-router"
router_provenance_dir="$XDG_STATE_HOME/wayfinder"
evidence_file="$temporary_dir/evidence/native-smoke.json"
mkdir -p "$mock_bin" "$plugin_dir" "$(dirname -- "$router_path")" "$router_provenance_dir"
cp -a "$repository_root/." "$plugin_dir/"
rm -rf -- "$plugin_dir/.git"
git -C "$plugin_dir" init -q
git -C "$plugin_dir" config user.name "Wayfinder smoke test"
git -C "$plugin_dir" config user.email "smoke@example.invalid"
git -C "$plugin_dir" add .
git -C "$plugin_dir" commit -qm "fixture"

cat > "$router_path" <<'ROUTER'
#!/usr/bin/env bash
printf '%s\n' "wayfinder-router 2026.8.1"
ROUTER
chmod 0755 "$router_path"
router_binary_sha256="$(sha256sum "$router_path" | cut -d ' ' -f 1)"
mkdir -p "$router_provenance_dir"
cat > "$router_provenance_dir/omarchy-router-install" <<EOF
schema_version=1
plugin_id=io.github.asdecided.wayfinder
role=current
router_version=2026.8.1
router_target=x86_64-unknown-linux-gnu
archive_sha256=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
binary_sha256=$router_binary_sha256
router_path=$router_path
EOF

cat > "$mock_bin/omarchy" <<'MOCK'
#!/usr/bin/env bash
case "$1 $2 ${3:-}" in
  "version  ") printf '%s\n' "4.0.0.alpha-1" ;;
  "plugin validate "*) exit 0 ;;
  "plugin list --json")
    printf '%s\n' '[{"id":"io.github.asdecided.wayfinder","enabled":true,"kinds":["service","bar-widget"]}]'
    ;;
  *) exit 1 ;;
esac
MOCK
cat > "$mock_bin/quickshell" <<'MOCK'
#!/usr/bin/env bash
printf '%s\n' "quickshell 0.3.1"
MOCK
cat > "$mock_bin/omarchy-shell" <<'MOCK'
#!/usr/bin/env bash
[[ "$1 $2" == "shell ping" ]]
MOCK
cat > "$mock_bin/omarchy-restart-shell" <<'MOCK'
#!/usr/bin/env bash
: > "$WAYFINDER_TEST_RESTART_MARKER"
MOCK
cat > "$mock_bin/systemctl" <<'MOCK'
#!/usr/bin/env bash
case "$1 $2 $3 ${4:-}" in
  "--user is-active wayfinder-router.service ") printf '%s\n' active ;;
  "--user show --property=ExecStart --value") printf '{ path=%s ; argv[]=%s ; }\n' "$WAYFINDER_TEST_ROUTER_PATH" "$WAYFINDER_TEST_ROUTER_PATH" ;;
  *) exit 1 ;;
esac
MOCK
cat > "$mock_bin/curl" <<'MOCK'
#!/usr/bin/env bash
exit 0
MOCK
chmod 0755 "$mock_bin"/*

WAYFINDER_TEST_RESTART_MARKER="$temporary_dir/restarted" \
WAYFINDER_TEST_ROUTER_PATH="$router_path" \
PATH="$mock_bin:$(dirname -- "$router_path"):$PATH" \
  "$plugin_dir/scripts/omarchy-native-smoke.sh" \
    --restart-shell \
    --confirm-widget-visible \
    --confirm-first-run-complete \
    --output "$evidence_file"

[[ -f "$temporary_dir/restarted" ]] || {
  printf '%s\n' "native smoke did not restart the Omarchy shell" >&2
  exit 1
}

jq -e '
  .schemaVersion == 1
  and .plugin.id == "io.github.asdecided.wayfinder"
  and .plugin.version == "0.3.2"
  and .plugin.cleanCheckout == true
  and .plugin.enabledBeforeAndAfterRestart == true
  and .plugin.widgetVisible == true
  and .omarchy.shellRestartSurvived == true
  and .router.version == "2026.8.1"
  and .router.pluginOwned == true
  and .router.healthBeforeAndAfterRestart == true
' "$evidence_file" >/dev/null

if WAYFINDER_TEST_ROUTER_PATH="$router_path" \
  PATH="$mock_bin:$(dirname -- "$router_path"):$PATH" \
  "$plugin_dir/scripts/omarchy-native-smoke.sh" \
    --restart-shell --confirm-first-run-complete >/dev/null 2>&1; then
  printf '%s\n' "native smoke accepted missing widget attestation" >&2
  exit 1
fi

if WAYFINDER_SMOKE_ENDPOINT="https://example.com" \
  WAYFINDER_TEST_ROUTER_PATH="$router_path" \
  PATH="$mock_bin:$(dirname -- "$router_path"):$PATH" \
  "$plugin_dir/scripts/omarchy-native-smoke.sh" \
    --restart-shell --confirm-widget-visible --confirm-first-run-complete >/dev/null 2>&1; then
  printf '%s\n' "native smoke accepted a non-loopback endpoint" >&2
  exit 1
fi

if WAYFINDER_TEST_RESTART_MARKER="$temporary_dir/restarted-wrong-unit" \
  WAYFINDER_TEST_ROUTER_PATH="$temporary_dir/not-the-owned-router" \
  PATH="$mock_bin:$(dirname -- "$router_path"):$PATH" \
  "$plugin_dir/scripts/omarchy-native-smoke.sh" \
    --restart-shell --confirm-widget-visible --confirm-first-run-complete >/dev/null 2>&1; then
  printf '%s\n' "native smoke accepted a service using another Router binary" >&2
  exit 1
fi

printf '\nlocal drift\n' >> "$plugin_dir/README.md"
if WAYFINDER_TEST_RESTART_MARKER="$temporary_dir/restarted-dirty" \
  WAYFINDER_TEST_ROUTER_PATH="$router_path" \
  PATH="$mock_bin:$(dirname -- "$router_path"):$PATH" \
  "$plugin_dir/scripts/omarchy-native-smoke.sh" \
    --restart-shell --confirm-widget-visible --confirm-first-run-complete >/dev/null 2>&1; then
  printf '%s\n' "native smoke accepted a dirty plugin checkout" >&2
  exit 1
fi
git -C "$plugin_dir" restore README.md

printf '\n# modified\n' >> "$router_path"
if WAYFINDER_TEST_RESTART_MARKER="$temporary_dir/restarted-tampered" \
  WAYFINDER_TEST_ROUTER_PATH="$router_path" \
  PATH="$mock_bin:$(dirname -- "$router_path"):$PATH" \
  "$plugin_dir/scripts/omarchy-native-smoke.sh" \
    --restart-shell --confirm-widget-visible --confirm-first-run-complete >/dev/null 2>&1; then
  printf '%s\n' "native smoke accepted a modified plugin-owned Router" >&2
  exit 1
fi

printf '%s\n' "Wayfinder native Omarchy smoke harness tests passed"
