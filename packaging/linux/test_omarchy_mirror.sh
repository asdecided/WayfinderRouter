#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repository_root="$(cd -- "$script_dir/../.." && pwd)"
mirror_dir="$repository_root/integrations/omarchy-wayfinder"
temporary_dir="$(mktemp -d)"
trap 'rm -rf -- "$temporary_dir"' EXIT

source_copy="$temporary_dir/standalone"
cp -a -- "$mirror_dir" "$source_copy"
"$script_dir/check_omarchy_mirror.sh" "$source_copy" >/dev/null

readme_mode="$(stat -c '%a' "$source_copy/README.md")"
chmod u+x -- "$source_copy/README.md"
if "$script_dir/check_omarchy_mirror.sh" "$source_copy" >/dev/null 2>&1; then
  printf '%s\n' "The Omarchy mirror check accepted executable-mode drift." >&2
  exit 1
fi
chmod "$readme_mode" -- "$source_copy/README.md"

printf '\nintentional drift\n' >> "$source_copy/README.md"
if "$script_dir/check_omarchy_mirror.sh" "$source_copy" >/dev/null 2>&1; then
  printf '%s\n' "The Omarchy mirror check accepted a drifted source tree." >&2
  exit 1
fi

if "$script_dir/check_omarchy_mirror.sh" "$mirror_dir" >/dev/null 2>&1; then
  printf '%s\n' "The Omarchy mirror check accepted the mirror as its own source." >&2
  exit 1
fi

printf '%s\n' "Omarchy mirror drift-guard test passed."
