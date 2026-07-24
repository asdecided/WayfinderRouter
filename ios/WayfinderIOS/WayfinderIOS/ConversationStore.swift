import Foundation
import SwiftData

enum ConversationMessageRole: String, Codable, Sendable {
  case user
  case assistant
  case system
}

enum ConversationMessageStatus: String, Codable, Sendable {
  case pending
  case streaming
  case completed
  case stopped
  case interrupted
  case failed
}

struct StoredRouteReceipt: Codable, Equatable, Sendable {
  let destinationID: String
  let destinationName: String
  let score: Double
  let recommendation: String
  let executionSummary: String
}

struct ConversationMessageSnapshot: Codable, Equatable, Identifiable, Sendable {
  let id: UUID
  let role: ConversationMessageRole
  let content: String
  let createdAt: Date
  let status: ConversationMessageStatus
  let routeReceipt: StoredRouteReceipt?
}

struct ConversationThreadSnapshot: Codable, Equatable, Identifiable, Sendable {
  let id: UUID
  var title: String
  let createdAt: Date
  var updatedAt: Date
  var messages: [ConversationMessageSnapshot]
  var draft: String

  static func title(for prompt: String) -> String {
    let collapsed =
      prompt
      .split(whereSeparator: \.isWhitespace)
      .joined(separator: " ")

    guard collapsed.count > 52 else {
      return collapsed
    }

    return "\(collapsed.prefix(49))…"
  }
}

struct ConversationWorkspaceSnapshot: Codable, Equatable, Sendable {
  var activeThreadID: UUID?
  var draft: String
  var retentionDays: Int?
  var updatedAt: Date

  static let empty = ConversationWorkspaceSnapshot(
    activeThreadID: nil,
    draft: "",
    retentionDays: nil,
    updatedAt: .distantPast
  )
}

struct ConversationExportEnvelope: Codable, Equatable, Sendable {
  let schemaVersion: Int
  let threads: [ConversationThreadSnapshot]
}

protocol ConversationStore: Sendable {
  func listThreads() async throws -> [ConversationThreadSnapshot]
  func thread(id: UUID) async throws -> ConversationThreadSnapshot?
  func save(thread: ConversationThreadSnapshot) async throws
  func deleteThread(id: UUID) async throws
  func deleteAll() async throws
  func pruneThreads(olderThan cutoff: Date) async throws -> Int
  func loadWorkspace() async throws -> ConversationWorkspaceSnapshot
  func save(workspace: ConversationWorkspaceSnapshot) async throws
  func exportData() async throws -> Data
}

enum ConversationStoreError: LocalizedError, Equatable {
  case invalidPayload

  var errorDescription: String? {
    switch self {
    case .invalidPayload:
      "Stored conversation data could not be read."
    }
  }
}

enum WayfinderConversationSchemaV1: VersionedSchema {
  static var versionIdentifier: Schema.Version {
    Schema.Version(1, 0, 0)
  }

  static var models: [any PersistentModel.Type] {
    [ConversationRecord.self, WorkspaceRecord.self]
  }

  @Model
  final class ConversationRecord {
    @Attribute(.unique) var id: UUID
    var title: String
    var createdAt: Date
    var updatedAt: Date
    var payload: Data

    init(
      id: UUID,
      title: String,
      createdAt: Date,
      updatedAt: Date,
      payload: Data
    ) {
      self.id = id
      self.title = title
      self.createdAt = createdAt
      self.updatedAt = updatedAt
      self.payload = payload
    }
  }

  @Model
  final class WorkspaceRecord {
    @Attribute(.unique) var key: String
    var activeThreadID: UUID?
    var draft: String
    var retentionDays: Int?
    var updatedAt: Date

    init(
      key: String,
      activeThreadID: UUID?,
      draft: String,
      retentionDays: Int?,
      updatedAt: Date
    ) {
      self.key = key
      self.activeThreadID = activeThreadID
      self.draft = draft
      self.retentionDays = retentionDays
      self.updatedAt = updatedAt
    }
  }
}

enum WayfinderConversationMigrationPlan: SchemaMigrationPlan {
  static var schemas: [any VersionedSchema.Type] {
    [WayfinderConversationSchemaV1.self]
  }

  static var stages: [MigrationStage] {
    []
  }
}

@ModelActor
actor SwiftDataConversationStore: ConversationStore {
  private static let workspaceKey = "primary"

  static func makeContainer(inMemory: Bool = false) throws -> ModelContainer {
    let schema = Schema(versionedSchema: WayfinderConversationSchemaV1.self)
    let configuration = ModelConfiguration(
      schema: schema,
      isStoredInMemoryOnly: inMemory
    )

    return try ModelContainer(
      for: schema,
      migrationPlan: WayfinderConversationMigrationPlan.self,
      configurations: [configuration]
    )
  }

  func listThreads() throws -> [ConversationThreadSnapshot] {
    var descriptor = FetchDescriptor<
      WayfinderConversationSchemaV1.ConversationRecord
    >(
      sortBy: [SortDescriptor(\.updatedAt, order: .reverse)]
    )
    descriptor.fetchLimit = 500

    return try modelContext.fetch(descriptor).map(decode)
  }

  func thread(id: UUID) throws -> ConversationThreadSnapshot? {
    let descriptor = FetchDescriptor<
      WayfinderConversationSchemaV1.ConversationRecord
    >(
      predicate: #Predicate { record in
        record.id == id
      }
    )

    return try modelContext.fetch(descriptor).first.map(decode)
  }

  func save(thread: ConversationThreadSnapshot) throws {
    let payload = try encoder.encode(thread)

    if let record = try record(id: thread.id) {
      record.title = thread.title
      record.createdAt = thread.createdAt
      record.updatedAt = thread.updatedAt
      record.payload = payload
    } else {
      modelContext.insert(
        WayfinderConversationSchemaV1.ConversationRecord(
          id: thread.id,
          title: thread.title,
          createdAt: thread.createdAt,
          updatedAt: thread.updatedAt,
          payload: payload
        )
      )
    }

    try modelContext.save()
  }

  func deleteThread(id: UUID) throws {
    if let record = try record(id: id) {
      modelContext.delete(record)
    }

    if let workspace = try workspaceRecord(),
      workspace.activeThreadID == id
    {
      workspace.activeThreadID = nil
      workspace.draft = ""
      workspace.updatedAt = Date()
    }

    try modelContext.save()
  }

  func deleteAll() throws {
    try modelContext.delete(
      model: WayfinderConversationSchemaV1.ConversationRecord.self
    )
    try modelContext.delete(
      model: WayfinderConversationSchemaV1.WorkspaceRecord.self
    )
    try modelContext.save()
  }

  func pruneThreads(olderThan cutoff: Date) throws -> Int {
    let descriptor = FetchDescriptor<
      WayfinderConversationSchemaV1.ConversationRecord
    >(
      predicate: #Predicate { record in
        record.updatedAt < cutoff
      }
    )
    let records = try modelContext.fetch(descriptor)
    let removedIDs = Set(records.map(\.id))

    for record in records {
      modelContext.delete(record)
    }

    if let workspace = try workspaceRecord(),
      let activeThreadID = workspace.activeThreadID,
      removedIDs.contains(activeThreadID)
    {
      workspace.activeThreadID = nil
      workspace.draft = ""
      workspace.updatedAt = Date()
    }

    try modelContext.save()
    return records.count
  }

  func loadWorkspace() throws -> ConversationWorkspaceSnapshot {
    guard let workspace = try workspaceRecord() else {
      return .empty
    }

    return ConversationWorkspaceSnapshot(
      activeThreadID: workspace.activeThreadID,
      draft: workspace.draft,
      retentionDays: workspace.retentionDays,
      updatedAt: workspace.updatedAt
    )
  }

  func save(workspace: ConversationWorkspaceSnapshot) throws {
    if let record = try workspaceRecord() {
      record.activeThreadID = workspace.activeThreadID
      record.draft = workspace.draft
      record.retentionDays = workspace.retentionDays
      record.updatedAt = workspace.updatedAt
    } else {
      modelContext.insert(
        WayfinderConversationSchemaV1.WorkspaceRecord(
          key: Self.workspaceKey,
          activeThreadID: workspace.activeThreadID,
          draft: workspace.draft,
          retentionDays: workspace.retentionDays,
          updatedAt: workspace.updatedAt
        )
      )
    }

    try modelContext.save()
  }

  func exportData() throws -> Data {
    let threads = try listThreads().sorted { lhs, rhs in
      if lhs.createdAt == rhs.createdAt {
        return lhs.id.uuidString < rhs.id.uuidString
      }
      return lhs.createdAt < rhs.createdAt
    }

    return try encoder.encode(
      ConversationExportEnvelope(schemaVersion: 1, threads: threads)
    )
  }

  private var encoder: JSONEncoder {
    let encoder = JSONEncoder()
    encoder.dateEncodingStrategy = .iso8601
    encoder.outputFormatting = [.sortedKeys]
    return encoder
  }

  private var decoder: JSONDecoder {
    let decoder = JSONDecoder()
    decoder.dateDecodingStrategy = .iso8601
    return decoder
  }

  private func record(
    id: UUID
  ) throws -> WayfinderConversationSchemaV1.ConversationRecord? {
    let descriptor = FetchDescriptor<
      WayfinderConversationSchemaV1.ConversationRecord
    >(
      predicate: #Predicate { record in
        record.id == id
      }
    )
    return try modelContext.fetch(descriptor).first
  }

  private func workspaceRecord()
    throws -> WayfinderConversationSchemaV1.WorkspaceRecord?
  {
    let key = Self.workspaceKey
    let descriptor = FetchDescriptor<
      WayfinderConversationSchemaV1.WorkspaceRecord
    >(
      predicate: #Predicate { record in
        record.key == key
      }
    )
    return try modelContext.fetch(descriptor).first
  }

  private func decode(
    _ record: WayfinderConversationSchemaV1.ConversationRecord
  ) throws -> ConversationThreadSnapshot {
    do {
      return try decoder.decode(
        ConversationThreadSnapshot.self,
        from: record.payload
      )
    } catch {
      throw ConversationStoreError.invalidPayload
    }
  }
}

actor InMemoryConversationStore: ConversationStore {
  private var threads: [UUID: ConversationThreadSnapshot] = [:]
  private var workspace: ConversationWorkspaceSnapshot = .empty

  func listThreads() -> [ConversationThreadSnapshot] {
    threads.values.sorted {
      if $0.updatedAt == $1.updatedAt {
        return $0.id.uuidString < $1.id.uuidString
      }
      return $0.updatedAt > $1.updatedAt
    }
  }

  func thread(id: UUID) -> ConversationThreadSnapshot? {
    threads[id]
  }

  func save(thread: ConversationThreadSnapshot) {
    threads[thread.id] = thread
  }

  func deleteThread(id: UUID) {
    threads[id] = nil
    if workspace.activeThreadID == id {
      workspace = .empty
    }
  }

  func deleteAll() {
    threads = [:]
    workspace = .empty
  }

  func pruneThreads(olderThan cutoff: Date) -> Int {
    let removed = threads.values.filter { $0.updatedAt < cutoff }
    for thread in removed {
      threads[thread.id] = nil
    }
    if let activeThreadID = workspace.activeThreadID,
      threads[activeThreadID] == nil
    {
      workspace = .empty
    }
    return removed.count
  }

  func loadWorkspace() -> ConversationWorkspaceSnapshot {
    workspace
  }

  func save(workspace: ConversationWorkspaceSnapshot) {
    self.workspace = workspace
  }

  func exportData() throws -> Data {
    let envelope = ConversationExportEnvelope(
      schemaVersion: 1,
      threads: listThreads().sorted {
        if $0.createdAt == $1.createdAt {
          return $0.id.uuidString < $1.id.uuidString
        }
        return $0.createdAt < $1.createdAt
      }
    )
    let encoder = JSONEncoder()
    encoder.dateEncodingStrategy = .iso8601
    encoder.outputFormatting = [.sortedKeys]
    return try encoder.encode(envelope)
  }
}
