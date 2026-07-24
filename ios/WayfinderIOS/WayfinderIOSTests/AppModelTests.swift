import XCTest

@testable import WayfinderIOS

@MainActor
final class AppModelTests: XCTestCase {
  func testSimplePromptRoutesToOnDeviceCandidate() async {
    let model = AppModel()
    model.draft = "Hello"

    await model.previewRoute()

    guard case .routed(let preview) = model.routePreviewState else {
      return XCTFail("Expected a routed preview")
    }
    XCTAssertEqual(preview.destinationID, "device-preview")
    XCTAssertEqual(preview.executionSummary, "On this device")
    XCTAssertEqual(preview.score, 0.0)
    XCTAssertEqual(model.submittedPrompt, "Hello")
  }

  func testStructuredPromptRoutesToHostedCandidateWhenAllowed() async {
    let model = AppModel()
    model.draft = "# Plan\n\n## Steps\n- one\n- two\n- three\n1. first\n2. second"

    await model.previewRoute()

    guard case .routed(let preview) = model.routePreviewState else {
      return XCTFail("Expected a routed preview")
    }
    XCTAssertEqual(preview.destinationID, "hosted-preview")
    XCTAssertEqual(preview.executionSummary, "Hosted cloud")
    XCTAssertEqual(preview.score, 0.15)
  }

  func testOnDeviceOnlyExcludesHostedRecommendation() async {
    let model = AppModel()
    model.privacyPosture = .onDeviceOnly
    model.draft = "# Plan\n\n## Steps\n- one\n- two\n- three\n1. first\n2. second"

    await model.previewRoute()

    guard case .unavailable(let message) = model.routePreviewState else {
      return XCTFail("Expected no eligible route")
    }
    XCTAssertTrue(message.contains("On-Device Only"))
  }

  func testEmptyPromptDoesNotRoute() async {
    let model = AppModel()
    model.draft = " \n "

    await model.previewRoute()

    XCTAssertEqual(
      model.routePreviewState,
      .unavailable("Enter a message to send.")
    )
  }

  func testRootTabsRemainCompleteAndStable() {
    XCTAssertEqual(
      AppTab.allCases.map(\.title),
      ["Chat", "Threads", "Destinations", "Settings"]
    )
  }

  func testNewChatClearsTransientConversationState() async {
    let model = AppModel()
    model.selectedTab = .settings
    model.draft = "Hello"
    await model.previewRoute()

    await model.startNewChat()

    XCTAssertEqual(model.selectedTab, .chat)
    XCTAssertEqual(model.draft, "")
    XCTAssertNil(model.submittedPrompt)
    XCTAssertEqual(model.routePreviewState, .idle)
  }

  func testSavedConversationRestoresIntoNewModel() async {
    let store = InMemoryConversationStore()
    let timestamp = Date(timeIntervalSince1970: 1_700_000_000)
    let firstModel = AppModel(
      conversationStore: store,
      now: { timestamp }
    )
    firstModel.draft = "Restore this conversation"

    await firstModel.previewRoute()

    let restoredModel = AppModel(conversationStore: store)
    await restoredModel.restoreConversations()

    XCTAssertEqual(restoredModel.threads.count, 1)
    XCTAssertEqual(
      restoredModel.activeThread?.messages.first?.content,
      "Restore this conversation"
    )
    XCTAssertEqual(restoredModel.submittedPrompt, "Restore this conversation")
  }

  func testSecondTurnAppendsToActiveConversation() async {
    let store = InMemoryConversationStore()
    let model = AppModel(conversationStore: store)
    model.draft = "First turn"
    await model.previewRoute()
    model.draft = "Second turn"

    await model.previewRoute()

    XCTAssertEqual(model.threads.count, 1)
    XCTAssertEqual(
      model.activeThread?.messages
        .filter { $0.role == .user }
        .map(\.content),
      ["First turn", "Second turn"]
    )
  }

  func testDeterministicProviderStreamsOrderedAssistantReply() async {
    let provider = DeterministicMockProvider(
      configuration: .init(
        outcome: .response(chunks: ["First ", "second ", "third."]),
        delay: .zero
      )
    )
    let model = AppModel(providerExecutor: provider)
    model.draft = "Stream this"

    await model.sendMessage()

    let messages = model.activeThread?.messages ?? []
    XCTAssertEqual(messages.map(\.role), [.user, .assistant])
    XCTAssertEqual(messages[1].content, "First second third.")
    XCTAssertEqual(messages[1].status, .completed)
    XCTAssertNotNil(messages[1].routeReceipt)
    XCTAssertEqual(model.executionPhase, .idle)
  }

  func testProviderFailurePreservesPartialReplyAndOffersRetryState() async {
    let provider = DeterministicMockProvider(
      configuration: .init(
        outcome: .failure(
          afterChunks: ["Partial reply"],
          message: "The deterministic provider rejected this request."
        ),
        delay: .zero
      )
    )
    let model = AppModel(providerExecutor: provider)
    model.draft = "Fail after output"

    await model.sendMessage()

    let assistant = model.activeThread?.messages.last
    XCTAssertEqual(assistant?.role, .assistant)
    XCTAssertEqual(assistant?.content, "Partial reply")
    XCTAssertEqual(assistant?.status, .failed)
  }

  func testStoppingGenerationProducesOneStoppedAssistantMessage() async {
    let provider = DeterministicMockProvider(
      configuration: .init(
        outcome: .response(chunks: ["Too ", "slow"]),
        delay: .seconds(5)
      )
    )
    let model = AppModel(providerExecutor: provider)
    model.draft = "Stop this"
    let sendTask = Task {
      await model.sendMessage()
    }

    await waitUntil {
      if case .streaming = model.executionPhase {
        return true
      }
      return false
    }
    await model.stopGenerating()
    await sendTask.value

    let assistantMessages =
      model.activeThread?.messages.filter { $0.role == .assistant } ?? []
    XCTAssertEqual(assistantMessages.count, 1)
    XCTAssertEqual(assistantMessages[0].status, .stopped)
    XCTAssertEqual(model.executionPhase, .idle)
  }

  func testRetryPreservesFailedAttemptAndCreatesNewAttempt() async {
    let provider = DeterministicMockProvider(
      configuration: .init(
        outcome: .failure(afterChunks: [], message: "Preview failed."),
        delay: .zero
      )
    )
    let model = AppModel(providerExecutor: provider)
    model.draft = "Try this"
    await model.sendMessage()
    let failedID = try! XCTUnwrap(model.activeThread?.messages.last?.id)

    await model.retry(messageID: failedID)

    let messages = model.activeThread?.messages ?? []
    XCTAssertEqual(messages.count, 4)
    XCTAssertEqual(
      messages.filter { $0.role == .assistant }.map(\.status),
      [.failed, .failed]
    )
    XCTAssertEqual(
      messages.filter { $0.role == .user }.map(\.content),
      [
        "Try this", "Try this",
      ])
    XCTAssertEqual(messages.first { $0.id == failedID }?.status, .failed)
  }

  func testRestoreMarksPendingAssistantMessageInterrupted() async {
    let store = InMemoryConversationStore()
    let threadID = UUID()
    let pendingID = UUID()
    await store.save(
      thread: ConversationThreadSnapshot(
        id: threadID,
        title: "Interrupted",
        createdAt: .distantPast,
        updatedAt: .distantPast,
        messages: [
          ConversationMessageSnapshot(
            id: UUID(),
            role: .user,
            content: "Continue",
            createdAt: .distantPast,
            status: .completed,
            routeReceipt: nil
          ),
          ConversationMessageSnapshot(
            id: pendingID,
            role: .assistant,
            content: "Partial",
            createdAt: .distantPast,
            status: .pending,
            routeReceipt: nil
          ),
        ],
        draft: ""
      )
    )
    await store.save(
      workspace: ConversationWorkspaceSnapshot(
        activeThreadID: threadID,
        draft: "",
        retentionDays: nil,
        updatedAt: .distantPast
      )
    )
    let model = AppModel(conversationStore: store)

    await model.restoreConversations()

    XCTAssertEqual(
      model.activeThread?.messages.first { $0.id == pendingID }?.status,
      .interrupted
    )
  }

  func testNewChatDraftRestoresWithoutCreatingThread() async {
    let store = InMemoryConversationStore()
    let firstModel = AppModel(conversationStore: store)
    firstModel.draft = "Unsent draft"
    await firstModel.saveDraft()

    let restoredModel = AppModel(conversationStore: store)
    await restoredModel.restoreConversations()

    XCTAssertEqual(restoredModel.draft, "Unsent draft")
    XCTAssertTrue(restoredModel.threads.isEmpty)
    XCTAssertNil(restoredModel.activeThreadID)
  }

  func testRetentionPolicyPrunesOldConversationOnRestore() async {
    let store = InMemoryConversationStore()
    let now = Date(timeIntervalSince1970: 10_000_000)
    let oldThread = ConversationThreadSnapshot(
      id: UUID(),
      title: "Old",
      createdAt: now.addingTimeInterval(-100 * 86_400),
      updatedAt: now.addingTimeInterval(-100 * 86_400),
      messages: [],
      draft: ""
    )
    await store.save(thread: oldThread)
    await store.save(
      workspace: ConversationWorkspaceSnapshot(
        activeThreadID: nil,
        draft: "",
        retentionDays: 30,
        updatedAt: now
      )
    )
    let model = AppModel(
      conversationStore: store,
      now: { now }
    )

    await model.restoreConversations()

    XCTAssertEqual(model.retentionPolicy, .thirtyDays)
    XCTAssertTrue(model.threads.isEmpty)
  }

  private func waitUntil(
    timeout: Duration = .seconds(1),
    condition: @escaping @MainActor () -> Bool
  ) async {
    let clock = ContinuousClock()
    let deadline = clock.now.advanced(by: timeout)

    while !condition(), clock.now < deadline {
      await Task.yield()
    }

    XCTAssertTrue(condition(), "Timed out waiting for state transition")
  }
}
