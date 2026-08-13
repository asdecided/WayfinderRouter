#!/usr/bin/env bash
set -euo pipefail

plugin_id="io.github.asdecided.wayfinder"
wayfinder_rev="9874b08b466822ea6fd0c0875a88950521110997"
source_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
plugin_dir="${XDG_CONFIG_HOME:-$HOME/.config}/omarchy/plugins/$plugin_id"
repo_root="$(cd -- "$source_dir/../.." && pwd)"

runtime_files=(
  manifest.json
  Model.js
  RouteMark.qml
  Service.qml
  BarWidget.qml
)

if [[ "$(realpath -m -- "$source_dir")" != "$(realpath -m -- "$plugin_dir")" ]]; then
  install -d -m 0755 "$plugin_dir"
  for file in "${runtime_files[@]}"; do
    install -m 0644 "$source_dir/$file" "$plugin_dir/$file"
  done
fi

if ! command -v wayfinder-router >/dev/null 2>&1; then
  if ! command -v cargo >/dev/null 2>&1; then
    printf '%s\n' \
      "Wayfinder plugin installed, but the router binary is missing." \
      "Install Rust, then run this installer again to build wayfinder-router."
  elif [[ -f "$repo_root/rust/Cargo.toml" ]]; then
    cargo install --path "$repo_root/rust/crates/wayfinder-cli" --locked
  else
    cargo install \
      --git https://github.com/asdecided/WayfinderRouter.git \
      --rev "$wayfinder_rev" \
      --locked \
      wayfinder-cli
  fi
fi

if command -v omarchy-shell >/dev/null 2>&1; then
  omarchy-shell shell rescanPlugins >/dev/null 2>&1 || true
fi

if command -v omarchy >/dev/null 2>&1; then
  omarchy plugin enable "$plugin_id"
else
  printf '%s\n' "Enable $plugin_id from Omarchy Setup > Plugins."
fi

printf '%s\n' \
  "Wayfinder is available in the Omarchy bar." \
  "Open it and choose Install service to start the localhost gateway."
