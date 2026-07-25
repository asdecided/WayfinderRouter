import Foundation

/// Whether the transcript follows a streaming reply (UX-002).
///
/// The audited build scrolled to the bottom on every delta, so a reader who
/// scrolled up to re-read something was yanked back on the next token. The
/// desktop checklist already forbids exactly this: "later stream fragments do
/// not steal the selection or auto-scroll unless the user is following the
/// latest turn near the bottom" (`docs/desktop-fidelity.md`).
///
/// Kept free of SwiftUI so the rule itself is unit-testable.
struct TranscriptScrollState: Equatable {
  /// How close to the bottom still counts as following, in points.
  static let followThreshold: CGFloat = 48

  /// True while new content should pull the transcript down.
  private(set) var isFollowingTail = true
  /// True while the viewport is within `followThreshold` of the bottom.
  private(set) var isNearBottom = true

  /// The scroll-to-bottom affordance appears exactly when following stopped.
  var showsScrollToBottomControl: Bool {
    !isFollowingTail
  }

  /// The user moved the transcript.
  ///
  /// Scrolling away stops the follow; arriving back at the bottom resumes it,
  /// which is what makes the pill unnecessary once you are there.
  mutating func viewportChanged(isNearBottom: Bool) {
    self.isNearBottom = isNearBottom
    isFollowingTail = isNearBottom
  }

  /// New content arrived. Returns whether the transcript should scroll.
  mutating func contentDidGrow() -> Bool {
    isFollowingTail
  }

  /// The user asked to return to the newest content.
  mutating func returnToBottom() {
    isFollowingTail = true
    isNearBottom = true
  }

  /// A different conversation was opened: start at its tail again.
  mutating func conversationChanged() {
    isFollowingTail = true
    isNearBottom = true
  }

  /// The user submitted a turn, which is an explicit request to see the
  /// newest content even if they had scrolled away.
  mutating func didSubmitTurn() {
    returnToBottom()
  }
}
