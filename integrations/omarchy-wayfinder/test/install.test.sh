#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
test_home="$(mktemp -d)"
existing_home="$(mktemp -d)"
unsupported_home="$(mktemp -d)"
trap 'rm -rf -- "$test_home" "$existing_home" "$unsupported_home"' EXIT

install -d -m 0755 "$existing_home/bin"
printf '#!/usr/bin/env sh\nprintf "existing router\\n"\n' > "$existing_home/bin/wayfinder-router"
printf '#!/usr/bin/env sh\n: > "$HOME/post-install-hook-called"\n' > "$existing_home/bin/omarchy"
cp -- "$existing_home/bin/omarchy" "$existing_home/bin/omarchy-shell"
chmod 0755 "$existing_home/bin/wayfinder-router"
chmod 0755 "$existing_home/bin/omarchy" "$existing_home/bin/omarchy-shell"
existing_digest="$(sha256sum "$existing_home/bin/wayfinder-router" | cut -d ' ' -f 1)"
HOME="$existing_home" \
XDG_CONFIG_HOME="$existing_home/config" \
WAYFINDER_BIN_DIR="$existing_home/install-target" \
PATH="$existing_home/bin:/usr/bin:/bin" \
  bash "$repo_root/install.sh" --bootstrap-router
test "$(sha256sum "$existing_home/bin/wayfinder-router" | cut -d ' ' -f 1)" = "$existing_digest"
test ! -e "$existing_home/install-target/wayfinder-router"
test ! -e "$existing_home/.local/state/wayfinder/omarchy-router-install"
test ! -e "$existing_home/config/omarchy/plugins/io.github.asdecided.wayfinder"
test ! -e "$existing_home/post-install-hook-called"

install -d -m 0755 "$unsupported_home/bin"
printf '#!/usr/bin/env sh\nprintf "riscv64\\n"\n' > "$unsupported_home/bin/uname"
chmod 0755 "$unsupported_home/bin/uname"
if HOME="$unsupported_home" \
  XDG_CONFIG_HOME="$unsupported_home/config" \
  PATH="$unsupported_home/bin:/usr/bin:/bin" \
  bash "$repo_root/install.sh" --bootstrap-router \
    > "$unsupported_home/install.out" 2> "$unsupported_home/install.err"; then
  printf '%s\n' "bootstrap accepted an unsupported architecture" >&2
  exit 1
fi
grep -F "does not publish a Linux binary for architecture riscv64" "$unsupported_home/install.err"
test ! -e "$unsupported_home/config/omarchy/plugins/io.github.asdecided.wayfinder"

HOME="$test_home" \
XDG_CONFIG_HOME="$test_home/config" \
WAYFINDER_BIN_DIR="$test_home/bin" \
PATH="/usr/bin:/bin" \
  bash "$repo_root/install.sh" --bootstrap-router

router="$test_home/bin/wayfinder-router"
test -x "$router"
"$router" --version | grep -F "1.0.0"

provenance="$test_home/.local/state/wayfinder/omarchy-router-install"
test -f "$provenance"
grep -F "router_version=1.0.0" "$provenance"
grep -F "router_path=$router" "$provenance"

plugin_dir="$test_home/config/omarchy/plugins/io.github.asdecided.wayfinder"
test ! -e "$plugin_dir"

HOME="$test_home" \
XDG_CONFIG_HOME="$test_home/config" \
WAYFINDER_BIN_DIR="$test_home/bin" \
PATH="$test_home/bin:/usr/bin:/bin" \
  bash "$repo_root/install.sh"
test -f "$plugin_dir/manifest.json"

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
