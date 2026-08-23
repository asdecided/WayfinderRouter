#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
test_home="$(mktemp -d)"
existing_home="$(mktemp -d)"
trap 'rm -rf -- "$test_home" "$existing_home"' EXIT

install -d -m 0755 "$existing_home/bin"
printf '#!/usr/bin/env sh\nprintf "existing router\\n"\n' > "$existing_home/bin/wayfinder-router"
chmod 0755 "$existing_home/bin/wayfinder-router"
existing_digest="$(sha256sum "$existing_home/bin/wayfinder-router" | cut -d ' ' -f 1)"
HOME="$existing_home" \
XDG_CONFIG_HOME="$existing_home/config" \
WAYFINDER_BIN_DIR="$existing_home/install-target" \
PATH="$existing_home/bin:/usr/bin:/bin" \
  bash "$repo_root/install.sh"
test "$(sha256sum "$existing_home/bin/wayfinder-router" | cut -d ' ' -f 1)" = "$existing_digest"
test ! -e "$existing_home/install-target/wayfinder-router"
test ! -e "$existing_home/.local/state/wayfinder/omarchy-router-install"

HOME="$test_home" \
XDG_CONFIG_HOME="$test_home/config" \
WAYFINDER_BIN_DIR="$test_home/bin" \
PATH="/usr/bin:/bin" \
  bash "$repo_root/install.sh"

router="$test_home/bin/wayfinder-router"
test -x "$router"
"$router" --version | grep -F "2026.8.0"

provenance="$test_home/.local/state/wayfinder/omarchy-router-install"
test -f "$provenance"
grep -F "router_version=2026.8.0" "$provenance"
grep -F "router_path=$router" "$provenance"

HOME="$test_home" \
XDG_CONFIG_HOME="$test_home/config" \
WAYFINDER_BIN_DIR="$test_home/bin" \
PATH="$test_home/bin:/usr/bin:/bin" \
  bash "$repo_root/install.sh" --upgrade-router > "$test_home/upgrade.out"
grep -F "already matches the reviewed plugin pin" "$test_home/upgrade.out"
if HOME="$test_home" \
  XDG_CONFIG_HOME="$test_home/config" \
  PATH="$test_home/bin:/usr/bin:/bin" \
  bash "$repo_root/install.sh" --rollback-router \
    > "$test_home/rollback.out" 2> "$test_home/rollback.err"; then
  printf '%s\n' "rollback succeeded without a last-known-good Router" >&2
  exit 1
fi
grep -F "No verified last-known-good Router" "$test_home/rollback.err"

plugin_dir="$test_home/config/omarchy/plugins/io.github.asdecided.wayfinder"
test -f "$plugin_dir/manifest.json"

cp -- "$router" "$test_home/original-router"
printf '\nmodified after installation\n' >> "$router"
if HOME="$test_home" \
  XDG_CONFIG_HOME="$test_home/config" \
  bash "$repo_root/uninstall.sh" --remove-owned-router \
    > "$test_home/modified.out" 2> "$test_home/modified.err"; then
  printf '%s\n' "uninstaller removed a modified Router binary" >&2
  exit 1
fi
grep -F "binary changed after installation; refusing lifecycle action" "$test_home/modified.err"
mv -- "$test_home/original-router" "$router"

HOME="$test_home" \
XDG_CONFIG_HOME="$test_home/config" \
  bash "$repo_root/uninstall.sh" --remove-owned-router
test ! -e "$router"
test ! -e "$provenance"

printf '%s\n' "Wayfinder native installer test passed"
