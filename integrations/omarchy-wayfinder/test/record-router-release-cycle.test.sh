#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
test_root="$(mktemp -d)"
trap 'rm -rf -- "$test_root"' EXIT

export HOME="$test_root/home"
export XDG_STATE_HOME="$test_root/state"
export PATH="$test_root/bin:$PATH"
install -d -m 0755 "$HOME/bin" "$test_root/bin" "$test_root/candidates"

make_router() {
  local destination="$1"
  local version="$2"
  printf '#!/usr/bin/env bash\nprintf '\''wayfinder-router %s\\n'\''\n' "$version" > "$destination"
  chmod 0755 "$destination"
}

make_router "$HOME/bin/wayfinder-router" 2026.8.1
make_router "$test_root/candidates/wayfinder-router" 1.0.0

plugin_id="io.github.asdecided.wayfinder"
router_provenance_dir="$XDG_STATE_HOME/wayfinder"
router_provenance_file="$router_provenance_dir/omarchy-router-install"
source "$repo_root/scripts/router-lifecycle.sh"
router_write_current_metadata \
  2026.8.1 \
  x86_64-unknown-linux-gnu \
  "$(printf '%064d' 1)" \
  "$(sha256sum "$HOME/bin/wayfinder-router" | cut -d ' ' -f 1)" \
  "$HOME/bin/wayfinder-router"

cat > "$test_root/installer.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
router_version="1.0.0"
plugin_id="io.github.asdecided.wayfinder"
router_provenance_dir="${XDG_STATE_HOME}/wayfinder"
router_provenance_file="$router_provenance_dir/omarchy-router-install"
source "$WAYFINDER_LIFECYCLE_LIBRARY"
case "$1" in
  --upgrade-router)
    router_promote_candidate \
      "$WAYFINDER_LIFECYCLE_CANDIDATE" \
      1.0.0 \
      x86_64-unknown-linux-gnu \
      "$(printf '%064d' 2)"
    ;;
  --rollback-router) router_rollback ;;
  *) exit 2 ;;
esac
EOF
chmod 0755 "$test_root/installer.sh"

cat > "$test_root/bin/systemctl" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF
cat > "$test_root/bin/curl" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' '{"status":"ok"}'
EOF
cat > "$test_root/bin/omarchy" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF
cat > "$test_root/bin/omarchy-shell" <<'EOF'
#!/usr/bin/env bash
[[ "${1:-}" == "shell" && "${2:-}" == "ping" ]]
EOF
chmod 0755 "$test_root/bin/systemctl" "$test_root/bin/curl" \
  "$test_root/bin/omarchy" "$test_root/bin/omarchy-shell"

export WAYFINDER_LIFECYCLE_INSTALLER="$test_root/installer.sh"
export WAYFINDER_LIFECYCLE_LIBRARY="$repo_root/scripts/router-lifecycle.sh"
export WAYFINDER_LIFECYCLE_CANDIDATE="$test_root/candidates/wayfinder-router"
evidence="$HOME/cycle.json"

bash "$repo_root/scripts/record-router-release-cycle.sh" \
  --from 2026.8.1 \
  --to 1.0.0 \
  --evidence "$evidence" \
  >/dev/null

jq -e '
  .schema_version == "wf-omarchy-router-cycle-v1"
  and .sequence == ["2026.8.1", "1.0.0", "2026.8.1", "1.0.0"]
  and .service_restart == "passed"
  and .health == "passed"
  and .omarchy_shell == "passed"
  and .final_version == "1.0.0"
' "$evidence" >/dev/null
[[ "$(stat -c '%a' "$evidence")" == "600" ]]
[[ "$("$HOME/bin/wayfinder-router" --version)" == "wayfinder-router 1.0.0" ]]

if bash "$repo_root/scripts/record-router-release-cycle.sh" \
  --from 2026.8.1 \
  --to 1.0.0 \
  --evidence "$HOME/should-not-exist.json" \
  >/dev/null 2>&1; then
  printf '%s\n' "Lifecycle recorder accepted the wrong starting version." >&2
  exit 1
fi
[[ ! -e "$HOME/should-not-exist.json" ]]

printf '%s\n' "Real Router lifecycle evidence harness tests passed"
