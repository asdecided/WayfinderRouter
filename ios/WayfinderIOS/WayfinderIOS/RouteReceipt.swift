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

  /// A one-line reading of the deterministic score, so the number is not the
  /// only thing the sheet says about it.
  var scoreExplanation: String {
    let formatted = score.formatted(.number.precision(.fractionLength(2)))
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
