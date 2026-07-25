import Foundation
import WayfinderRoutingBridge
import XCTest

@testable import WayfinderIOS

/// UX-002. The audited build scrolled to the bottom on every delta, so a
/// reader who scrolled up was yanked back by the next token. These cover the
/// rule the desktop checklist already states: follow the tail only while the
/// reader is at it.
final class TranscriptScrollStateTests: XCTestCase {

  func testStartsFollowingTheTail() {
    let state = TranscriptScrollState()

    XCTAssertTrue(state.isFollowingTail)
    XCTAssertFalse(state.showsScrollToBottomControl)
  }

  func testScrollingAwayStopsFollowingAndOffersAWayBack() {
    var state = TranscriptScrollState()

    state.viewportChanged(isNearBottom: false)

    XCTAssertFalse(state.isFollowingTail)
    XCTAssertTrue(state.showsScrollToBottomControl)
  }

  func testContentGrowthDoesNotScrollWhileTheReaderIsAway() {
    var state = TranscriptScrollState()
    state.viewportChanged(isNearBottom: false)

    XCTAssertFalse(
      state.contentDidGrow(),
      "a delta stole the reader's position"
    )
  }

  func testContentGrowthScrollsWhileFollowing() {
    var state = TranscriptScrollState()

    XCTAssertTrue(state.contentDidGrow())
  }

  func testScrollingBackToTheBottomResumesFollowing() {
    var state = TranscriptScrollState()
    state.viewportChanged(isNearBottom: false)

    state.viewportChanged(isNearBottom: true)

    XCTAssertTrue(state.isFollowingTail)
    XCTAssertFalse(
      state.showsScrollToBottomControl,
      "the control stayed visible after returning to the bottom"
    )
    XCTAssertTrue(state.contentDidGrow())
  }

  func testReturnToBottomResumesFollowing() {
    var state = TranscriptScrollState()
    state.viewportChanged(isNearBottom: false)

    state.returnToBottom()

    XCTAssertTrue(state.isFollowingTail)
    XCTAssertFalse(state.showsScrollToBottomControl)
  }

  func testOpeningAnotherConversationStartsAtItsTail() {
    var state = TranscriptScrollState()
    state.viewportChanged(isNearBottom: false)

    state.conversationChanged()

    XCTAssertTrue(state.isFollowingTail)
  }

  func testSubmittingATurnResumesFollowingEvenAfterScrollingAway() {
    // Sending is an explicit request to watch the reply arrive.
    var state = TranscriptScrollState()
    state.viewportChanged(isNearBottom: false)

    state.didSubmitTurn()

    XCTAssertTrue(state.isFollowingTail)
    XCTAssertTrue(state.contentDidGrow())
  }

  func testRepeatedDeltasWhileAwayNeverResumeFollowingOnTheirOwn() {
    var state = TranscriptScrollState()
    state.viewportChanged(isNearBottom: false)

    for _ in 0..<50 {
      XCTAssertFalse(state.contentDidGrow())
    }
    XCTAssertTrue(state.showsScrollToBottomControl)
  }
}

/// UX-010 and UX-017. Receipt copy must speak the contract's execution
/// grammar, and the detail must explain the score rather than print it.
final class RouteReceiptTests: XCTestCase {

  func testReceiptSummaryUsesRanGrammarAndNamesTheDestination() {
    let receipt = StoredRouteReceipt(
      destinationID: "apple-foundation-models",
      destinationName: "Apple On-Device",
      score: 0.08,
      recommendation: "local",
      executionSummary: "On this device",
      boundaryID: ExecutionBoundary.onDevice.storageID
    )

    XCTAssertEqual(
      receipt.receiptSummary,
      "Ran on this device · Apple On-Device"
    )
  }

  func testHostedReceiptReadsAsHostedCloud() {
    let receipt = StoredRouteReceipt(
      destinationID: "openai",
      destinationName: "GPT-5.6",
      score: 0.7,
      recommendation: "cloud",
      executionSummary: "Hosted cloud",
      boundaryID: ExecutionBoundary.hosted.storageID
    )

    XCTAssertTrue(receipt.receiptSummary.hasPrefix("Ran in hosted cloud"))
  }

  func testAReceiptWrittenBeforeBoundaryWasStoredStillReadsAsRan() {
    // Records persisted by earlier builds have no boundary.
    let receipt = StoredRouteReceipt(
      destinationID: "device-preview",
      destinationName: "On-device preview",
      score: 0.05,
      recommendation: "local",
      executionSummary: "On this device"
    )

    XCTAssertNil(receipt.boundary)
    XCTAssertTrue(
      receipt.receiptSummary.hasPrefix("Ran "),
      "legacy receipt lost the contract grammar: \(receipt.receiptSummary)"
    )
  }

  func testScoreExplanationSaysSomethingBeyondTheNumber() {
    let low = StoredRouteReceipt(
      destinationID: "a",
      destinationName: "A",
      score: 0.04,
      recommendation: "local",
      executionSummary: "On this device"
    )
    let high = StoredRouteReceipt(
      destinationID: "b",
      destinationName: "B",
      score: 0.82,
      recommendation: "cloud",
      executionSummary: "Hosted cloud"
    )

    XCTAssertTrue(low.scoreExplanation.contains("0.04"))
    XCTAssertNotEqual(low.scoreExplanation, high.scoreExplanation)
    XCTAssertTrue(high.scoreExplanation.contains("cloud"))
    XCTAssertGreaterThan(low.scoreExplanation.count, 20)
  }

  func testExclusionsCarryPlainLanguageReasons() {
    let plan = RoutePlan(
      schemaVersion: 1,
      requestId: UUID().uuidString,
      score: 0.2,
      recommendation: "local",
      selectedDestinationId: "chosen",
      fallbackDestinationIds: [],
      candidates: [
        CandidateAssessment(destinationId: "chosen", exclusions: []),
        CandidateAssessment(
          destinationId: "blocked",
          exclusions: [.privacyBoundaryDenied, .providerNotReady]
        ),
      ]
    )

    let exclusions = plan.exclusions { id in
      id == "blocked" ? "Hosted preview" : nil
    }

    XCTAssertEqual(exclusions.count, 1)
    XCTAssertEqual(exclusions[0].destinationName, "Hosted preview")
    XCTAssertEqual(
      exclusions[0].reasons,
      ["blocked by the current privacy posture", "not connected yet"]
    )
    // Reason codes must not surface as raw enum names.
    for reason in exclusions[0].reasons {
      XCTAssertFalse(reason.contains("privacyBoundaryDenied"))
    }
  }

  func testBoundaryStorageIdentifiersRoundTrip() {
    for boundary in [
      ExecutionBoundary.onDevice, .localNetwork, .hosted,
    ] {
      XCTAssertEqual(
        ExecutionBoundary(storageID: boundary.storageID)?.storageID,
        boundary.storageID
      )
    }
    XCTAssertNil(ExecutionBoundary(storageID: "not-a-boundary"))
  }

  func testEveryPrivacyPostureExplainsItsConsequence() {
    // UX-018: three bare titles told the user nothing about what changes.
    for posture in PrivacyPostureOption.allCases {
      XCTAssertGreaterThan(
        posture.consequence.count,
        40,
        "\(posture.title) has no consequence copy"
      )
      XCTAssertNotEqual(posture.consequence, posture.title)
    }
  }

  func testFeedbackVocabularyAnnouncesEveryStreamingLifecycleChange() {
    // UX-003: start, completion, and failure are invisible non-visually.
    XCTAssertNotNil(WayfinderFeedbackKind.messageSent.announcement)
    XCTAssertNotNil(WayfinderFeedbackKind.responseCompleted.announcement)
    XCTAssertNotNil(WayfinderFeedbackKind.responseEnded.announcement)
    XCTAssertNotNil(WayfinderFeedbackKind.failure.announcement)
  }
}
