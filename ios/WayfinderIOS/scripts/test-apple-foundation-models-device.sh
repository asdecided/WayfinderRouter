#!/bin/bash

set -euo pipefail

if [[ $# -lt 1 || $# -gt 2 ]]; then
  echo "usage: $0 <physical-device-id> [development-team-id]" >&2
  exit 2
fi

device_id="$1"
development_team="${2:-${WAYFINDER_DEVELOPMENT_TEAM:-}}"
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
project_dir="$(cd "${script_dir}/.." && pwd)"

build_settings=()
if [[ -n "${development_team}" ]]; then
  build_settings+=("DEVELOPMENT_TEAM=${development_team}")
fi

xcodebuild test \
  -project "${project_dir}/WayfinderIOS.xcodeproj" \
  -scheme WayfinderIOS \
  -destination "platform=iOS,id=${device_id}" \
  -only-testing:WayfinderIOSTests/AppleFoundationModelsDeviceTests \
  -allowProvisioningUpdates \
  "${build_settings[@]}"
