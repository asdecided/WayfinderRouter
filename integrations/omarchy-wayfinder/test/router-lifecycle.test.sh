#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
test_root="$(mktemp -d)"
trap 'rm -rf -- "$test_root"' EXIT

export HOME="$test_root/home"
export XDG_STATE_HOME="$test_root/state"
plugin_id="io.github.asdecided.wayfinder"
router_provenance_dir="$XDG_STATE_HOME/wayfinder"
router_provenance_file="$router_provenance_dir/omarchy-router-install"
source "$repo_root/scripts/router-lifecycle.sh"

make_router() {
  local destination="$1"
  local version="$2"
  local marker="$3"
  install -d -m 0755 "$(dirname -- "$destination")"
  printf '#!/usr/bin/env bash\nprintf '\''wayfinder-router %s\\n'\''\n# %s\n' \
    "$version" "$marker" > "$destination"
  chmod 0755 "$destination"
}

assert_version() {
  local binary="$1"
  local version="$2"
  [[ "$("$binary" --version)" == "wayfinder-router $version" ]]
}

archive_v1="$(printf '%064d' 1)"
archive_v2="$(printf '%064d' 2)"
archive_v3="$(printf '%064d' 3)"
archive_v4="$(printf '%064d' 4)"
router_path="$HOME/bin/wayfinder-router"
candidate_v1="$test_root/candidates/router-v1"
candidate_v2="$test_root/candidates/router-v2"
candidate_v3="$test_root/candidates/router-v3"
candidate_v4="$test_root/candidates/router-v4"
make_router "$candidate_v1" 2026.8.0 v1
make_router "$candidate_v2" 2026.8.1 v2
make_router "$candidate_v3" 2026.8.2 v3
make_router "$candidate_v4" 2026.8.3 v4
install -d -m 0755 "$(dirname -- "$router_path")"
install -m 0755 "$candidate_v1" "$router_path"
router_write_current_metadata 2026.8.0 x86_64-unknown-linux-gnu "$archive_v1" \
  "$(sha256sum "$router_path" | cut -d ' ' -f 1)" "$router_path"

router_promote_candidate "$candidate_v2" 2026.8.1 x86_64-unknown-linux-gnu "$archive_v2"
assert_version "$router_path" 2026.8.1
assert_version "$router_backup_binary" 2026.8.0
[[ "$(router_metadata_value "$router_provenance_file" router_version)" == "2026.8.1" ]]
[[ "$(router_metadata_value "$router_backup_metadata" router_version)" == "2026.8.0" ]]
[[ ! -e "$router_transaction_file" ]]

router_rollback
assert_version "$router_path" 2026.8.0
assert_version "$router_backup_binary" 2026.8.1
[[ "$(router_metadata_value "$router_provenance_file" router_version)" == "2026.8.0" ]]

printf '%s\n' "tampered" >> "$router_path"
if router_promote_candidate "$candidate_v3" 2026.8.2 x86_64-unknown-linux-gnu "$archive_v3" \
    >/dev/null 2>&1; then
  printf '%s\n' "promotion accepted a modified active Router" >&2
  exit 1
fi
install -m 0755 "$candidate_v1" "$router_path"

router_lifecycle_hook() {
  [[ "$1" != "after-promotion" ]] || return 75
}
if router_promote_candidate "$candidate_v3" 2026.8.2 x86_64-unknown-linux-gnu "$archive_v3" \
    >/dev/null 2>&1; then
  printf '%s\n' "interrupted promotion unexpectedly completed" >&2
  exit 1
fi
assert_version "$router_path" 2026.8.2
[[ -f "$router_transaction_file" ]]
[[ "$(router_metadata_value "$router_provenance_file" router_version)" == "2026.8.0" ]]
router_lifecycle_hook() { :; }
router_recover_transaction >/dev/null
assert_version "$router_path" 2026.8.2
[[ "$(router_metadata_value "$router_provenance_file" router_version)" == "2026.8.2" ]]
[[ ! -e "$router_transaction_file" ]]

router_rollback >/dev/null
assert_version "$router_path" 2026.8.0

router_lifecycle_hook() {
  [[ "$1" != "after-transaction" ]] || return 76
}
if router_promote_candidate "$candidate_v4" 2026.8.3 x86_64-unknown-linux-gnu "$archive_v4" \
    >/dev/null 2>&1; then
  printf '%s\n' "pre-promotion interruption unexpectedly completed" >&2
  exit 1
fi
assert_version "$router_path" 2026.8.0
[[ -f "$router_transaction_file" ]]
router_lifecycle_hook() { :; }
router_recover_transaction >/dev/null
assert_version "$router_path" 2026.8.0
[[ "$(router_metadata_value "$router_provenance_file" router_version)" == "2026.8.0" ]]
[[ ! -e "$router_transaction_file" ]]

rm -f -- "$router_backup_binary" "$router_backup_metadata"
if router_rollback >/dev/null 2>&1; then
  printf '%s\n' "rollback succeeded without a verified last-known-good Router" >&2
  exit 1
fi

router_load_current
victim="$HOME/do-not-delete"
printf '%s\n' "owned by the user" > "$victim"
router_write_transaction 2026.8.3 x86_64-unknown-linux-gnu "$archive_v4" \
  "$(sha256sum "$candidate_v4" | cut -d ' ' -f 1)" "$victim"
if router_recover_transaction >/dev/null 2>&1; then
  printf '%s\n' "recovery accepted a candidate path outside the promotion boundary" >&2
  exit 1
fi
[[ -f "$victim" ]]
rm -f -- "$router_transaction_file"

HOME="$HOME" XDG_STATE_HOME="$XDG_STATE_HOME" \
  bash "$repo_root/uninstall.sh" --remove-owned-router >/dev/null
[[ ! -e "$router_path" ]]
[[ ! -e "$router_provenance_file" ]]
[[ ! -e "$router_backup_dir" ]]

printf '%s\n' "Wayfinder Router lifecycle tests passed"
