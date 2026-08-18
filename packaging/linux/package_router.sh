#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
dist_dir="${DIST_DIR:-$repo_root/dist}"
host_arch="$(uname -m)"

case "$host_arch" in
  x86_64) default_target="x86_64-unknown-linux-gnu" ;;
  aarch64 | arm64) default_target="aarch64-unknown-linux-gnu" ;;
  *)
    printf 'unsupported Linux release architecture: %s\n' "$host_arch" >&2
    exit 1
    ;;
esac

target="${TARGET:-$default_target}"
if [[ "$target" != "$default_target" ]]; then
  printf 'release builds must be native: host %s requires target %s, got %s\n' \
    "$host_arch" "$default_target" "$target" >&2
  exit 1
fi

package_name="wayfinder-router-$target"
archive="$dist_dir/$package_name.tar.gz"
checksum="$archive.sha256"
source_date_epoch="${SOURCE_DATE_EPOCH:-$(git -C "$repo_root" show -s --format=%ct HEAD)}"
if [[ ! "$source_date_epoch" =~ ^[0-9]+$ ]]; then
  printf 'SOURCE_DATE_EPOCH must be an unsigned integer\n' >&2
  exit 1
fi

if [[ -n "${ROUTER_BINARY:-}" ]]; then
  router_binary="$ROUTER_BINARY"
else
  command -v cargo >/dev/null 2>&1 || {
    printf 'cargo is required to build the Router release\n' >&2
    exit 1
  }
  CARGO_INCREMENTAL=0 SOURCE_DATE_EPOCH="$source_date_epoch" \
    cargo build \
      --manifest-path "$repo_root/rust/Cargo.toml" \
      --package wayfinder-cli \
      --bin wayfinder-router \
      --target "$target" \
      --release \
      --locked
  router_binary="$repo_root/rust/target/$target/release/wayfinder-router"
fi

if [[ ! -f "$router_binary" || ! -x "$router_binary" ]]; then
  printf 'Router binary is missing or not executable: %s\n' "$router_binary" >&2
  exit 1
fi

work_dir="$(mktemp -d)"
trap 'rm -rf -- "$work_dir"' EXIT
stage_dir="$work_dir/$package_name"
install -d -m 0755 "$stage_dir" "$dist_dir"
install -m 0755 "$router_binary" "$stage_dir/wayfinder-router"
install -m 0644 "$repo_root/LICENSE" "$stage_dir/LICENSE"
install -m 0644 "$repo_root/NOTICE" "$stage_dir/NOTICE"
find "$stage_dir" -exec touch -h -d "@$source_date_epoch" {} +

tar \
  --sort=name \
  --format=gnu \
  --owner=0 \
  --group=0 \
  --numeric-owner \
  --mtime="@$source_date_epoch" \
  -cf - \
  -C "$work_dir" \
  "$package_name" | gzip -n > "$archive"

(
  cd -- "$dist_dir"
  sha256sum "$(basename -- "$archive")" > "$(basename -- "$checksum")"
  sha256sum --check "$(basename -- "$checksum")"
)

printf '%s\n' "$archive" "$checksum"
