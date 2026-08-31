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

Open the Wayfinder bar item and choose **Set up Wayfinder**. The panel invokes
the installed plugin's bounded `./install.sh --bootstrap-router` mode, which
downloads only the pinned release for the detected architecture, verifies its
SHA-256 digest, and installs it under the current user's home. It refuses
unsupported architectures and paths outside `HOME`. Running that same bounded
command from the installed plugin directory is the recovery path if the panel
cannot remain open long enough to show the error.

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

Reprint the reviewed recipe with `wayfinder-router connect codex`, `wayfinder-router connect claude-code`, `wayfinder-router connect opencode`, `wayfinder-router connect pi`, or `wayfinder-router connect aider`. Confirm the exact client version in the compatibility matrix and the loopback endpoint. The placeholder local token is not a provider credential.

For Codex, review `~/.codex/config.toml` and remove the `wayfinder` provider/model selection to reverse the connection. For Claude Code, unset the four variables listed in the main README. For OpenCode, remove the `provider.wayfinder` object and any `wayfinder/auto` selection from the project or user `opencode.json`. For Pi, remove the `providers.wayfinder` object and any saved `wayfinder/auto` selection from `~/.pi/agent/models.json`. For Aider, unset `OPENAI_API_BASE` and `OPENAI_API_KEY` and stop selecting `openai/auto`. Wayfinder never silently edits a client or imports its provider credentials.

## Project profile controls

The pinned `router-v1.0.0` exposes authenticated local project profiles. If the panel reports them as unavailable, confirm `wayfinder-router --version` and `wayfinder-router capabilities --json`, then check whether a different executable appears first on `PATH`. Do not work around the capability gate by editing QML or project state.

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

The lifecycle remains contract-validated for upgrade and rollback. The plugin
pins Router `1.0.0`; `2026.8.1` remains the immutable final DateVer release.
The next stronger release-gated claim requires a recorded
`2026.8.1` → `1.0.0` → `2026.8.1` → `1.0.0` cycle between checksum-verified
releases. Lifecycle decisions use exact versions and reviewed checksums, never
ordering across the numbering change.
