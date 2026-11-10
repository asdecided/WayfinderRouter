//! How the chat pipeline arrived at its chosen route.
//!
//! The mode is reported on the `x-wayfinder-router-mode` header, in decision
//! bodies, in recent-decision records, in the `wayfinder_router_requests_total`
//! metric label and in the cache route key. Those wire strings are produced
//! only by [`RouteMode::as_str`]; the pipeline itself compares variants.

/// The routing mode of one chat decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RouteMode {
    /// The complexity score picked the tier.
    Scored,
    /// The request named a configured model explicitly.
    Pinned,
    /// A slash directive in the conversation pinned a model.
    SlashPinned,
    /// The request named a routing preset.
    Preset,
    /// A per-request threshold header replaced the configured tiers.
    ThresholdOverride,
    /// The sticky latch kept the conversation on a higher tier.
    Sticky,
    /// An exhausted spend budget lowered the route.
    BudgetDegraded,
    /// The virtual key's model allowlist clamped the route.
    KeyScoped,
    /// A canary rollout replaced the scored baseline route.
    Canary,
}

impl RouteMode {
    /// The stable wire name of this mode.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Scored => "scored",
            Self::Pinned => "pinned",
            Self::SlashPinned => "slash-pinned",
            Self::Preset => "preset",
            Self::ThresholdOverride => "threshold-override",
            Self::Sticky => "sticky",
            Self::BudgetDegraded => "budget-degraded",
            Self::KeyScoped => "key-scoped",
            Self::Canary => "canary",
        }
    }

    /// Whether the request pinned its destination explicitly or by slash
    /// directive, which disables tier failover and offline tier convergence.
    pub(crate) const fn is_pinned(self) -> bool {
        matches!(self, Self::Pinned | Self::SlashPinned)
    }
}

#[cfg(test)]
mod tests {
    use super::RouteMode;

    #[test]
    fn route_mode_wire_names_are_stable() {
        let cases = [
            (RouteMode::Scored, "scored"),
            (RouteMode::Pinned, "pinned"),
            (RouteMode::SlashPinned, "slash-pinned"),
            (RouteMode::Preset, "preset"),
            (RouteMode::ThresholdOverride, "threshold-override"),
            (RouteMode::Sticky, "sticky"),
            (RouteMode::BudgetDegraded, "budget-degraded"),
            (RouteMode::KeyScoped, "key-scoped"),
            (RouteMode::Canary, "canary"),
        ];
        for (mode, expected) in cases {
            assert_eq!(mode.as_str(), expected);
        }
        assert!(RouteMode::Pinned.is_pinned());
        assert!(RouteMode::SlashPinned.is_pinned());
        assert!(!RouteMode::Preset.is_pinned());
        assert!(!RouteMode::Scored.is_pinned());
    }
}
