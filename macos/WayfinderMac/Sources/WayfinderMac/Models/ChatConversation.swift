import Foundation

public struct ChatConversation: Identifiable, Codable, Equatable, Sendable {
    public let id: UUID
    public var messages: [ChatMessage]
    public let createdAt: Date
    public var updatedAt: Date

    public init(
        id: UUID = UUID(),
        messages: [ChatMessage] = [],
        createdAt: Date = Date(),
        updatedAt: Date = Date()
    ) {
        self.id = id
        self.messages = messages
        self.createdAt = createdAt
        self.updatedAt = updatedAt
    }

    public var title: String {
        messages.first(where: { $0.role == .user })?.text
            .trimmingCharacters(in: .whitespacesAndNewlines)
            .nonEmpty ?? "New chat"
    }

    public var turnCount: Int {
        messages.filter { $0.role == .user }.count
    }
}

private extension String {
    var nonEmpty: String? { isEmpty ? nil : self }
}
