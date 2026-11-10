// Framework-agnostic decision-render helpers (WF-ADR-0042), originally shared by the legacy
// desktop popover (since removed from the tree; see git history). These format/select over a decision object as produced by
// `gateway.decisionFromDebug` / `decisionFromHeaders` — `{ model, score, mode, isLocal,
// contributions: [{ name, value, share }], targets, decisionOnly, dryRun }`. They NEVER score
// (WF-ADR-0001); they only shape what the gateway already decided.
//
// The decision-render contract was pinned by recorded golden gateway fixtures that lived with
// the legacy desktop client (removed with it; see git history), not by any Python module —
// these helpers are kept deliberately small and literal.

export const LOCAL_GLYPH = '●';
export const CLOUD_GLYPH = '◆';

/** 'local' | 'cloud' — the route kind, from the gateway's `isLocal`. */
export function routeKind(decision) {
  return decision && decision.isLocal ? 'local' : 'cloud';
}

/** Uppercase route label for a pill/badge, e.g. 'LOCAL' / 'CLOUD'. */
export function routeLabel(decision) {
  return routeKind(decision).toUpperCase();
}

/** The route glyph: filled dot for local, diamond for cloud. */
export function routeGlyph(decision) {
  return decision && decision.isLocal ? LOCAL_GLYPH : CLOUD_GLYPH;
}

/** Score as a fixed 2-dp string (tabular-nums in the UI so it doesn't jitter). */
export function formatScore(score) {
  return (Number.isFinite(score) ? score : 0).toFixed(2);
}

/**
 * Top-N contributions by share, largest first — the "why" rows. Ties keep input order
 * (stable sort). Returns a new array; never mutates the decision.
 */
export function topContributions(decision, n = 5) {
  const rows = (decision && decision.contributions) || [];
  return rows
    .map((c, i) => ({ ...c, _i: i }))
    .sort((a, b) => (b.share - a.share) || (a._i - b._i))
    .slice(0, n)
    .map(({ _i, ...c }) => c);
}

/**
 * A one-line routing badge, e.g. "cloud · score 0.82" (+ " · cache hit" / " · offline" /
 * " · decision only") — the decision card's sub-line.
 */
export function routingBadge(decision, { cache = false, offline = false } = {}) {
  if (!decision) return '';
  const parts = [`${routeKind(decision)} · score ${formatScore(decision.score)}`];
  if (decision.decisionOnly) parts.push('decision only');
  if (offline) parts.push('offline');
  if (cache) parts.push('cache hit');
  return parts.join(' · ');
}
