#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
work_dir="$(mktemp -d)"
trap 'rm -rf -- "$work_dir"' EXIT

case "$(uname -m)" in
  x86_64) target="x86_64-unknown-linux-gnu" ;;
  aarch64 | arm64) target="aarch64-unknown-linux-gnu" ;;
  *) exit 0 ;;
esac

fake_binary="$work_dir/wayfinder-router"
printf '#!/usr/bin/env sh\nprintf "wayfinder-router test\\n"\n' > "$fake_binary"
chmod 0755 "$fake_binary"

for run in one two; do
  DIST_DIR="$work_dir/$run" \
    ROUTER_BINARY="$fake_binary" \
    SOURCE_DATE_EPOCH=1 \
    TARGET="$target" \
    "$repo_root/packaging/linux/package_router.sh" >/dev/null
done

archive_name="wayfinder-router-$target.tar.gz"
(
  cd "$work_dir/one"
  sha256sum --check "$archive_name.sha256"
)
cmp "$work_dir/one/$archive_name" "$work_dir/two/$archive_name"

tar -tzf "$work_dir/one/$archive_name" > "$work_dir/contents"
expected="$work_dir/expected"
printf '%s\n' \
  "wayfinder-router-$target/" \
  "wayfinder-router-$target/LICENSE" \
  "wayfinder-router-$target/NOTICE" \
  "wayfinder-router-$target/wayfinder-router" > "$expected"
cmp "$expected" "$work_dir/contents"

extract_dir="$work_dir/extract"
mkdir "$extract_dir"
tar -xzf "$work_dir/one/$archive_name" -C "$extract_dir"
test "$("$extract_dir/wayfinder-router-$target/wayfinder-router")" = "wayfinder-router test"

if DIST_DIR="$work_dir/bad" ROUTER_BINARY="$fake_binary" \
  TARGET="unsupported-linux-target" \
  "$repo_root/packaging/linux/package_router.sh" >/dev/null 2>&1; then
  printf 'package script accepted a non-native target\n' >&2
  exit 1
fi
