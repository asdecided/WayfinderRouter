#!/usr/bin/env bash

# Sourced by install.sh and the lifecycle test. The caller supplies:
# plugin_id, router_provenance_dir, router_provenance_file.

router_backup_dir="$router_provenance_dir/omarchy-router-last-known-good"
router_backup_binary="$router_backup_dir/wayfinder-router"
router_backup_metadata="$router_backup_dir/provenance"
router_transaction_file="$router_provenance_dir/omarchy-router-transaction"
router_lock_file="$router_provenance_dir/omarchy-router-lifecycle.lock"

router_acquire_lock() {
  command -v flock >/dev/null 2>&1 || {
    printf '%s\n' "Router lifecycle actions require flock from util-linux." >&2
    return 1
  }
  install -d -m 0700 "$router_provenance_dir"
  [[ ! -L "$router_lock_file" ]] || {
    printf '%s\n' "Router lifecycle lock path must not be a symlink." >&2
    return 1
  }
  exec {router_lock_fd}> "$router_lock_file"
  flock -w 10 "$router_lock_fd" || {
    printf '%s\n' "Another Wayfinder Router lifecycle action is still running." >&2
    return 1
  }
}

router_metadata_value() {
  local file="$1"
  local key="$2"
  sed -n "s/^${key}=//p" "$file"
}

router_require_safe_value() {
  local label="$1"
  local value="$2"
  if [[ -z "$value" || "$value" == *$'\n'* || "$value" == *$'\r'* ]]; then
    printf 'invalid %s in Router lifecycle metadata\n' "$label" >&2
    return 1
  fi
}

router_require_release_identity() {
  local version="$1"
  local target="$2"
  [[ "$version" =~ ^[0-9]{4}\.[0-9]+\.[0-9]+$ ]] || {
    printf 'invalid Router release version: %s\n' "$version" >&2
    return 1
  }
  [[ "$target" =~ ^(x86_64|aarch64)-unknown-linux-gnu$ ]] || {
    printf 'invalid Router release target: %s\n' "$target" >&2
    return 1
  }
}

router_require_user_path() {
  local path="$1"
  local resolved_home resolved_path
  resolved_home="$(realpath -m -- "$HOME")"
  resolved_path="$(realpath -m -- "$path")"
  case "$resolved_path" in
    "$resolved_home"/*) ;;
    *)
      printf 'Router path is outside the user-owned installation boundary: %s\n' "$resolved_path" >&2
      return 1
      ;;
  esac
}

router_write_metadata() {
  local destination="$1"
  local role="$2"
  local version="$3"
  local target="$4"
  local archive_sha256="$5"
  local binary_sha256="$6"
  local router_path="$7"

  for field in "$role" "$version" "$target" "$archive_sha256" "$binary_sha256" "$router_path"; do
    router_require_safe_value "metadata value" "$field" || return $?
  done
  router_require_release_identity "$version" "$target" || return $?
  (
    umask 077
    printf '%s\n' \
      "schema_version=1" \
      "plugin_id=$plugin_id" \
      "role=$role" \
      "router_version=$version" \
      "router_target=$target" \
      "archive_sha256=$archive_sha256" \
      "binary_sha256=$binary_sha256" \
      "router_path=$router_path" \
      > "$destination"
  )
}

router_write_current_metadata() {
  local version="$1"
  local target="$2"
  local archive_sha256="$3"
  local binary_sha256="$4"
  local router_path="$5"
  local temporary

  install -d -m 0700 "$router_provenance_dir"
  temporary="$(mktemp "$router_provenance_dir/.omarchy-router-provenance.XXXXXX")"
  router_write_metadata "$temporary" current "$version" "$target" "$archive_sha256" \
    "$binary_sha256" "$router_path" || {
      rm -f -- "$temporary"
      return 1
    }
  mv -f -- "$temporary" "$router_provenance_file"
}

router_load_current() {
  if [[ ! -f "$router_provenance_file" || -L "$router_provenance_file" ]]; then
    printf '%s\n' "No plugin-owned Wayfinder Router installation was recorded." >&2
    return 1
  fi

  current_router_schema="$(router_metadata_value "$router_provenance_file" schema_version)"
  current_router_plugin="$(router_metadata_value "$router_provenance_file" plugin_id)"
  current_router_version="$(router_metadata_value "$router_provenance_file" router_version)"
  current_router_target="$(router_metadata_value "$router_provenance_file" router_target)"
  current_router_archive_sha256="$(router_metadata_value "$router_provenance_file" archive_sha256)"
  current_router_binary_sha256="$(router_metadata_value "$router_provenance_file" binary_sha256)"
  current_router_path="$(router_metadata_value "$router_provenance_file" router_path)"

  [[ "$current_router_schema" == "1" && "$current_router_plugin" == "$plugin_id" ]] || {
    printf '%s\n' "Wayfinder Router provenance is incomplete or belongs to another installer." >&2
    return 1
  }
  for field in "$current_router_version" "$current_router_target" "$current_router_archive_sha256" \
    "$current_router_binary_sha256" "$current_router_path"; do
    router_require_safe_value "current provenance" "$field" || return $?
  done
  router_require_release_identity "$current_router_version" "$current_router_target" || return $?
  [[ "$current_router_archive_sha256" =~ ^[0-9a-f]{64}$ ]] || {
    printf '%s\n' "Current Router archive provenance is invalid." >&2
    return 1
  }
  [[ "$current_router_binary_sha256" =~ ^[0-9a-f]{64}$ ]] || {
    printf '%s\n' "Current Router binary provenance is invalid." >&2
    return 1
  }
  router_require_user_path "$current_router_path" || return $?
  if [[ ! -f "$current_router_path" || -L "$current_router_path" || ! -x "$current_router_path" ]]; then
    printf '%s\n' "The recorded plugin-owned Router is missing, linked, or not executable." >&2
    return 1
  fi
  local actual_sha256
  actual_sha256="$(sha256sum "$current_router_path" | cut -d ' ' -f 1)"
  [[ "$actual_sha256" == "$current_router_binary_sha256" ]] || {
    printf '%s\n' "The Router binary changed after installation; refusing lifecycle action." >&2
    return 1
  }
}

router_write_transaction() {
  local candidate_version="$1"
  local candidate_target="$2"
  local candidate_archive_sha256="$3"
  local candidate_binary_sha256="$4"
  local candidate_temp_path="$5"
  local temporary

  temporary="$(mktemp "$router_provenance_dir/.omarchy-router-transaction.XXXXXX")"
  (
    umask 077
    printf '%s\n' \
      "schema_version=1" \
      "plugin_id=$plugin_id" \
      "router_path=$current_router_path" \
      "old_version=$current_router_version" \
      "old_target=$current_router_target" \
      "old_archive_sha256=$current_router_archive_sha256" \
      "old_binary_sha256=$current_router_binary_sha256" \
      "candidate_version=$candidate_version" \
      "candidate_target=$candidate_target" \
      "candidate_archive_sha256=$candidate_archive_sha256" \
      "candidate_binary_sha256=$candidate_binary_sha256" \
      "candidate_temp_path=$candidate_temp_path" \
      > "$temporary"
  )
  mv -f -- "$temporary" "$router_transaction_file"
}

router_recover_transaction() {
  [[ -e "$router_transaction_file" ]] || return 0
  if [[ ! -f "$router_transaction_file" || -L "$router_transaction_file" ]]; then
    printf '%s\n' "Router transaction state is not a regular file; refusing recovery." >&2
    return 1
  fi

  local schema transaction_plugin router_path old_version old_target old_archive_sha256
  local old_binary_sha256 candidate_version candidate_target candidate_archive_sha256
  local candidate_binary_sha256 candidate_temp_path actual_sha256
  schema="$(router_metadata_value "$router_transaction_file" schema_version)"
  transaction_plugin="$(router_metadata_value "$router_transaction_file" plugin_id)"
  router_path="$(router_metadata_value "$router_transaction_file" router_path)"
  old_version="$(router_metadata_value "$router_transaction_file" old_version)"
  old_target="$(router_metadata_value "$router_transaction_file" old_target)"
  old_archive_sha256="$(router_metadata_value "$router_transaction_file" old_archive_sha256)"
  old_binary_sha256="$(router_metadata_value "$router_transaction_file" old_binary_sha256)"
  candidate_version="$(router_metadata_value "$router_transaction_file" candidate_version)"
  candidate_target="$(router_metadata_value "$router_transaction_file" candidate_target)"
  candidate_archive_sha256="$(router_metadata_value "$router_transaction_file" candidate_archive_sha256)"
  candidate_binary_sha256="$(router_metadata_value "$router_transaction_file" candidate_binary_sha256)"
  candidate_temp_path="$(router_metadata_value "$router_transaction_file" candidate_temp_path)"

  [[ "$schema" == "1" && "$transaction_plugin" == "$plugin_id" ]] || {
    printf '%s\n' "Router transaction state is incomplete or belongs to another installer." >&2
    return 1
  }
  for field in "$router_path" "$old_version" "$old_target" "$old_archive_sha256" \
    "$old_binary_sha256" "$candidate_version" "$candidate_target" \
    "$candidate_archive_sha256" "$candidate_binary_sha256" "$candidate_temp_path"; do
    router_require_safe_value "transaction" "$field" || return $?
  done
  for digest in "$old_archive_sha256" "$old_binary_sha256" \
    "$candidate_archive_sha256" "$candidate_binary_sha256"; do
    [[ "$digest" =~ ^[0-9a-f]{64}$ ]] || {
      printf '%s\n' "Router transaction state contains an invalid digest." >&2
      return 1
    }
  done
  router_require_release_identity "$old_version" "$old_target" || return $?
  router_require_release_identity "$candidate_version" "$candidate_target" || return $?
  router_require_user_path "$router_path" || return $?
  router_require_user_path "$candidate_temp_path" || return $?
  [[ "$(dirname -- "$(realpath -m -- "$candidate_temp_path")")" \
      == "$(dirname -- "$(realpath -m -- "$router_path")")" \
      && "$(basename -- "$candidate_temp_path")" == .wayfinder-router.candidate.* ]] || {
    printf '%s\n' "Router transaction candidate path is outside the promotion boundary." >&2
    return 1
  }
  [[ -f "$router_path" && ! -L "$router_path" ]] || {
    printf '%s\n' "The Router transaction target is missing or linked; manual recovery is required." >&2
    return 1
  }
  actual_sha256="$(sha256sum "$router_path" | cut -d ' ' -f 1)"
  if [[ "$actual_sha256" == "$candidate_binary_sha256" ]]; then
    router_write_current_metadata "$candidate_version" "$candidate_target" \
      "$candidate_archive_sha256" "$candidate_binary_sha256" "$router_path" || return $?
    rm -f -- "$candidate_temp_path" "$router_transaction_file"
    printf '%s\n' "Recovered a completed Router promotion and committed its provenance."
  elif [[ "$actual_sha256" == "$old_binary_sha256" ]]; then
    rm -f -- "$candidate_temp_path" "$router_transaction_file"
    printf '%s\n' "Recovered an uncommitted Router promotion; the previous binary remains active."
  else
    printf '%s\n' "Router transaction recovery found an unrecognized binary; refusing changes." >&2
    return 1
  fi
}

router_lifecycle_hook() {
  :
}

router_promote_candidate() {
  local candidate_binary="$1"
  local candidate_version="$2"
  local candidate_target="$3"
  local candidate_archive_sha256="$4"
  local candidate_binary_sha256 router_bin_dir candidate_temp backup_temp backup_metadata_temp

  router_recover_transaction || return $?
  router_load_current || return $?
  [[ -f "$candidate_binary" && ! -L "$candidate_binary" && -x "$candidate_binary" ]] || {
    printf '%s\n' "The candidate Router is missing, linked, or not executable." >&2
    return 1
  }
  [[ "$candidate_archive_sha256" =~ ^[0-9a-f]{64}$ ]] || {
    printf '%s\n' "The candidate archive digest is invalid." >&2
    return 1
  }
  router_require_release_identity "$candidate_version" "$candidate_target" || return $?
  "$candidate_binary" --version | grep -Fx "wayfinder-router $candidate_version" >/dev/null || {
    printf '%s\n' "The candidate Router version does not match its reviewed pin." >&2
    return 1
  }
  candidate_binary_sha256="$(sha256sum "$candidate_binary" | cut -d ' ' -f 1)"
  if [[ "$candidate_binary_sha256" == "$current_router_binary_sha256" ]]; then
    printf 'Wayfinder Router %s is already the active plugin-owned binary.\n' "$candidate_version"
    return 0
  fi

  router_bin_dir="$(dirname -- "$current_router_path")"
  install -d -m 0700 "$router_provenance_dir" "$router_backup_dir"
  candidate_temp="$(mktemp "$router_bin_dir/.wayfinder-router.candidate.XXXXXX")"
  backup_temp="$(mktemp "$router_backup_dir/.wayfinder-router.backup.XXXXXX")"
  backup_metadata_temp="$(mktemp "$router_backup_dir/.provenance.XXXXXX")"
  install -m 0755 "$candidate_binary" "$candidate_temp"
  install -m 0755 "$current_router_path" "$backup_temp"
  router_write_metadata "$backup_metadata_temp" last-known-good "$current_router_version" \
    "$current_router_target" "$current_router_archive_sha256" "$current_router_binary_sha256" \
    "$current_router_path" || return $?
  mv -f -- "$backup_temp" "$router_backup_binary"
  mv -f -- "$backup_metadata_temp" "$router_backup_metadata"
  router_write_transaction "$candidate_version" "$candidate_target" "$candidate_archive_sha256" \
    "$candidate_binary_sha256" "$candidate_temp" || return $?
  router_lifecycle_hook after-transaction || return $?
  mv -f -- "$candidate_temp" "$current_router_path"
  router_lifecycle_hook after-promotion || return $?
  router_write_current_metadata "$candidate_version" "$candidate_target" \
    "$candidate_archive_sha256" "$candidate_binary_sha256" "$current_router_path" || return $?
  rm -f -- "$router_transaction_file"
  printf 'Promoted Wayfinder Router %s; retained %s as last known good.\n' \
    "$candidate_version" "$current_router_version"
}

router_rollback() {
  router_recover_transaction || return $?
  router_load_current || return $?
  if [[ ! -f "$router_backup_binary" || -L "$router_backup_binary" || ! -x "$router_backup_binary" \
      || ! -f "$router_backup_metadata" || -L "$router_backup_metadata" ]]; then
    printf '%s\n' "No verified last-known-good Router is available for rollback." >&2
    return 1
  fi

  local backup_schema backup_plugin backup_role backup_version backup_target backup_archive_sha256
  local backup_binary_sha256 backup_router_path actual_backup_sha256
  backup_schema="$(router_metadata_value "$router_backup_metadata" schema_version)"
  backup_plugin="$(router_metadata_value "$router_backup_metadata" plugin_id)"
  backup_role="$(router_metadata_value "$router_backup_metadata" role)"
  backup_version="$(router_metadata_value "$router_backup_metadata" router_version)"
  backup_target="$(router_metadata_value "$router_backup_metadata" router_target)"
  backup_archive_sha256="$(router_metadata_value "$router_backup_metadata" archive_sha256)"
  backup_binary_sha256="$(router_metadata_value "$router_backup_metadata" binary_sha256)"
  backup_router_path="$(router_metadata_value "$router_backup_metadata" router_path)"
  for field in "$backup_version" "$backup_target" "$backup_archive_sha256" \
    "$backup_binary_sha256" "$backup_router_path"; do
    router_require_safe_value "last-known-good provenance" "$field" || return $?
  done
  router_require_release_identity "$backup_version" "$backup_target" || return $?
  [[ "$backup_schema" == "1" && "$backup_plugin" == "$plugin_id" \
      && "$backup_role" == "last-known-good" \
      && "$backup_router_path" == "$current_router_path" ]] || {
    printf '%s\n' "Last-known-good Router provenance is invalid." >&2
    return 1
  }
  [[ "$backup_archive_sha256" =~ ^[0-9a-f]{64}$ \
      && "$backup_binary_sha256" =~ ^[0-9a-f]{64}$ ]] || {
    printf '%s\n' "Last-known-good Router provenance contains an invalid digest." >&2
    return 1
  }
  actual_backup_sha256="$(sha256sum "$router_backup_binary" | cut -d ' ' -f 1)"
  [[ "$actual_backup_sha256" == "$backup_binary_sha256" ]] || {
    printf '%s\n' "The last-known-good Router digest does not match; refusing rollback." >&2
    return 1
  }
  router_promote_candidate "$router_backup_binary" "$backup_version" "$backup_target" \
    "$backup_archive_sha256" || return $?
}
