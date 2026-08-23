#!/usr/bin/env bash
set -euo pipefail

plugin_id="io.github.asdecided.wayfinder"
router_version="2026.8.0"
router_release_tag="router-v${router_version}"
router_release_base_url="https://github.com/asdecided/WayfinderRouter/releases/download/${router_release_tag}"
router_sha256_x86_64="7ed9f67e244aef14b3014d2da96bbfe23cefa532e09f382d671bc23f9a430cd6"
router_sha256_aarch64="ac8f1b76bde7e6191bc562df4ebab0cbb3eddb2a812667e4f27ffb0e013814d9"
source_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
plugin_dir="${XDG_CONFIG_HOME:-$HOME/.config}/omarchy/plugins/$plugin_id"
router_bin_dir="${WAYFINDER_BIN_DIR:-$HOME/.local/bin}"
router_provenance_dir="${XDG_STATE_HOME:-$HOME/.local/state}/wayfinder"
router_provenance_file="$router_provenance_dir/omarchy-router-install"

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
  for command_name in curl tar sha256sum; do
    command -v "$command_name" >/dev/null 2>&1 || {
      printf 'Wayfinder installation requires %s.\n' "$command_name" >&2
      exit 1
    }
  done

  case "$(uname -m)" in
    x86_64)
      router_target="x86_64-unknown-linux-gnu"
      router_sha256="$router_sha256_x86_64"
      ;;
    aarch64 | arm64)
      router_target="aarch64-unknown-linux-gnu"
      router_sha256="$router_sha256_aarch64"
      ;;
    *)
      printf 'Wayfinder does not publish a Linux binary for architecture %s.\n' "$(uname -m)" >&2
      exit 1
      ;;
  esac

  resolved_home="$(realpath -m -- "$HOME")"
  resolved_router_bin_dir="$(realpath -m -- "$router_bin_dir")"
  case "$resolved_router_bin_dir/" in
    "$resolved_home"/*) ;;
    *)
      printf '%s\n' "Wayfinder must be installed to a user-owned path inside HOME." >&2
      exit 1
      ;;
  esac

  router_package="wayfinder-router-${router_target}"
  router_archive="${router_package}.tar.gz"
  router_tmp_dir="$(mktemp -d)"
  trap 'rm -rf -- "$router_tmp_dir"' EXIT

  curl \
    --fail \
    --location \
    --proto '=https' \
    --tlsv1.2 \
    --output "$router_tmp_dir/$router_archive" \
    "$router_release_base_url/$router_archive"

  (
    cd -- "$router_tmp_dir"
    printf '%s  %s\n' "$router_sha256" "$router_archive" | sha256sum --check --strict
  )

  mapfile -t router_entries < <(tar -tzf "$router_tmp_dir/$router_archive")
  expected_entries=(
    "$router_package/"
    "$router_package/LICENSE"
    "$router_package/NOTICE"
    "$router_package/wayfinder-router"
  )
  if [[ "${router_entries[*]}" != "${expected_entries[*]}" ]]; then
    printf '%s\n' "Wayfinder release archive has an unexpected layout." >&2
    exit 1
  fi

  install -d -m 0755 "$router_tmp_dir/extracted" "$router_bin_dir"
  tar -xzf "$router_tmp_dir/$router_archive" -C "$router_tmp_dir/extracted"
  router_binary="$router_tmp_dir/extracted/$router_package/wayfinder-router"
  if [[ ! -f "$router_binary" || -L "$router_binary" || ! -x "$router_binary" ]]; then
    printf '%s\n' "Wayfinder release archive does not contain the expected executable." >&2
    exit 1
  fi
  install -m 0755 "$router_binary" "$router_bin_dir/wayfinder-router"
  "$router_bin_dir/wayfinder-router" --version

  installed_binary_sha256="$(sha256sum "$router_bin_dir/wayfinder-router" | cut -d ' ' -f 1)"
  install -d -m 0700 "$router_provenance_dir"
  provenance_tmp="$router_provenance_file.tmp.$$"
  (
    umask 077
    printf '%s\n' \
      "schema_version=1" \
      "plugin_id=$plugin_id" \
      "router_version=$router_version" \
      "router_target=$router_target" \
      "archive_sha256=$router_sha256" \
      "binary_sha256=$installed_binary_sha256" \
      "router_path=$resolved_router_bin_dir/wayfinder-router" \
      > "$provenance_tmp"
  )
  mv -- "$provenance_tmp" "$router_provenance_file"
else
  printf '%s\n' "Using the existing wayfinder-router executable."
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
  "Open it and choose Set up Wayfinder to create a local policy and start the user service."
