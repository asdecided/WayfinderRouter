#!/usr/bin/env bash
set -euo pipefail

plugin_id="io.github.asdecided.wayfinder"

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
  "To remove the service too, run: wayfinder-router service uninstall"
