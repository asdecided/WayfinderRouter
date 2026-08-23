#!/usr/bin/env bash
set -euo pipefail

plugin_id="io.github.asdecided.wayfinder"
router_provenance_file="${XDG_STATE_HOME:-$HOME/.local/state}/wayfinder/omarchy-router-install"

if [[ "${1:-}" == "--remove-owned-router" ]]; then
  if [[ ! -f "$router_provenance_file" ]]; then
    printf '%s\n' "No plugin-owned Wayfinder Router installation was recorded."
  else
    recorded_schema_version="$(sed -n 's/^schema_version=//p' "$router_provenance_file")"
    recorded_plugin_id="$(sed -n 's/^plugin_id=//p' "$router_provenance_file")"
    recorded_router_path="$(sed -n 's/^router_path=//p' "$router_provenance_file")"
    recorded_binary_sha256="$(sed -n 's/^binary_sha256=//p' "$router_provenance_file")"
    resolved_home="$(realpath -m -- "$HOME")"
    resolved_router_path="$(realpath -m -- "$recorded_router_path")"

    if [[ "$recorded_schema_version" != "1" || "$recorded_plugin_id" != "$plugin_id" || -z "$recorded_binary_sha256" ]]; then
      printf '%s\n' "Wayfinder Router provenance is incomplete; refusing removal." >&2
      exit 1
    fi
    case "$resolved_router_path" in
      "$resolved_home"/wayfinder-router | "$resolved_home"/*/wayfinder-router) ;;
      *)
        printf '%s\n' "Recorded Router path is outside the user-owned installation boundary." >&2
        exit 1
        ;;
    esac

    if [[ ! -f "$resolved_router_path" || -L "$resolved_router_path" ]]; then
      printf '%s\n' "The recorded plugin-owned Router is no longer present."
    else
      current_binary_sha256="$(sha256sum "$resolved_router_path" | cut -d ' ' -f 1)"
      if [[ "$current_binary_sha256" != "$recorded_binary_sha256" ]]; then
        printf '%s\n' "The Router binary changed after installation; refusing removal." >&2
        exit 1
      fi
      rm -f -- "$resolved_router_path"
      printf '%s\n' "Removed the checksum-matched plugin-owned Router binary."
    fi
    rm -f -- "$router_provenance_file"
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
