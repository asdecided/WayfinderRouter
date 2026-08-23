#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
plugin_root="$(cd -- "$script_dir/.." && pwd)"
omarchy_source="${1:-}"

if [[ -z "$omarchy_source" || ! -d "$omarchy_source/.git" ]]; then
  printf '%s\n' "usage: validate-omarchy-contract.sh PATH_TO_PINNED_OMARCHY_CHECKOUT" >&2
  exit 2
fi

read_compatibility() {
  node -e 'const c=require(process.argv[1]); console.log(process.argv.slice(2).reduce((v,k)=>v[k], c))' \
    "$plugin_root/compatibility.json" "$@"
}

expected_commit="$(read_compatibility omarchy commit)"
expected_version="$(read_compatibility omarchy version)"
quickshell_contract_commit="$(read_compatibility quickshell contractCommit)"
quickshell_package="$(read_compatibility quickshell package)"

actual_commit="$(git -C "$omarchy_source" rev-parse HEAD)"
[[ "$actual_commit" == "$expected_commit" ]] || {
  printf 'expected Omarchy %s, got %s\n' "$expected_commit" "$actual_commit" >&2
  exit 1
}
[[ "$(tr -d '[:space:]' < "$omarchy_source/version")" == "$expected_version" ]] || {
  printf 'the pinned Omarchy source does not report version %s\n' "$expected_version" >&2
  exit 1
}
git -C "$omarchy_source" merge-base --is-ancestor "$quickshell_contract_commit" HEAD || {
  printf '%s\n' "the pinned Omarchy source predates the reviewed Quickshell contract" >&2
  exit 1
}
grep -Fx "$quickshell_package" "$omarchy_source/install/omarchy-base.packages" >/dev/null || {
  printf 'the pinned Omarchy package set does not include %s\n' "$quickshell_package" >&2
  exit 1
}

LC_ALL=C bash "$omarchy_source/bin/omarchy-plugin-validate" "$plugin_root"
printf 'validated Wayfinder against Omarchy %s at %s\n' "$expected_version" "$expected_commit"
