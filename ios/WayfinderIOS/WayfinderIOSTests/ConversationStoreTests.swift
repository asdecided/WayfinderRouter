import SwiftData
import XCTest

@testable import WayfinderIOS

final class ConversationStoreTests: XCTestCase {
  func testSwiftDataStoreRoundTripsThreadAndWorkspace() async throws {
    let store = try makeStore()
    let thread = makeThread(
      id: UUID(uuidString: "AAAAAAAA-AAAA-AAAA-AAAA-AAAAAAAAAAAA")!,
      prompt: "Persist this"
    )
    let workspace = ConversationWorkspaceSnapshot(
      activeThreadID: thread.id,
      draft: "",
      retentionDays: 90,
      updatedAt: thread.updatedAt
    )

    try await store.save(thread: thread)
    try await store.save(workspace: workspace)

    let restoredThread = try await store.thread(id: thread.id)
    let restoredWorkspace = try await store.loadWorkspace()

    XCTAssertEqual(restoredThread, thread)
    XCTAssertEqual(restoredWorkspace, workspace)
  }

  func testThreadsSortMostRecentlyUpdatedFirst() async throws {
    let store = try makeStore()
    let older = makeThread(
      id: UUID(uuidString: "AAAAAAAA-AAAA-AAAA-AAAA-AAAAAAAAAAAA")!,
      prompt: "Older",
      timestamp: Date(timeIntervalSince1970: 100)
    )
    let newer = makeThread(
      id: UUID(uuidString: "BBBBBBBB-BBBB-BBBB-BBBB-BBBBBBBBBBBB")!,
      prompt: "Newer",
      timestamp: Date(timeIntervalSince1970: 200)
    )

    try await store.save(thread: older)
    try await store.save(thread: newer)

    let threads = try await store.listThreads()

    XCTAssertEqual(threads.map(\.id), [newer.id, older.id])
  }

  func testStreamingAssistantStateRoundTrips() async throws {
    let store = try makeStore()
    let timestamp = Date(timeIntervalSince1970: 100)
    var thread = makeThread(prompt: "Stream this", timestamp: timestamp)
    thread.messages.append(
      ConversationMessageSnapshot(
        id: UUID(),
        role: .assistant,
        content: "Partial",
        createdAt: timestamp,
        status: .streaming,
        routeReceipt: nil
      )
    )

    try await store.save(thread: thread)

    let restored = try await store.thread(id: thread.id)
    XCTAssertEqual(restored?.messages.last?.status, .streaming)
    XCTAssertEqual(restored?.messages.last?.content, "Partial")
  }

  func testDeleteActiveThreadClearsWorkspace() async throws {
    let store = try makeStore()
    let thread = makeThread(prompt: "Delete me")
    try await store.save(thread: thread)
    try await store.save(
      workspace: ConversationWorkspaceSnapshot(
        activeThreadID: thread.id,
        draft: "unsent",
        retentionDays: nil,
        updatedAt: thread.updatedAt
      )
    )

    try await store.deleteThread(id: thread.id)

    let deletedThread = try await store.thread(id: thread.id)
    let workspace = try await store.loadWorkspace()
    XCTAssertNil(deletedThread)
    XCTAssertNil(workspace.activeThreadID)
    XCTAssertEqual(workspace.draft, "")
    XCTAssertNil(workspace.retentionDays)
    XCTAssertGreaterThan(workspace.updatedAt, thread.updatedAt)
  }

  func testRetentionPrunesOnlyThreadsBeforeCutoff() async throws {
    let store = try makeStore()
    let old = makeThread(
      prompt: "Old",
      timestamp: Date(timeIntervalSince1970: 100)
    )
    let current = makeThread(
      prompt: "Current",
      timestamp: Date(timeIntervalSince1970: 300)
    )
    try await store.save(thread: old)
    try await store.save(thread: current)

    let removed = try await store.pruneThreads(
      olderThan: Date(timeIntervalSince1970: 200)
    )

    XCTAssertEqual(removed, 1)
    let remainingIDs = try await store.listThreads().map(\.id)
    XCTAssertEqual(remainingIDs, [current.id])
  }

  func testExportIsVersionedSortedAndContainsNoCredentialFields() async throws {
    let store = try makeStore()
    let later = makeThread(
      id: UUID(uuidString: "BBBBBBBB-BBBB-BBBB-BBBB-BBBBBBBBBBBB")!,
      prompt: "Later",
      timestamp: Date(timeIntervalSince1970: 200)
    )
    let earlier = makeThread(
      id: UUID(uuidString: "AAAAAAAA-AAAA-AAAA-AAAA-AAAAAAAAAAAA")!,
      prompt: "Earlier",
      timestamp: Date(timeIntervalSince1970: 100)
    )
    try await store.save(thread: later)
    try await store.save(thread: earlier)

    let data = try await store.exportData()
    let envelope = try JSONDecoder.wayfinder.decode(
      ConversationExportEnvelope.self,
      from: data
    )
    let text = String(decoding: data, as: UTF8.self)

    XCTAssertEqual(envelope.schemaVersion, 1)
    XCTAssertEqual(envelope.threads.map(\.id), [earlier.id, later.id])
    XCTAssertFalse(text.localizedCaseInsensitiveContains("accessToken"))
    XCTAssertFalse(text.localizedCaseInsensitiveContains("apiKey"))
    let secondExport = try await store.exportData()
    XCTAssertEqual(data, secondExport)
  }

  func testDeleteAllRemovesThreadsAndWorkspaceDraft() async throws {
    let store = try makeStore()
    let thread = makeThread(prompt: "Clear everything")
    try await store.save(thread: thread)
    try await store.save(
      workspace: ConversationWorkspaceSnapshot(
        activeThreadID: nil,
        draft: "new chat draft",
        retentionDays: nil,
        updatedAt: thread.updatedAt
      )
    )

    try await store.deleteAll()

    let threads = try await store.listThreads()
    let workspace = try await store.loadWorkspace()
    XCTAssertTrue(threads.isEmpty)
    XCTAssertEqual(workspace, .empty)
  }

  private func makeStore() throws -> SwiftDataConversationStore {
    let container = try SwiftDataConversationStore.makeContainer(inMemory: true)
    return SwiftDataConversationStore(modelContainer: container)
  }

  private func makeThread(
    id: UUID = UUID(),
    prompt: String,
    timestamp: Date = Date(timeIntervalSince1970: 100)
  ) -> ConversationThreadSnapshot {
    ConversationThreadSnapshot(
      id: id,
      title: ConversationThreadSnapshot.title(for: prompt),
      createdAt: timestamp,
      updatedAt: timestamp,
      messages: [
        ConversationMessageSnapshot(
          id: UUID(),
          role: .user,
          content: prompt,
          createdAt: timestamp,
          status: .completed,
          routeReceipt: StoredRouteReceipt(
            destinationID: "device-preview",
            destinationName: "On-device preview",
            score: 0,
            recommendation: "local",
            executionSummary: "On this device"
          )
        )
      ],
      draft: ""
    )
  }
}

extension JSONDecoder {
  fileprivate static var wayfinder: JSONDecoder {
    let decoder = JSONDecoder()
    decoder.dateDecodingStrategy = .iso8601
    return decoder
  }
}
