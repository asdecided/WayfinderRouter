#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repository_root="$(cd -- "$script_dir/../.." && pwd)"
mirror_dir="$repository_root/integrations/omarchy-wayfinder"
pin_file="$script_dir/omarchy-wayfinder-commit.txt"
source_dir="${1:-}"

if [[ -z "$source_dir" || ! -d "$source_dir" ]]; then
  printf '%s\n' "usage: check_omarchy_mirror.sh PATH_TO_PINNED_STANDALONE_CHECKOUT" >&2
  exit 2
fi

pinned_commit="$(tr -d '[:space:]' < "$pin_file")"
if [[ ! "$pinned_commit" =~ ^[0-9a-f]{40}$ ]]; then
  printf '%s\n' "The Omarchy plugin pin must be one full lowercase commit SHA." >&2
  exit 1
fi

if [[ "$(realpath -m -- "$source_dir")" == "$(realpath -m -- "$mirror_dir")" ]]; then
  printf '%s\n' "The standalone checkout and in-tree mirror must be different directories." >&2
  exit 1
fi

tree_modes() {
  local directory="$1"
  (
    cd -- "$directory"
    find . -path './.git' -prune -o \( -type f -o -type l \) \
      -printf '%y %P %m %l\n' | LC_ALL=C sort
  )
}

if ! diff --unified --label standalone-modes --label mirror-modes \
  <(tree_modes "$source_dir") <(tree_modes "$mirror_dir"); then
  printf '%s\n' \
    "The in-tree Omarchy mirror has different file types or executable modes." >&2
  exit 1
fi

if ! diff --recursive --brief --exclude=.git -- "$source_dir" "$mirror_dir"; then
  printf '%s\n' \
    "integrations/omarchy-wayfinder does not match standalone commit $pinned_commit." \
    "Sync the complete standalone tree and update the reviewed commit pin together." >&2
  exit 1
fi

printf 'Omarchy mirror matches standalone commit %s.\n' "$pinned_commit"
