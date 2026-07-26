import Foundation
import XCTest

@testable import WayfinderIOS

/// UX-013. First launch dropped into an empty chat with no explanation, and
/// the only configuration flow in the app ended without payoff.
@MainActor
final class FirstRunTests: XCTestCase {

  func testAFreshInstallHasNotCompletedFirstRun() async {
    let model = AppModel(conversationStore: InMemoryConversationStore())

    await model.restoreConversations()

    XCTAssertFalse(model.hasCompletedFirstRun)
  }

  func testTheChooserStaysDownUntilTheRestoreHasAnswered() async {
    // `hasCompletedFirstRun` reads "not yet" for the whole of a cold launch,
    // because the stored value only arrives from the restore. Presenting on
    // it alone flashed onboarding at every returning user, every launch.
    let store = InMemoryConversationStore()
    let first = AppModel(conversationStore: store)
    await first.restoreConversations()
    await first.completeFirstRun()

    let relaunched = AppModel(conversationStore: store)
    XCTAssertFalse(
      relaunched.shouldPresentFirstRun,
      "the cover was presented before the restore could say otherwise"
    )

    await relaunched.restoreConversations()
    XCTAssertFalse(relaunched.shouldPresentFirstRun)
  }

  func testAGenuineFirstLaunchStillPresentsTheChooser() async {
    let model = AppModel(conversationStore: InMemoryConversationStore())

    await model.restoreConversations()

    XCTAssertTrue(model.shouldPresentFirstRun)
  }

  func testCompletingFirstRunSurvivesRelaunch() async {
    let store = InMemoryConversationStore()
    let first = AppModel(conversationStore: store)
    await first.restoreConversations()

    await first.completeFirstRun()

    XCTAssertTrue(first.hasCompletedFirstRun)

    let relaunched = AppModel(conversationStore: store)
    await relaunched.restoreConversations()
    XCTAssertTrue(
      relaunched.hasCompletedFirstRun,
      "the chooser would reappear on every launch"
    )
  }

  func testSkippingEverythingStillLeavesAUsableApp() async {
    // The roadmap requires a useful no-destination state, not a gate.
    let store = InMemoryConversationStore()
    let model = AppModel(
      conversationStore: store,
      destinations: .liveDirectProviders
    )
    await model.restoreConversations()

    await model.completeFirstRun()

    XCTAssertTrue(model.hasCompletedFirstRun)
    XCTAssertTrue(model.readyDestinations.isEmpty)
    XCTAssertEqual(
      model.destinationReadinessSummary,
      "No destination is connected yet."
    )
    model.draft = "still usable"
    XCTAssertTrue(model.canSendMessage)
  }

  func testASavedKeyChangesTheReadinessChatReports() async {
    // The audited build flipped a badge on the key screen and nothing else.
    let model = AppModel(
      credentialStore: InMemoryCredentialStore(),
      destinations: .liveDirectProviders
    )
    let before = model.destinationReadinessSummary

    _ = await model.saveAPIKey(
      "temporary-key",
      for: APIKeyProviderDescriptor.supported[0]
    )

    XCTAssertNotEqual(
      model.destinationReadinessSummary,
      before,
      "saving a key produced no visible state outside the key screens"
    )
    XCTAssertFalse(model.readyDestinations.isEmpty)
  }

  func testSavingAKeyStillDoesNotChangeAutomaticRouting() async {
    // Guardrail: readiness may change, routing may not.
    let model = AppModel(
      credentialStore: InMemoryCredentialStore(),
      destinations: .liveDirectProviders
    )

    _ = await model.saveAPIKey(
      "temporary-key",
      for: APIKeyProviderDescriptor.supported[0]
    )

    XCTAssertNil(model.selectedDestinationID)
    XCTAssertTrue(
      model.readyDestinations
        .filter { $0.boundary != .onDevice }
        .allSatisfy { !$0.automaticEligible },
      "a saved key silently entered Automatic routing"
    )
  }

  func testOnDeviceIsEnrolledInAutomaticWithoutBeingAsked() {
    // The one case where a default costs nothing to consent to: on-device
    // execution sends nothing anywhere. Every destination used to be built
    // ineligible, so Automatic could never select anything and an unpinned
    // turn always failed while the copy claimed otherwise.
    let model = AppModel(destinations: .liveDirectProviders)

    let onDevice = model.destinations.first { $0.boundary == .onDevice }
    XCTAssertEqual(onDevice?.automaticEligible, true)
    XCTAssertTrue(model.hasAutomaticDestination)
  }

  func testOnDeviceEnrolmentIsNotSomethingTheUserCanBeAskedToWithdraw() {
    let model = AppModel(destinations: .liveDirectProviders)

    let onDevice = model.destinations.first { $0.boundary == .onDevice }
    XCTAssertNotNil(onDevice)
    XCTAssertFalse(
      model.canEnrolInAutomatic(onDevice!),
      "offering a toggle that cannot move implies the default is negotiable"
    )
  }

  func testAHostedDestinationJoinsAutomaticOnlyWhenTheUserSaysSo() async {
    let store = InMemoryConversationStore()
    let model = AppModel(
      conversationStore: store,
      destinations: .liveDirectProviders
    )
    await model.restoreConversations()

    let hosted = model.destinations.first { $0.boundary == .hosted }
    XCTAssertNotNil(hosted)
    XCTAssertFalse(model.isEnrolledInAutomatic(hosted!))

    await model.setEnrolledInAutomatic(true, destinationID: hosted!.id)

    let enrolled = model.destinations.first { $0.id == hosted!.id }
    XCTAssertEqual(enrolled?.automaticEligible, true)

    // And it survives a relaunch, or the consent was theatre.
    let relaunched = AppModel(
      conversationStore: store,
      destinations: .liveDirectProviders
    )
    await relaunched.restoreConversations()
    XCTAssertTrue(
      relaunched.destinations.first { $0.id == hosted!.id }?.automaticEligible
        ?? false
    )
  }

  func testASaveBeforeTheRestoreDoesNotWithdrawStoredConsent() async {
    // The write path had to learn the same lesson as the first-run flag: a
    // workspace save that lands before the restore has read stored consent
    // would write an empty list over it and silently un-enrol everything.
    let store = InMemoryConversationStore()
    let first = AppModel(
      conversationStore: store,
      destinations: .liveDirectProviders
    )
    await first.restoreConversations()
    let hosted = first.destinations.first { $0.boundary == .hosted }!
    await first.setEnrolledInAutomatic(true, destinationID: hosted.id)

    let relaunched = AppModel(
      conversationStore: store,
      destinations: .liveDirectProviders
    )
    relaunched.draft = "typed before the restore finished"
    await relaunched.saveDraft()
    await relaunched.restoreConversations()

    XCTAssertEqual(
      relaunched.destinations.first { $0.id == hosted.id }?.automaticEligible,
      true,
      "an early save withdrew a destination the user had enrolled"
    )
  }

  func testWithdrawingAHostedDestinationTakesEffectAndPersists() async {
    let store = InMemoryConversationStore()
    let model = AppModel(
      conversationStore: store,
      destinations: .liveDirectProviders
    )
    await model.restoreConversations()
    let hosted = model.destinations.first { $0.boundary == .hosted }!
    await model.setEnrolledInAutomatic(true, destinationID: hosted.id)

    await model.setEnrolledInAutomatic(false, destinationID: hosted.id)

    XCTAssertEqual(
      model.destinations.first { $0.id == hosted.id }?.automaticEligible,
      false
    )

    let relaunched = AppModel(
      conversationStore: store,
      destinations: .liveDirectProviders
    )
    await relaunched.restoreConversations()
    XCTAssertEqual(
      relaunched.destinations.first { $0.id == hosted.id }?.automaticEligible,
      false
    )
  }
}

/// UX-014 and UX-022. Threads had no search, rename, pin, or archive; titles
/// were the first ~49 characters of the opening prompt, never revisited.
@MainActor
final class ThreadManagementTests: XCTestCase {

  private func seed(
    _ store: InMemoryConversationStore,
    titles: [String],
    from start: Date = Date(timeIntervalSince1970: 1_000_000)
  ) async -> [UUID] {
    var ids: [UUID] = []
    for (offset, title) in titles.enumerated() {
      let id = UUID()
      ids.append(id)
      await store.save(
        thread: ConversationThreadSnapshot(
          id: id,
          title: title,
          createdAt: start.addingTimeInterval(Double(offset)),
          updatedAt: start.addingTimeInterval(Double(offset)),
          messages: [
            ConversationMessageSnapshot(
              id: UUID(),
              role: .user,
              content: "body of \(title)",
              createdAt: start,
              status: .completed,
              routeReceipt: nil
            )
          ],
          draft: ""
        )
      )
    }
    return ids
  }

  func testRenamingAThreadPersists() async {
    let store = InMemoryConversationStore()
    let ids = await seed(store, titles: ["Original"])
    let model = AppModel(conversationStore: store)
    await model.restoreConversations()

    await model.renameThread(id: ids[0], to: "  Chosen name  ")

    XCTAssertEqual(model.threads.first?.title, "Chosen name")
    let persisted = await store.thread(id: ids[0])
    XCTAssertEqual(persisted?.title, "Chosen name")
    XCTAssertEqual(persisted?.hasCustomTitle, true)
  }

  func testRenamingToBlankIsRejected() async {
    let store = InMemoryConversationStore()
    let ids = await seed(store, titles: ["Original"])
    let model = AppModel(conversationStore: store)
    await model.restoreConversations()

    await model.renameThread(id: ids[0], to: "   ")

    XCTAssertEqual(model.threads.first?.title, "Original")
  }

  func testPinnedThreadsSortAheadOfMoreRecentOnes() async {
    let store = InMemoryConversationStore()
    let ids = await seed(store, titles: ["Oldest", "Middle", "Newest"])
    let model = AppModel(conversationStore: store)
    await model.restoreConversations()
    XCTAssertEqual(model.threads.first?.title, "Newest")

    await model.setPinned(true, threadID: ids[0])

    XCTAssertEqual(model.threads.first?.title, "Oldest")
    XCTAssertEqual(model.pinnedThreads.map(\.title), ["Oldest"])
    XCTAssertEqual(model.unpinnedThreads.map(\.title), ["Newest", "Middle"])
  }

  func testUnpinningRestoresRecencyOrder() async {
    let store = InMemoryConversationStore()
    let ids = await seed(store, titles: ["Oldest", "Middle", "Newest"])
    let model = AppModel(conversationStore: store)
    await model.restoreConversations()
    await model.setPinned(true, threadID: ids[0])

    await model.setPinned(false, threadID: ids[0])

    XCTAssertEqual(
      model.threads.map(\.title),
      ["Newest", "Middle", "Oldest"]
    )
    XCTAssertTrue(model.pinnedThreads.isEmpty)
  }

  func testPinStateSurvivesRelaunch() async {
    let store = InMemoryConversationStore()
    let ids = await seed(store, titles: ["Keep me"])
    let model = AppModel(conversationStore: store)
    await model.restoreConversations()
    await model.setPinned(true, threadID: ids[0])

    let relaunched = AppModel(conversationStore: store)
    await relaunched.restoreConversations()

    XCTAssertEqual(relaunched.pinnedThreads.map(\.title), ["Keep me"])
  }

  func testSearchMatchesTitlesAndMessageBodies() async {
    let store = InMemoryConversationStore()
    _ = await seed(store, titles: ["Groceries", "Taxes"])
    let model = AppModel(conversationStore: store)
    await model.restoreConversations()

    XCTAssertEqual(
      model.threads(matching: "grocer").map(\.title),
      ["Groceries"]
    )
    XCTAssertEqual(
      model.threads(matching: "body of Taxes").map(\.title),
      ["Taxes"]
    )
    XCTAssertEqual(model.threads(matching: "  ").count, 2)
    XCTAssertTrue(model.threads(matching: "nothing here").isEmpty)
  }

  func testSearchIsCaseInsensitive() async {
    let store = InMemoryConversationStore()
    _ = await seed(store, titles: ["Groceries"])
    let model = AppModel(conversationStore: store)
    await model.restoreConversations()

    XCTAssertEqual(model.threads(matching: "GROCERIES").count, 1)
  }

  func testTheDrawerNoLongerCapsAtTwelveThreads() async {
    let store = InMemoryConversationStore()
    _ = await seed(store, titles: (1...20).map { "Thread \($0)" })
    let model = AppModel(conversationStore: store)

    await model.restoreConversations()

    XCTAssertEqual(model.threads.count, 20)
    XCTAssertGreaterThan(
      model.unpinnedThreads.count,
      12,
      "the sidebar source is still capped at twelve"
    )
  }

  func testRelativeTimestampsReadAsRelative() async {
    // UX-022: both references use relative time in conversation lists.
    XCTAssertEqual(Date().relativeDescription, "Just now")
    let anHourAgo = Date().addingTimeInterval(-3600)
    XCTAssertNotEqual(anHourAgo.relativeDescription, "Just now")
    // Old enough that relative wording stops being useful.
    let longAgo = Date().addingTimeInterval(-40 * 24 * 60 * 60)
    XCTAssertFalse(longAgo.relativeDescription.contains("ago"))
  }
}
