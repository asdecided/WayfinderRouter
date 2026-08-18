#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
surfacecheck_bin="${SURFACECHECK_BIN:-surfacecheck}"
workspace_version="$(sed -n '/^\[workspace.package\]$/,/^\[/s/^version = "\([^"]*\)"$/\1/p' "$repo_root/rust/Cargo.toml")"

if [[ -z "$workspace_version" ]]; then
  echo "could not read the Rust workspace version" >&2
  exit 1
fi

release_notes="$repo_root/docs/releases/router-v$workspace_version.md"
if [[ ! -f "$release_notes" ]]; then
  echo "missing release notes: $release_notes" >&2
  exit 1
fi

work_dir="$(mktemp -d)"
trap 'rm -rf -- "$work_dir"' EXIT

printf '{"name":"wayfinder-router","version":"%s"}\n' "$workspace_version" > "$work_dir/package.json"

cat > "$work_dir/surfacecheck.toml" <<EOF
[project]
name = "wayfinder-router"
version_source = "package.json"

[[surface]]
name = "Router release notes"
kind = "markdown"
path = "$release_notes"
require = [
  "Wayfinder Router {version} for Linux",
  "wayfinder-router-x86_64-unknown-linux-gnu.tar.gz",
  "wayfinder-router-aarch64-unknown-linux-gnu.tar.gz",
  ".sha256",
  "min-cost",
]

[[surface]]
name = "README Linux release contract"
kind = "markdown"
path = "$repo_root/README.md"
require = ["router-v*", "checksum-verified native Linux", "archives", "x86_64", "aarch64"]
EOF

"$surfacecheck_bin" check --config "$work_dir/surfacecheck.toml" --offline
