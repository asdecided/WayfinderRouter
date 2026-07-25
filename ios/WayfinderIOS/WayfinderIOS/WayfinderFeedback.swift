import SwiftUI

/// The app's haptic vocabulary (UX-026).
///
/// One mapping, in one place. Every case is a *state change* the user did not
/// already feel through their own touch — a reply completing, a turn failing,
/// a destructive action taking effect. Nothing here fires on scrolling, and
/// nothing fires merely because a control was tapped: the tap is already felt.
enum WayfinderFeedbackKind: Equatable, Sendable {
  /// A turn was accepted and routing began.
  case messageSent
  /// A reply reached its terminal content.
  case responseCompleted
  /// A reply ended without completing, at the user's request or otherwise.
  case responseEnded
  /// A turn failed, or storage could not keep up.
  case failure
  /// A destructive action took effect.
  case destructiveConfirmed
  /// Content was placed on the pasteboard.
  case copied
  /// A selection moved to the end of its range and stopped.
  case selectionBoundary

  /// What VoiceOver says when this happens (UX-003).
  ///
  /// The same vocabulary drives touch and speech, so a state change can never
  /// be felt but not heard. Streaming start, completion, and failure are all
  /// here because none of them are visible non-visually.
  var announcement: String? {
    switch self {
    case .messageSent: "Wayfinder is responding"
    case .responseCompleted: "Reply complete"
    case .responseEnded: "Reply ended"
    case .failure: "Reply failed"
    case .copied: "Copied"
    case .destructiveConfirmed, .selectionBoundary: nil
    }
  }

  var sensoryFeedback: SensoryFeedback {
    switch self {
    case .messageSent: .impact(weight: .light, intensity: 0.7)
    case .responseCompleted: .success
    case .responseEnded: .impact(weight: .light, intensity: 0.5)
    case .failure: .error
    case .destructiveConfirmed: .impact(weight: .heavy, intensity: 0.8)
    case .copied: .success
    case .selectionBoundary: .selection
    }
  }
}

/// A feedback request carrying its own identity, so repeating the same kind
/// twice in a row still registers as two distinct events.
struct WayfinderFeedbackEvent: Equatable, Sendable {
  let sequence: Int
  let kind: WayfinderFeedbackKind
}

extension View {
  /// Plays the app's feedback vocabulary for model-driven state changes.
  /// Attach once, near the root of a screen — not per control.
  func wayfinderFeedback(_ event: WayfinderFeedbackEvent?) -> some View {
    sensoryFeedback(trigger: event) { _, current in
      current.map(\.kind.sensoryFeedback)
    }
  }

  /// Plays feedback for a state change owned by a view rather than the model,
  /// such as text reaching the pasteboard. Still routed through the shared
  /// vocabulary so there is only one mapping.
  func wayfinderFeedback<T: Equatable>(
    _ kind: WayfinderFeedbackKind,
    trigger: T,
    condition: @escaping (T, T) -> Bool
  ) -> some View {
    sensoryFeedback(kind.sensoryFeedback, trigger: trigger, condition: condition)
  }
}
