#!/usr/bin/env bash
set -euo pipefail

plugin_id="io.github.asdecided.wayfinder"
source_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
router_provenance_dir="${XDG_STATE_HOME:-$HOME/.local/state}/wayfinder"
router_provenance_file="${XDG_STATE_HOME:-$HOME/.local/state}/wayfinder/omarchy-router-install"
source "$source_dir/scripts/router-lifecycle.sh"

if [[ "${1:-}" == "--remove-owned-router" ]]; then
  router_acquire_lock
  router_recover_transaction
  if [[ ! -f "$router_provenance_file" ]]; then
    printf '%s\n' "No plugin-owned Wayfinder Router installation was recorded."
  else
    router_load_current
    rm -f -- "$current_router_path"
    printf '%s\n' "Removed the checksum-matched plugin-owned Router binary."
    rm -f -- "$router_provenance_file"
    rm -f -- "$router_backup_binary" "$router_backup_metadata" "$router_transaction_file"
    rmdir --ignore-fail-on-non-empty -- "$router_backup_dir" 2>/dev/null || true
  fi
elif [[ $# -gt 0 ]]; then
  printf 'usage: %s [--remove-owned-router]\n' "$0" >&2
  exit 2
fi

if command -v omarchy >/dev/null 2>&1; then
  omarchy plugin remove "$plugin_id"
else
  printf '%s\n' \
    "Remove $plugin_id from Omarchy Setup > Plugins." \
    "The Wayfinder service is intentionally left installed."
fi

printf '%s\n' \
  "The plugin has been removed." \
  "Wayfinder remains available to other applications." \
  "To remove the service too, run: wayfinder-router service uninstall" \
  "A plugin-installed binary is removed only with: ./uninstall.sh --remove-owned-router"
