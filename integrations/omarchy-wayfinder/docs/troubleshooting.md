# Troubleshooting, updates, and rollback

Wayfinder keeps the Omarchy shell, Router service, plugin source, policy, project profiles, and credentials as separate ownership domains. Diagnose the failing layer before changing anything.

## Safe first checks

These commands do not print provider credential values:

```sh
omarchy version
quickshell --version
omarchy plugin list --json
omarchy plugin validate ~/.config/omarchy/plugins/io.github.asdecided.wayfinder
wayfinder-router --version
wayfinder-router capabilities --json
wayfinder-router doctor --json
systemctl --user status wayfinder-router.service --no-pager
journalctl --user -u wayfinder-router.service --since today --no-pager
curl --fail --silent http://127.0.0.1:8088/healthz
```

Compare the versions with the [supported matrix](compatibility.md). Redact local paths if they are sensitive before sharing output. Do not post policy files, environment dumps, project tokens, or provider credentials.

## Plugin is installed but not visible

1. Run the official manifest validator shown above.
2. Confirm `io.github.asdecided.wayfinder` appears in `omarchy plugin list --json`.
3. Enable it with `omarchy plugin enable io.github.asdecided.wayfinder`.
4. Rescan with `omarchy-shell shell rescanPlugins`.
5. If the shell still rejects it, inspect the current user journal without pasting unrelated environment data.

The plugin requires Omarchy Quattro's third-party plugin contract. Omarchy 3.x has no compatible shell plugin surface.

## Router is missing

Run `./install.sh` from the installed plugin directory. The installer downloads only the pinned release for the detected architecture, verifies its SHA-256 digest, and installs it under the current user's home. It refuses unsupported architectures and paths outside `HOME`.

If another `wayfinder-router` is already on `PATH`, the installer uses it without replacing it. Check `command -v wayfinder-router` and `wayfinder-router --version` to determine which executable owns the command.

## Setup or service health failed

Run `wayfinder-router doctor --json`, then fix only the reported layer:

- a missing policy can be created by reopening the panel and choosing **Set up Wayfinder**;
- an invalid existing policy is never overwritten—repair it, then choose **Check policy**;
- missing provider environment is reported by variable name and may be added to the user-service environment without entering QML;
- an inactive unit can be inspected with `systemctl --user status` and the bounded journal command above;
- a healthy service on a non-default port requires the same loopback endpoint in plugin settings.

First-run setup is resumable. Reopening the panel continues from the first incomplete step instead of replacing completed state.

## Coding agent does not route

Reprint the reviewed recipe with `wayfinder-router connect codex` or `wayfinder-router connect claude-code`. Confirm the exact client version in the compatibility matrix and the loopback endpoint. The placeholder local token is not a provider credential.

For Codex, review `~/.codex/config.toml` and remove the `wayfinder` provider/model selection to reverse the connection. For Claude Code, unset the four variables listed in the main README. Wayfinder never silently edits either client.

## Project profile controls are unavailable

The currently downloadable `router-v2026.8.0` does not expose the project-profile capability. The panel intentionally reports it as unavailable. Do not work around that gate by editing QML or project state; the control becomes active only when a later coordinated plugin release pins a compatible Router.

## Review and update plugin source

Omarchy's Git-managed update shows a diff before it fast-forwards the checkout:

```sh
plugin_dir="$HOME/.config/omarchy/plugins/io.github.asdecided.wayfinder"
git -C "$plugin_dir" status --short
git -C "$plugin_dir" rev-parse HEAD
omarchy plugin update io.github.asdecided.wayfinder
node "$plugin_dir/scripts/validate.mjs"
node "$plugin_dir/scripts/validate-compatibility.mjs"
```

Stop if the checkout has local changes. Record the previous commit before approving an update. A plugin source update does not upgrade or replace an existing Router binary.

## Roll back plugin source

If a reviewed plugin update breaks the shell surface, disable it first, select the recorded previous commit without rewriting repository history, validate, and re-enable:

```sh
plugin_dir="$HOME/.config/omarchy/plugins/io.github.asdecided.wayfinder"
omarchy plugin disable io.github.asdecided.wayfinder
git -C "$plugin_dir" switch --detach PREVIOUS_REVIEWED_COMMIT
node "$plugin_dir/scripts/validate.mjs"
omarchy plugin enable io.github.asdecided.wayfinder
omarchy-shell shell rescanPlugins
```

Replace `PREVIOUS_REVIEWED_COMMIT` with the full commit recorded before the update. To return to the normal update lane later, switch back to the repository's tracked default branch, validate the diff, and run `omarchy plugin update` again.

## Upgrade or roll back a plugin-owned Router

After a reviewed plugin update moves the Router release and archive digests, explicitly promote the new pin:

```sh
./install.sh --upgrade-router
```

Upgrade is allowed only when the active binary and provenance were created by this plugin and still match. The candidate archive is checksum-verified and executed before promotion. The previous binary and its provenance become the last-known-good pair.

Promotion replaces the on-disk executable. Restart `wayfinder-router.service` from the Wayfinder panel after a successful upgrade so the supervised process runs that binary.

To swap back to that verified pair:

```sh
./install.sh --rollback-router
```

Rollback retains the displaced version as the next last-known-good target, so the operation is reversible. A missing, modified, linked, independently installed, or provenance-mismatched binary stops both actions without replacement.

Restart the service after rollback as well. If the promoted service cannot start, roll back the binary, restart again, and inspect the bounded user journal command from the first section.

Promotion intent is recorded before the atomic rename. The next installer action automatically resolves an interrupted transaction by comparing the active binary with both reviewed digests. To request only that recovery step, run:

```sh
./install.sh --recover-router
```

Do not edit or delete transaction/provenance files during recovery. If the active digest matches neither side, Wayfinder fails closed and requires manual inspection. Removing the plugin or disabling the widget does not stop the independently supervised Router service.

The lifecycle is contract-validated with synthetic versioned binaries. It becomes release-gated only when a coordinated release moves the real Router pin and exercises upgrade and rollback between two checksum-verified archives.
