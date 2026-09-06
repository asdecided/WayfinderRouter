#!/usr/bin/env bash
set -euo pipefail

tag="router-v$VERSION"
release_id="$(jq -er '.release.id' "$GITHUB_EVENT_PATH")"
release="$(gh api "repos/$GITHUB_REPOSITORY/releases/$release_id")"
jq -e --arg tag "$tag" '.tag_name == $tag and .draft == false and .immutable != true' <<< "$release" >/dev/null
# Never attach binaries to a tag that moved while they were building.
git fetch --no-tags origin "refs/tags/$tag"
test "$(git rev-parse 'FETCH_HEAD^{commit}')" = "$GITHUB_SHA"

existing_dir="$(mktemp -d)"
trap 'rm -rf "$existing_dir"' EXIT
missing=()
# Verify every existing asset before uploading anything. This supports retrying
# interrupted uploads but never overwrites a different published download.
for asset in "$RUNNER_TEMP"/router-dist/*; do
  name="$(basename "$asset")"
  asset_id="$(jq -r --arg name "$name" '.assets[] | select(.name == $name) | .id' <<< "$release")"
  if [[ -n "$asset_id" ]]; then
    gh api -H 'Accept: application/octet-stream' \
      "repos/$GITHUB_REPOSITORY/releases/assets/$asset_id" > "$existing_dir/$name"
    cmp "$asset" "$existing_dir/$name" || {
      echo "::error::Existing release asset differs: $name; refusing replacement"
      exit 1
    }
  else
    missing+=("$asset")
  fi
done
if (( ${#missing[@]} )); then
  gh release upload "$tag" "${missing[@]}"
fi
url="$(jq -r '.html_url' <<< "$release")"
printf '### Router %s downloads ready\n\n[Download release](%s)\n\nSource: `%s`\n' "$VERSION" "$url" "$GITHUB_SHA" >> "$GITHUB_STEP_SUMMARY"
