import Foundation
import WayfinderRoutingBridge

/// Receipt language and routing explanation (UX-010, UX-017).
///
/// WF-DESIGN-0020 prescribes execution-boundary grammar — "Ran on this
/// iPhone · Apple On-Device" — not "Routed to …". "Ran" states what happened;
/// "routed to" states only what was chosen, which is the weaker claim for the
/// product's central assertion.

extension ExecutionBoundary {
  /// Stable identifier persisted with a receipt.
  var storageID: String {
    switch self {
    case .onDevice: "onDevice"
    case .localNetwork: "localNetwork"
    case .hosted: "hosted"
    }
  }

  init?(storageID: String) {
    switch storageID {
    case "onDevice": self = .onDevice
    case "localNetwork": self = .localNetwork
    case "hosted": self = .hosted
    default: return nil
    }
  }
}

extension ExclusionReason {
  /// Plain-language consequence, not the enum name.
  var explanation: String {
    switch self {
    case .providerNotReady:
      "not connected yet"
    case .privacyBoundaryDenied:
      "blocked by the current privacy posture"
    case .textUnsupported:
      "cannot handle text prompts"
    case .contextWindowUnknown:
      "context window is unknown"
    case .contextWindowTooSmall:
      "context window is too small for this message"
    case .imageInputUnsupported:
      "cannot accept images"
    case .toolsUnsupported:
      "cannot use tools"
    case .streamingUnsupported:
      "cannot stream a reply"
    case .automaticNotAllowed:
      "not enabled for Automatic routing"
    }
  }
}

extension StoredRouteReceipt {
  var boundary: ExecutionBoundary? {
    boundaryID.flatMap(ExecutionBoundary.init(storageID:))
  }

  /// The compact transcript chip: "Ran on this device · Apple On-Device".
  ///
  /// Falls back to the stored summary for receipts written before the
  /// boundary was persisted, so old turns never render an empty claim.
  var receiptSummary: String {
    guard let boundary else {
      return "Ran \(executionSummary.lowercased()) · \(destinationName)"
    }
    return "\(boundary.receiptPhrase) · \(destinationName)"
  }

  /// True when this turn ran against a destination the user pinned, rather
  /// than one Automatic selected. The routing core marks the single-tier
  /// engine it builds for a pinned run with this recommendation.
  var wasPinnedByUser: Bool {
    recommendation == Self.pinnedRecommendation
  }

  static let pinnedRecommendation = "pinned"

  /// How the tier reads in the detail sheet. "pinned" is an internal marker,
  /// not a tier the router picked, and showing it raw under the heading
  /// "Routing tier" credited the router with the user's decision.
  var tierDescription: String {
    wasPinnedByUser ? "Not used — you chose this destination" : recommendation
  }

  /// A one-line reading of the deterministic score, so the number is not the
  /// only thing the sheet says about it.
  ///
  /// A pinned run never claims Wayfinder chose anything: the score was still
  /// computed on this device, but it decided nothing, and saying otherwise
  /// attributes a human decision to the router.
  var scoreExplanation: String {
    let formatted = score.formatted(.number.precision(.fractionLength(2)))
    guard !wasPinnedByUser else {
      return
        "Scored \(formatted) on this device. You chose this destination, so the score did not affect where this ran."
    }
    switch score {
    case ..<0.10:
      return
        "Scored \(formatted) — straightforward enough for the cheapest capable destination."
    case ..<0.45:
      return
        "Scored \(formatted) — moderate complexity, so Wayfinder chose the \(recommendation) tier."
    default:
      return
        "Scored \(formatted) — demanding enough to justify the \(recommendation) tier."
    }
  }

  var hasRoutingExplanation: Bool {
    !(excluded ?? []).isEmpty || !(fallbackDestinationNames ?? []).isEmpty
  }
}

extension RoutePlan {
  /// Destinations the core rejected before scoring, named and explained.
  func exclusions(
    naming displayName: (String) -> String?
  ) -> [StoredRouteExclusion] {
    candidates.compactMap { candidate in
      guard !candidate.exclusions.isEmpty else {
        return nil
      }
      return StoredRouteExclusion(
        destinationName: displayName(candidate.destinationId)
          ?? candidate.destinationId,
        reasons: candidate.exclusions.map(\.explanation)
      )
    }
  }
}
