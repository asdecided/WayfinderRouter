// Canonical design tokens (WF-ADR-0042), mirrored from wayfinder_router/demo.html `:root`
// (the source of truth per WF-ADR-0020). The legacy desktop popover (since removed; see git
// history) rendered these over macOS vibrancy on a ~92%-opacity card layer; route colour is local-green / cloud-amber, tabular-nums so the
// score never jitters. Keep these in sync with demo.html — a later `tools/extract-tokens` may
// generate this file; for now it is a hand-mirrored copy.

export const light = {
  bg: '#ffffff', panel: '#f9f9fa', elev: '#ffffff',
  text: '#0d0d0d', muted: '#6b6b78',
  line: '#ececef', lineStrong: '#e2e2e6', user: '#f4f4f5',
  accent: '#10a37f', accentWeak: '#eaf6f2',   // local
  cloud: '#bd6a13', cloudWeak: '#fbf0e3',      // hosted
  btn: '#0d0d0d', btnText: '#ffffff', track: '#ececed',
  radius: '18px', radiusSm: '13px', pill: '999px',
  font: 'ui-sans-serif,-apple-system,"SF Pro Text","Segoe UI",Roboto,Helvetica,Arial,sans-serif',
  mono: 'ui-monospace,"SF Mono","Cascadia Code",Menlo,Consolas,monospace',
};

export const dark = {
  bg: '#1e1e20', panel: '#262629', elev: '#2a2a2d',
  text: '#ececec', muted: '#9a9aa6',
  line: 'rgba(255,255,255,.08)', lineStrong: 'rgba(255,255,255,.13)', user: '#2d2d31',
  accent: '#19c8a4', accentWeak: '#15302a',    // local
  cloud: '#e0a25c', cloudWeak: '#332610',       // hosted
  btn: '#ececec', btnText: '#0d0d0d', track: '#39393d',
  radius: '18px', radiusSm: '13px', pill: '999px',
  font: light.font, mono: light.mono,
};

// The route's accent colour: local→green, hosted→amber. `isLocal` comes from the gateway
// decision (decisionFrom*), never computed in the client.
export function routeColor(theme, isLocal) {
  return isLocal ? theme.accent : theme.cloud;
}
