# Wayfinder for Omarchy

Wayfinder is the model router built for Omarchy. One plugin installs the native
Rust Router, gives supported coding agents a shared local endpoint, and exposes
health, recent routes, local-versus-hosted distribution, model readiness, and
savings in the Omarchy bar.

The plugin is the flagship product surface; the independently supervised
`wayfinder-router` process remains the only routing authority. Reloading or
disabling the Omarchy shell does not interrupt requests.

## Requirements

- Omarchy Quattro with third-party plugin support.
- `curl`, `systemd --user`, and `wl-copy` (provided by a standard Omarchy
  installation).
- Rust 1.85 or later when building `wayfinder-router` from source.

The plugin never stores provider credentials. It reads only the gateway's
prompt-free local status surfaces.

## Install

```sh
omarchy plugin add https://github.com/asdecided/omarchy-wayfinder.git
cd ~/.config/omarchy/plugins/io.github.asdecided.wayfinder
./install.sh
```

The installer builds the Rust CLI when Cargo is available and the binary is
missing, rescans plugins, and enables the widget. It does not create a gateway
configuration or provider credential.

Open the Wayfinder bar item and choose **Install service**. The existing CLI
writes and starts `~/.config/systemd/user/wayfinder-router.service` on the
configured loopback address.

## Install from the Wayfinder workspace

Contributors working from a WayfinderRouter checkout can install the same
plugin directly:

```sh
git clone https://github.com/asdecided/WayfinderRouter.git
cd WayfinderRouter
git checkout --detach 9874b08b466822ea6fd0c0875a88950521110997
./integrations/omarchy-wayfinder/install.sh
```

The detached checkout matches the Router revision used by the standalone
installer. Router upgrades are reviewed as explicit plugin changes before this
pin moves.

Omarchy clones third-party plugins disabled so their source can be reviewed
before enablement. The script performs the explicit enable step.

## Configure

Use **Setup → Plugins → Wayfinder** to change:

- **Gateway endpoint** — defaults to `http://127.0.0.1:8088`.
- **Refresh interval** — defaults to 15 seconds and is bounded to 5–300.
- **Router config** — optional path passed to `wayfinder-router service install`.

Only a loopback HTTP endpoint can be installed or restarted by the widget.
Remote HTTPS Wayfinder gateways are observed but never managed.

Point compatible applications at the endpoint shown in the panel:

```sh
export OPENAI_BASE_URL=http://127.0.0.1:8088/v1
```

Provider keys remain in the environment or the existing reviewed credential
boundary referenced by the Wayfinder configuration.

## Controls

- Left click: open or close the status panel.
- Right click: refresh status.
- Middle click: copy the endpoint.
- **Install / Start / Restart service**: manage the local systemd user unit.

When operator endpoints are protected by OIDC, the plugin continues to show
health but hides unavailable model, recent-route, and savings metadata. It does
not ask for or persist an operator token.

## Remove

```sh
omarchy plugin remove io.github.asdecided.wayfinder
```

Removal intentionally leaves the Wayfinder gateway running because other
applications may still use it. Remove that independently only when desired:

```sh
wayfinder-router service uninstall
```

## Validate

```sh
node scripts/validate.mjs
node test/model.test.mjs
bash -n install.sh uninstall.sh
omarchy plugin validate  # when run on Omarchy Quattro
```

## License

Apache-2.0. See `LICENSE`.
