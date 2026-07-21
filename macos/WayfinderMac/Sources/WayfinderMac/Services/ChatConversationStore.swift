import Foundation

public final class ChatConversationStore: @unchecked Sendable {
    private static let maximumStoredConversations = 100
    private static let maximumFileBytes = 8 * 1_024 * 1_024

    private let fileURL: URL?
    private let fileManager: FileManager

    public init(fileURL: URL? = nil, fileManager: FileManager = .default) {
        self.fileURL = fileURL
        self.fileManager = fileManager
    }

    public static func applicationSupport(fileManager: FileManager = .default) -> ChatConversationStore {
        let root = fileManager.urls(for: .applicationSupportDirectory, in: .userDomainMask).first
        return ChatConversationStore(
            fileURL: root?
                .appendingPathComponent("Wayfinder", isDirectory: true)
                .appendingPathComponent("chat-history.json", isDirectory: false),
            fileManager: fileManager
        )
    }

    public func load() -> [ChatConversation] {
        guard let fileURL,
              let attributes = try? fileManager.attributesOfItem(atPath: fileURL.path),
              let byteCount = attributes[.size] as? NSNumber,
              byteCount.intValue <= Self.maximumFileBytes,
              let data = try? Data(contentsOf: fileURL, options: .mappedIfSafe),
              let conversations = try? JSONDecoder().decode([ChatConversation].self, from: data) else {
            return []
        }
        return Array(
            conversations
                .filter { !$0.messages.isEmpty }
                .sorted { $0.updatedAt > $1.updatedAt }
                .prefix(Self.maximumStoredConversations)
        )
    }

    public func save(_ conversations: [ChatConversation]) {
        guard let fileURL else { return }
        let bounded = Array(
            conversations
                .filter { !$0.messages.isEmpty }
                .sorted { $0.updatedAt > $1.updatedAt }
                .prefix(Self.maximumStoredConversations)
        )
        guard let data = try? JSONEncoder().encode(bounded),
              data.count <= Self.maximumFileBytes else {
            return
        }
        do {
            try fileManager.createDirectory(
                at: fileURL.deletingLastPathComponent(),
                withIntermediateDirectories: true
            )
            try data.write(to: fileURL, options: [.atomic, .completeFileProtection])
        } catch {
            // Chat remains usable in memory if local history cannot be written.
        }
    }
}
