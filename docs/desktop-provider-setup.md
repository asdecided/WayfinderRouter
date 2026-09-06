# Native desktop provider setup

The Linux `wayfinder-router setup` command emits one JSON result with
`schema_version: 1` and `ok`. Errors are fixed operator messages, never raw
provider or credential-process output. `capabilities --json` advertises
`setup_schema_version: 1` on Linux. Consumers must check it before offering setup.

Use the standard `$XDG_CONFIG_HOME/wayfinder/wayfinder-router.toml` (or
`$HOME/.config/wayfinder/wayfinder-router.toml`). Create it with `init --preset
local` first. The endpoint must be `http://127.0.0.1:PORT`. Custom paths and
policies are deliberately preserved.

```sh
wayfinder-router setup status --config "$HOME/.config/wayfinder/wayfinder-router.toml" --endpoint http://127.0.0.1:8088
```

All actions use those same `--config` and `--endpoint` options:

| Action | Effect |
| --- | --- |
| `discover` | Read one API key line from stdin; discover models; store in Secret Service |
| `refresh-models` | Refresh the connected account's models |
| `activate --model MODEL` | Explicitly activate one discovered model and restart service |
| `test` | Send a small billed request and require a matching successful receipt |
| `repair` | Reconcile interrupted activation and restart the service |
| `disconnect` | Stop service, restore starter, remove owned key, restart local service |

The desktop must have `/usr/bin/secret-tool` and an unlocked Secret Service
keyring. Keys never go into command arguments or policy/state files. The frontend
writes the key through the process's stdin pipe and immediately clears its input.
The command never prompts for provider credentials through another application.

State lives in `omarchy-setup/state.json` beside the policy. It contains only a
credential item reference, discovered model IDs, stage, and dated verification.
A process lock prevents concurrent changes. Configuration updates use private
staging files, atomic rename, and fsync. On interruption, inspect status and use
repair or disconnect; do not delete the journal to bypass a failed cleanup.

Existing `service`, `connect`, `init`, and `doctor` commands retain their roles.
Binary archive installation, upgrade, and rollback remain the plugin installer's
checksum- and ownership-checked operations. There is no Python setup runtime.

Controlled Rust tests cover failure boundaries. An actual OpenAI account,
unlocked desktop keyring, service restart, reboot and coding-agent request on
Omarchy are still required for release acceptance.
