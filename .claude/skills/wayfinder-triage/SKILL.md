---
name: wayfinder-triage
description: >-
  Triage the Wayfinder repo in one pass — survey open PRs (CI status, review
  comments, mergeability) and open issues / feature requests, report what needs
  action, then drive small, clear fixes following the project's conventions.
  Use when asked to "do Wayfinder triage", "triage the repo", check repo health,
  or review what's open across PRs and issues.
---

# Wayfinder triage

A repeatable health-and-action pass over the `asdecided/WayfinderRouter` repo.
Works the same from the CLI, web, or mobile because it relies only on the GitHub
MCP tools (`mcp__github__*`) — there is no `gh` CLI in web/mobile sessions.

Default posture: **report first, act conservatively.** Fix what is clearly small and
unambiguous; ask before anything architectural; never merge unless the user has
authorized it.

## 1. Survey

- **Open PRs** — `mcp__github__list_pull_requests` (state `open`). For each, gather:
  - CI: `pull_request_read` method `get_check_runs` (note any non-success conclusion).
  - Reviews: `pull_request_read` method `get_review_comments` (unresolved threads).
  - Mergeability: `pull_request_read` method `get` (`mergeable_state`).
- **Open issues** — `mcp__github__list_issues` (state `OPEN`); classify each as a
  feature request, bug, or question.

## 2. Report

Give a concise status — one line per PR (CI ✅/❌, review state, mergeable?) and per
issue (type + the ask). Lead with anything red or blocked.

## 3. Act (per item)

- **Tractable, small CI failure** → reproduce locally, fix, push to the PR's branch.
- **Ambiguous, architectural, or large** → ask the user first (use AskUserQuestion).
- **Duplicate / no action needed** → skip silently.
- **Green + mergeable PR** → report it as ready. Merge only if the user has said so.
- To keep watching a PR (CI + reviews) instead of one-shot triage, use the PR
  activity subscription rather than polling.

## Project conventions (honor these when fixing or contributing)

- **Branch, never push to `main`.** Open a PR from a feature branch.
- **Run the full CI gate locally before pushing** (mirrors `.github/workflows/ci.yml`):
  - `ruff check .`
  - `mypy wayfinder_router`  (install extras first so mypy sees optional deps:
    `pip install -e ".[dev,gateway,ui]"`)
  - `pytest -q`
  - confirm `rac` is **not** importable — the standalone invariant (WF-ADR-0001).
- **Deterministic core invariant (WF-ADR-0001):** scoring/routing makes no model
  call, needs no key, and touches no network. Model calls live only in the
  invocation / gateway layer (WF-ADR-0004). Never move logic that breaks this.
- **CHANGELOG (Keep a Changelog):** every user-visible change gets a bullet under
  `## Unreleased` in `CHANGELOG.md`, in user-impact voice, citing the relevant
  `WF-ADR-####` / `WF-DESIGN-####`. Create the `## Unreleased` section if absent.
- **Commits:** conventional style (`feat(...)`, `fix(...)`, `docs(...)`).
- **Merging:** squash with a clean, hand-written commit title + message. For stacked
  PRs, after the base merges, rebase the child onto the squashed base so its diff
  stays minimal.
- **Versioning is CalVer `YYYY.M.MICRO`,** single-sourced at `__version__` in
  `wayfinder_router/__init__.py` (read dynamically by `pyproject.toml`). Cut a release
  by promoting `## Unreleased` to a dated `## vYYYY.M.MICRO — DATE` entry (with a short
  intro) and bumping `__version__` to match. Tagging/publishing is a human step.

## Notes

- Providers: Wayfinder forwards to any OpenAI-compatible `/chat/completions` endpoint
  with a Bearer key — there is no per-provider code, so "does it support X?" is almost
  always "yes, point an arm's `base_url` at it." Auth schemes other than Bearer (e.g.
  Azure's `api-key` header) would need a code change.
- Keys are read from the environment at request time and never written to disk; an
  optional `api_key_cmd` can fill one from a secret store in memory.
