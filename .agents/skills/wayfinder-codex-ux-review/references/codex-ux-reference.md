# Codex UX Reference For WAYFINDER

Use this as a stable lens for WAYFINDER reviews. Refresh from official OpenAI docs when exact Codex product behavior matters.

## What To Borrow From Codex

- Thread-first work: the user should always understand what task/context they are in, what the app is doing, and what can be reviewed next.
- Tool visibility: actions should feel available without becoming noisy. Use compact icon+label rows, clear states, and predictable placement.
- Review loops: make it easy to inspect a result, leave precise feedback, and rerun a narrow pass.
- Calm density: prioritize scan-friendly information, restrained contrast, small controls, and readable hierarchy over hero sections or marketing-style layout.
- Parallel surfaces: Codex separates conversation, terminal, browser/review, settings, and worktree state. WAYFINDER should similarly keep routing status, chat, settings, and diagnostics distinct.
- Explicit state: show running, degraded, offline, unreachable, loading, empty, and error states directly. Avoid vague "something happened" surfaces.

## What Not To Copy

- Do not turn WAYFINDER into a general coding-agent dashboard. It is a router and menu-bar utility.
- Do not import web-app card grids into the native popover. The accepted WAYFINDER direction is a flat native menu/list.
- Do not make decorative hero panels, large gradients, or one-off illustrations for core utility screens.
- Do not use Codex language where WAYFINDER has stricter privacy semantics. Offline mode is the only state that guarantees nothing leaves the machine.
- Do not rely on the Browser plugin for native Swift UI inspection.

## Native macOS Review Checklist

- Menu-bar popover reads as transient, fast, and native.
- The current route, local/cloud split, savings, and health are visible without explanation.
- Chat opens as an intentional deeper surface, not as clutter inside the glance view.
- Settings contains setup, keys, gateway state, and diagnostic actions rather than scattering them through the popover.
- Every row has a stable height, aligned icon slot, readable label, and clear trailing affordance when needed.
- Color has meaning: teal for local/interactive, amber for cloud/degraded, neutral for structure.
- Motion is brief, interruptible, and reduced-motion aware.
- Keyboard and VoiceOver paths exist for all controls.

## Browser Plugin Role

Use the in-app browser for local web previews, rendered docs, or public pages. For the Swift app, prefer app screenshots, SwiftUI previews if present, Computer Use when available, and direct source inspection.

## Official Source Anchors

- Codex app features: https://developers.openai.com/codex/app/features
- In-app browser: https://developers.openai.com/codex/app/browser
- Skills: https://developers.openai.com/codex/skills
- Plugins: https://developers.openai.com/codex/plugins
