# WF-ADR-0083: Portable Router SemVer

- Status: Accepted
- Date: 2026-08-30

## Context

The portable Router used DateVer while Wayfinder Desktop used SemVer. The date
encoded in the Router version did not communicate compatibility, forced a new
calendar-shaped number for every release, and made a coordinated Router and
Omarchy release harder to reason about. The next Router candidate, `2026.8.2`,
has not been tagged or published.

The existing `router-v2026.8.0` and `router-v2026.8.1` releases are public and
must remain immutable. Omarchy also needs to upgrade from the last DateVer
release to the first SemVer release and roll back without comparing versions.

## Decision

The portable Router returns to SemVer. The unpublished `2026.8.2` candidate is
released as `1.0.0`; no `router-v2026.8.2` tag or release is created.

Stable Router tags use `router-vMAJOR.MINOR.PATCH`, with a strict SemVer core and
no prerelease or build suffix. Major versions change incompatible public Router
contracts, minor versions add backward-compatible behavior, and patch versions
contain backward-compatible fixes.

The public Router surface includes documented CLI commands and exit behavior,
Router-owned configuration, HTTP contracts, release archive layout, service
lifecycle contracts, and versioned receipts or evidence. Undocumented internals
and upstream-provider behavior are not compatibility promises.

Router, Desktop, protocol, configuration, receipt, evidence, and persisted-data
schema versions remain independent. A product release does not silently advance
another product or schema.

Published DateVer tags remain immutable release identities and supported
rollback inputs. Installers use exact tags and reviewed checksums as authority;
they do not order or reinterpret release versions. The Omarchy lifecycle must
accept strict SemVer cores and prove `2026.8.1 -> 1.0.0 -> 2026.8.1` promotion
and rollback. Homebrew changes from the DateVer line with `version_scheme 1`.

Wayfinder Desktop retains its independent `desktop-vMAJOR.MINOR.PATCH` line.

## Consequences

- Router `1.0.0` describes a stable public contract instead of a release date.
- Existing installations can upgrade and roll back across the numbering change.
- Downstream integrations must validate the version grammar, not assume a
  four-digit major version or compare a DateVer release with a SemVer release.
- The first Homebrew formula update after the reset must declare the version
  scheme change so Homebrew treats `1.0.0` as newer than `2026.8.1`.
