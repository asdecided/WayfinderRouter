# Native Omarchy release smoke

Hosted CI validates the plugin contract, but it cannot prove that a real
Quickshell session renders the widget or survives an Omarchy shell restart.
Run this gate on a clean machine matching the supported matrix before moving a
coordinated Router/plugin candidate to release-ready.

## Preconditions

1. Install the candidate through Omarchy's marketplace path.
2. Open Wayfinder in the bar and complete **Set up Wayfinder**.
3. Confirm the Router service is healthy and the widget is visibly rendering.
4. Keep the installed plugin Git checkout clean. Store evidence outside it.

The smoke deliberately requires three explicit flags. Two record human
attestations that a headless script cannot infer; the third authorizes a visible
shell restart.

```sh
plugin_dir="$HOME/.config/omarchy/plugins/io.github.asdecided.wayfinder"
"$plugin_dir/scripts/omarchy-native-smoke.sh" \
  --restart-shell \
  --confirm-widget-visible \
  --confirm-first-run-complete \
  --output "$HOME/wayfinder-native-smoke.json"
```

The script fails unless:

- the installed checkout is clean and has one immutable source commit;
- Omarchy, Quickshell, the plugin, and Router match the compatibility matrix;
- Omarchy's validator accepts the installed plugin;
- Wayfinder is enabled as both a service and bar widget;
- the resolved Router command and systemd unit both use the exact
  provenance-verified plugin-owned binary;
- the user service and loopback health endpoint are healthy before restart;
- the Omarchy shell returns within 30 seconds; and
- the enabled plugin, Router service, and health endpoint survive the restart.

The resulting JSON contains versions, source and artifact digests, boolean
results, and the two operator attestations. It contains no hostname, username,
absolute path, policy, project token, provider credential, environment dump, or
request content. Review and attach it to the coordinated release PR.

This artifact proves only the exact source commit and machine architecture it
records. It is not reusable evidence for a later candidate. Upgrade and
rollback between two real release archives remain a separate coordinated gate.
