import Foundation
import XCTest

@testable import WayfinderIOS

/// UX-021 and UX-008. The audited build re-encoded and saved the entire thread
/// on every streamed delta, and locked navigation for the duration of a reply.
@MainActor
final class ChatStreamingPersistenceTests: XCTestCase {

  // MARK: - Checkpointing

  func testStreamingDoesNotWriteOncePerDelta() async {
    let chunks = Array(repeating: "token ", count: 200)
    let store = CountingConversationStore()
    let model = AppModel(
      conversationStore: store,
      providerExecutor: DeterministicMockProvider(
        configuration: .init(outcome: .response(chunks: chunks), delay: .zero)
      ),
      checkpointPolicy: .standard
    )
    model.draft = "Write at length"

    await model.sendMessage()

    let writes = await store.threadWriteCount
    XCTAssertLessThan(
      writes,
      chunks.count / 4,
      "streaming performed \(writes) thread writes for \(chunks.count) deltas"
    )
    XCTAssertEqual(
      model.activeThread?.messages.last?.content,
      chunks.joined()
    )
  }

  func testTerminalStateIsAlwaysPersistedRegardlessOfCadence() async {
    let store = CountingConversationStore()
    let model = AppModel(
      conversationStore: store,
      providerExecutor: DeterministicMockProvider(
        configuration: .init(
          outcome: .response(chunks: ["one ", "two"]),
          delay: .zero
        )
      ),
      // A cadence that would never fire on its own for so few deltas.
      checkpointPolicy: StreamingCheckpointPolicy(
        deltaInterval: 10_000,
        minimumInterval: .seconds(3600)
      )
    )
    model.draft = "Short reply"

    await model.sendMessage()

    let persisted = await store.storedThreads().first
    XCTAssertEqual(persisted?.messages.last?.content, "one two")
    XCTAssertEqual(persisted?.messages.last?.status, .completed)
  }

  func testStoppedReplyPersistsItsPartialContent() async {
    let store = CountingConversationStore()
    let model = AppModel(
      conversationStore: store,
      providerExecutor: DeterministicMockProvider(
        configuration: .init(
          outcome: .response(chunks: ["kept ", "then ", "stopped"]),
          delay: .milliseconds(60)
        )
      ),
      checkpointPolicy: StreamingCheckpointPolicy(
        deltaInterval: 10_000,
        minimumInterval: .seconds(3600)
      )
    )
    model.draft = "Stop midway"
    let send = Task { await model.sendMessage() }

    await waitUntil {
      (model.activeThread?.messages.last?.content.isEmpty == false)
    }
    await model.stopGenerating()
    await send.value

    let persisted = await store.storedThreads().first
    XCTAssertEqual(persisted?.messages.last?.status, .stopped)
    XCTAssertEqual(
      persisted?.messages.last?.content,
      model.activeThread?.messages.last?.content,
      "the stopped reply was persisted with different content than it shows"
    )
  }

  func testAReplyLostMidStreamReconcilesAsInterruptedOnRestore() async {
    // Checkpointing must not weaken interruption recovery: the assistant slot
    // is persisted before any content arrives, so a lost process still leaves
    // a message that restore can settle truthfully.
    let store = CountingConversationStore()
    let model = AppModel(
      conversationStore: store,
      providerExecutor: DeterministicMockProvider(
        configuration: .init(
          outcome: .response(chunks: ["never ", "finishes"]),
          delay: .seconds(30)
        )
      )
    )
    model.draft = "Interrupt me"
    let send = Task { await model.sendMessage() }
    await waitUntil {
      if case .streaming = model.executionPhase { return true }
      return false
    }

    // Simulate relaunch against the same store while the turn is in flight.
    let restored = AppModel(conversationStore: store)
    await restored.restoreConversations()

    send.cancel()
    await model.stopGenerating()
    await send.value

    let assistant = restored.activeThread?.messages.last
    XCTAssertEqual(assistant?.role, .assistant)
    XCTAssertEqual(assistant?.status, .interrupted)
  }

  // MARK: - Navigation during generation

  func testStartingANewChatDuringGenerationIsNotBlocked() async {
    let model = AppModel(
      providerExecutor: DeterministicMockProvider(
        configuration: .init(
          outcome: .response(chunks: ["slow ", "reply"]),
          delay: .seconds(30)
        )
      )
    )
    model.draft = "First conversation"
    let send = Task { await model.sendMessage() }
    await waitUntil {
      if case .streaming = model.executionPhase { return true }
      return false
    }
    let firstThreadID = model.activeThreadID

    await model.startNewChat()
    await send.value

    XCTAssertNil(model.activeThreadID, "new chat did not take effect")
    XCTAssertNotNil(firstThreadID)
    let interrupted = model.threads
      .first { $0.id == firstThreadID }?
      .messages.last
    XCTAssertEqual(
      interrupted?.status,
      .interrupted,
      "navigating away should read as interrupted, not as the user stopping"
    )
    XCTAssertEqual(model.executionPhase, .idle)
  }

  func testSwitchingThreadsDuringGenerationSettlesTheTurnOnItsOwnThread() async {
    let store = InMemoryConversationStore()
    let existing = ConversationThreadSnapshot(
      id: UUID(),
      title: "Earlier",
      createdAt: .distantPast,
      updatedAt: .distantPast,
      messages: [],
      draft: ""
    )
    await store.save(thread: existing)

    let model = AppModel(
      conversationStore: store,
      providerExecutor: DeterministicMockProvider(
        configuration: .init(
          outcome: .response(chunks: ["slow ", "reply"]),
          delay: .seconds(30)
        )
      )
    )
    await model.restoreConversations()
    model.draft = "Start something"
    let send = Task { await model.sendMessage() }
    await waitUntil {
      if case .streaming = model.executionPhase { return true }
      return false
    }
    let streamingThreadID = try! XCTUnwrap(model.activeThreadID)

    await model.selectThread(id: existing.id)
    await send.value

    XCTAssertEqual(model.activeThreadID, existing.id)
    let settled = model.threads
      .first { $0.id == streamingThreadID }?
      .messages.last
    XCTAssertEqual(
      settled?.status,
      .interrupted,
      "the in-flight turn did not settle on the thread it belonged to"
    )
  }

  func testStopButtonStillReadsAsStoppedNotInterrupted() async {
    let model = AppModel(
      providerExecutor: DeterministicMockProvider(
        configuration: .init(
          outcome: .response(chunks: ["a ", "b"]),
          delay: .seconds(30)
        )
      )
    )
    model.draft = "Stop me"
    let send = Task { await model.sendMessage() }
    await waitUntil {
      if case .streaming = model.executionPhase { return true }
      return false
    }

    await model.stopGenerating()
    await send.value

    XCTAssertEqual(model.activeThread?.messages.last?.status, .stopped)
  }

  // MARK: - Inline persistence recovery

  func testAFailedSaveOffersARetryThatCanSucceed() async {
    let store = FailableConversationStore()
    await store.setFailing(true)
    let model = AppModel(
      conversationStore: store,
      providerExecutor: DeterministicMockProvider(
        configuration: .init(outcome: .response(chunks: ["ok"]), delay: .zero)
      )
    )
    model.draft = "Save me"

    await model.sendMessage()

    XCTAssertNotNil(model.persistenceNotice)
    XCTAssertNotNil(
      model.persistenceRetry,
      "a storage failure left the user with no way forward"
    )

    await store.setFailing(false)
    await model.retryPersistence()

    XCTAssertNil(model.persistenceNotice)
    XCTAssertNil(model.persistenceRetry)
    let persisted = await store.storedThreads().first
    XCTAssertEqual(persisted?.messages.last?.content, "ok")
  }

  func testAFailedRetryKeepsAnActionableNotice() async {
    let store = FailableConversationStore()
    await store.setFailing(true)
    let model = AppModel(
      conversationStore: store,
      providerExecutor: DeterministicMockProvider(
        configuration: .init(outcome: .response(chunks: ["ok"]), delay: .zero)
      )
    )
    model.draft = "Save me"
    await model.sendMessage()

    await model.retryPersistence()

    XCTAssertNotNil(model.persistenceNotice)
    XCTAssertNotNil(model.persistenceRetry)
  }

  func testDismissingTheNoticeClearsItsRetry() async {
    let store = FailableConversationStore()
    await store.setFailing(true)
    let model = AppModel(conversationStore: store)
    model.draft = "Save me"
    await model.sendMessage()

    model.dismissPersistenceNotice()

    XCTAssertNil(model.persistenceNotice)
    XCTAssertNil(model.persistenceRetry)
  }

  // MARK: - Helpers

  private func waitUntil(
    timeout: Duration = .seconds(2),
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

// MARK: - Stores

/// Records how often a thread is written so per-delta persistence is testable.
private actor CountingConversationStore: ConversationStore {
  private let backing = InMemoryConversationStore()
  private(set) var threadWriteCount = 0

  func listThreads() async -> [ConversationThreadSnapshot] {
    await backing.listThreads()
  }

  func thread(id: UUID) async -> ConversationThreadSnapshot? {
    await backing.thread(id: id)
  }

  func save(thread: ConversationThreadSnapshot) async {
    threadWriteCount += 1
    await backing.save(thread: thread)
  }

  func deleteThread(id: UUID) async {
    await backing.deleteThread(id: id)
  }

  func deleteAll() async {
    await backing.deleteAll()
  }

  func pruneThreads(olderThan cutoff: Date) async -> Int {
    await backing.pruneThreads(olderThan: cutoff)
  }

  func loadWorkspace() async -> ConversationWorkspaceSnapshot {
    await backing.loadWorkspace()
  }

  func save(workspace: ConversationWorkspaceSnapshot) async {
    await backing.save(workspace: workspace)
  }

  func exportData() async throws -> Data {
    try await backing.exportData()
  }

  func storedThreads() async -> [ConversationThreadSnapshot] {
    await backing.listThreads()
  }
}

/// A store whose writes can be made to fail on demand.
private actor FailableConversationStore: ConversationStore {
  struct Failure: Error {}

  private let backing = InMemoryConversationStore()
  private var isFailing = false

  func setFailing(_ failing: Bool) {
    isFailing = failing
  }

  func listThreads() async -> [ConversationThreadSnapshot] {
    await backing.listThreads()
  }

  func thread(id: UUID) async -> ConversationThreadSnapshot? {
    await backing.thread(id: id)
  }

  func save(thread: ConversationThreadSnapshot) async throws {
    if isFailing { throw Failure() }
    await backing.save(thread: thread)
  }

  func deleteThread(id: UUID) async throws {
    if isFailing { throw Failure() }
    await backing.deleteThread(id: id)
  }

  func deleteAll() async throws {
    if isFailing { throw Failure() }
    await backing.deleteAll()
  }

  func pruneThreads(olderThan cutoff: Date) async -> Int {
    await backing.pruneThreads(olderThan: cutoff)
  }

  func loadWorkspace() async -> ConversationWorkspaceSnapshot {
    await backing.loadWorkspace()
  }

  func save(workspace: ConversationWorkspaceSnapshot) async throws {
    if isFailing { throw Failure() }
    await backing.save(workspace: workspace)
  }

  func exportData() async throws -> Data {
    try await backing.exportData()
  }

  func storedThreads() async -> [ConversationThreadSnapshot] {
    await backing.listThreads()
  }
}
