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
      model.readyDestinations.allSatisfy { !$0.automaticEligible },
      "a saved key silently entered Automatic routing"
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
