---
name: wayfinder-codex-ux-review
description: Review and improve WAYFINDER's native macOS UX against OpenAI Codex app patterns and the repo's accepted menu-bar design contracts. Use when working on the Swift macOS app, menu-bar popover, chat window, settings window, native app screenshots, Codex-like interaction design, or UX review tasks that mention WAYFINDER, Codex, Codex app, Browser plugin, in-app browser, or OpenAI product reference.
---

# WAYFINDER Codex UX Review

## Overview

Use this skill to audit or revise the WAYFINDER native macOS experience so it feels closer to the OpenAI Codex product where that analogy is useful: quiet, thread-centered, dense but readable, tool-aware, and reviewable. Keep WAYFINDER's own architecture and accepted design contracts authoritative.

## Core Workflow

1. Confirm the target surface:
   - Native Swift app: `macos/WayfinderMac`.
   - The legacy web/Tauri client was removed from the tree (it remains in git history); do not target it.
   - Product docs/design contracts: `macos/README.md`, `decisions/WF-ADR-0042-desktop-menu-bar-client.md`, `designs/WF-DESIGN-0012-desktop-popover-design.md`, and `designs/WF-DESIGN-0014-flat-list-popover.md`.
2. Read `references/codex-ux-reference.md` before making UX judgments.
3. Inspect the current Swift implementation before prescribing changes. Start with:
   - `macos/WayfinderMac/Package.swift`
   - `macos/WayfinderMac/Sources/WayfinderMacApp/WayfinderMacMain.swift`
   - `macos/WayfinderMac/Sources/WayfinderMac/WayfinderMacApp.swift`
4. Make UX changes in the smallest native surface that owns the behavior. Prefer SwiftUI/AppKit-native structure over web-era assumptions.
5. Verify with the strongest available check:
   - Run `swift test` for behavior that can be tested.
   - Run `swift run WayfinderMac` for visual/manual review when feasible.
   - Use screenshots or Computer Use for native macOS visual inspection when available.
   - Use the Browser plugin only for local web previews, docs, or browser-rendered prototypes; do not treat it as the primary inspection tool for native Swift UI.

## UX Priorities

- Preserve the menu-bar utility shape: fast glance, transient popover, native calm, no decorative landing-page energy.
- Make the routing decision legible first. The product is the decision, not a generic chat wrapper.
- Favor flat native menu grammar: rows, hairlines, compact labels, restrained icons, clear status, and no card grids unless an accepted design doc calls for one.
- Mirror Codex at the interaction level: persistent thread context, clear task state, tool/action affordances, reviewability, precise feedback loops, and compact controls.
- Avoid copying Codex visual details that conflict with WAYFINDER's accepted tokens, route colors, privacy claims, or menu-bar constraints.
- Keep privacy language exact: offline mode is the only mode that guarantees nothing leaves the machine.
- Prefer native accessibility: keyboard reachability, VoiceOver labels, reduced motion, visible focus, and no hidden action behind hover-only UI.

## Codex Product Source

Use `references/codex-ux-reference.md` for the stable review lens. When the task needs current Codex product facts, first use the OpenAI docs/Codex manual workflow rather than relying on memory.

Relevant public source anchors:

- https://developers.openai.com/codex/app/features
- https://developers.openai.com/codex/app/browser
- https://developers.openai.com/codex/skills
- https://developers.openai.com/codex/plugins

## Browser Plugin Guidance

For native Swift work, do not lead with the Browser plugin. Use it when:

- The task includes a local web preview, rendered documentation, or prototype route.
- The user explicitly asks to use `@Browser` or the in-app browser.

When using Browser, keep the task scoped to a concrete URL, viewport, state, and visible issue. For native macOS UI, ask for or capture screenshots instead.

## Output Shape

For UX review, lead with prioritized findings grounded in the visible UI or source files. For implementation, summarize the changed surfaces and the verification performed.

For design direction, separate "Codex-like" from "WAYFINDER-specific" so future changes do not flatten the product into a copy.
