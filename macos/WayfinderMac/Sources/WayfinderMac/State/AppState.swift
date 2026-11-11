import Foundation

@MainActor
public final class AppState: ObservableObject {
    @Published public private(set) var routingStats: RoutingStats
    @Published public private(set) var gatewayOverview: GatewayOverview
    @Published public private(set) var isRefreshingStats = false
    @Published public var chatDraft = ""
    @Published public var chatDestination: ChatDestination = .automatic
    @Published public private(set) var chatMessageDestinationOverride: ChatDestination?
    @Published public private(set) var chatDestinations: [ChatDestination] = [.automatic]
    @Published public private(set) var chatMessages: [ChatMessage]
    @Published public private(set) var chatConversations: [ChatConversation]
    @Published public private(set) var activeChatConversationID: UUID
    @Published public private(set) var isSendingMessage = false
    @Published public private(set) var setupAssessment: SetupAssessment = .checking

    public let chatDestinationNameStore: ChatDestinationNameStore

    private let client: any WayfinderClient
    private let chatConversationStore: ChatConversationStore
    private let setupService = SetupService()
    private var chatTask: Task<Void, Never>?

    public init(
        client: any WayfinderClient,
        chatDestinationNameStore: ChatDestinationNameStore? = nil,
        chatConversationStore: ChatConversationStore = ChatConversationStore()
    ) {
        self.client = client
        self.chatDestinationNameStore = chatDestinationNameStore ?? ChatDestinationNameStore()
        self.chatConversationStore = chatConversationStore
        self.routingStats = .empty
        self.gatewayOverview = .checking
        let storedConversations = chatConversationStore.load()
        self.chatConversations = storedConversations
        if let mostRecent = storedConversations.first {
            self.activeChatConversationID = mostRecent.id
            self.chatMessages = mostRecent.messages
        } else {
            self.activeChatConversationID = UUID()
            self.chatMessages = []
        }
    }

    public var canSendMessage: Bool {
        guard !isSendingMessage, let submission = resolvedChatSubmission else {
            return false
        }
        return submission.destination.isAvailable
    }

    public var canClearChat: Bool {
        !chatMessages.isEmpty && !isSendingMessage
    }

    public var canRetryChat: Bool {
        guard !isSendingMessage, let failedTurn = lastRetryableChatTurn else {
            return false
        }
        return retryDestination(for: failedTurn.prompt).isAvailable
    }

    public func refreshStats() {
        guard !isRefreshingStats else {
            return
        }

        isRefreshingStats = true
        Task {
            do {
                let overview = try await client.loadOverview()
                await MainActor.run {
                    self.gatewayOverview = overview.preservingUnavailableEndpoints(
                        from: self.gatewayOverview.endpoints
                    )
                    self.updateChatDestinations(from: self.gatewayOverview)
                    self.routingStats = overview.routingStats
                    self.isRefreshingStats = false
                }
            } catch {
                await MainActor.run {
                    self.gatewayOverview = GatewayOverview
                        .unreachable(error.localizedDescription)
                        .preservingUnavailableEndpoints(from: self.gatewayOverview.endpoints)
                    self.routingStats = .empty
                    self.isRefreshingStats = false
                }
            }
        }
    }

    public func refreshSetupAssessment() {
        Task {
            let assessment = await setupService.assess()
            await MainActor.run { self.setupAssessment = assessment }
        }
    }

    public func sendChatDraft() {
        guard !isSendingMessage,
              let submission = resolvedChatSubmission,
              submission.destination.isAvailable else {
            return
        }
        submitChatMessage(
            prompt: submission.prompt,
            destination: submission.destination
        )
    }

    public func selectChatMessageDestinationOverride(_ destination: ChatDestination) {
        guard !isSendingMessage else { return }
        chatMessageDestinationOverride = destination
        chatDraft = ChatDestinationMentionResolver.removingActiveMention(from: chatDraft)
    }

    public func clearChatMessageDestinationOverride() {
        guard !isSendingMessage else { return }
        chatMessageDestinationOverride = nil
    }

    public func stopChatResponse() {
        chatTask?.cancel()
    }

    public func startNewChat() {
        guard canClearChat else {
            return
        }
        saveActiveConversation()
        activeChatConversationID = UUID()
        chatMessages = []
        chatDraft = ""
        chatMessageDestinationOverride = nil
    }

    public func selectChatConversation(_ id: UUID) {
        guard !isSendingMessage, id != activeChatConversationID,
              let conversation = chatConversations.first(where: { $0.id == id }) else {
            return
        }
        saveActiveConversation()
        activeChatConversationID = conversation.id
        chatMessages = conversation.messages
        chatDraft = ""
        chatMessageDestinationOverride = nil
    }

    public func retryLastChatTurn() {
        guard !isSendingMessage, let failedTurn = lastRetryableChatTurn else {
            return
        }
        let destination = retryDestination(for: failedTurn.prompt)
        guard destination.isAvailable else { return }
        chatMessages.removeSubrange(failedTurn.promptIndex...failedTurn.responseIndex)
        submitChatMessage(prompt: failedTurn.prompt.text, destination: destination)
    }

    static func chatRequestMessages(from messages: [ChatMessage]) -> [ChatRequestMessage] {
        messages.compactMap { message in
            guard message.state == .complete, !message.text.isEmpty else {
                return nil
            }
            return ChatRequestMessage(
                role: message.role == .user ? "user" : "assistant",
                content: message.text
            )
        }
    }

    nonisolated static func chatDestinations(from overview: GatewayOverview) -> [ChatDestination] {
        let configured = overview.endpoints
            .filter { $0.isChatDestinationAvailable && $0.name != "auto" }
            .map(ChatDestination.init(endpoint:))
        return [.automatic] + configured
    }

    public func setChatDestinationDisplayName(_ name: String, for routeName: String) {
        chatDestinationNameStore.setName(name, for: routeName)
        updateChatDestinations(from: gatewayOverview)
    }

    public func resetChatDestinationDisplayName(for routeName: String) {
        chatDestinationNameStore.resetName(for: routeName)
        updateChatDestinations(from: gatewayOverview)
    }

    nonisolated static func chatErrorMessage(
        _ error: Error,
        destination: ChatDestination
    ) -> String {
        if destination.isChatGPTAccount,
           let clientError = error as? WayfinderClientError {
            switch clientError {
            case .chatProviderBusy:
                return "ChatGPT is already answering another request. Wait for it to finish, then retry."
            case .chatTurnInterrupted, .gatewayStatus(409, _):
                return "ChatGPT interrupted this reply before completion. Retry when you're ready."
            case .chatTurnFailed, .gatewayStatus(502, _):
                return "ChatGPT could not complete this reply. Retry, or choose another destination."
            case .chatUsageLimitReached, .gatewayStatus(429, _):
                return "This ChatGPT account has reached its current usage limit. Try again later, or choose another destination."
            case .chatAccountNotReady, .gatewayStatus(503, _):
                return "ChatGPT is not connected or its Codex model is unavailable. Check Accounts in Settings, then retry."
            default:
                break
            }
        }
        return error.localizedDescription
    }

    nonisolated static func chatRecoverySettingsSection(
        _ error: Error,
        destination: ChatDestination
    ) -> SettingsSection {
        guard let clientError = error as? WayfinderClientError else {
            return .gateway
        }
        switch clientError {
        case .chatAccountNotReady:
            return .connections
        case .gatewayStatus(503, _) where destination.isChatGPTAccount:
            return .connections
        default:
            return .gateway
        }
    }

    private func updateChatDestinations(from overview: GatewayOverview) {
        var destinations = Self.chatDestinations(from: overview).map { destination in
            guard let routeName = destination.routeName else { return destination }
            return destination.withTitle(
                chatDestinationNameStore.name(for: routeName, default: destination.title)
            )
        }
        if let refreshed = destinations.first(where: { $0.id == chatDestination.id }) {
            chatDestination = refreshed
        } else if !chatDestination.isAutomatic {
            let unavailable = chatDestination.withAvailability(false)
            destinations.append(unavailable)
            chatDestination = unavailable
        }
        chatDestinations = destinations
    }

    private var resolvedChatSubmission: ChatSubmission? {
        let trimmedDraft = chatDraft.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmedDraft.isEmpty else { return nil }

        if let chatMessageDestinationOverride {
            return ChatSubmission(
                prompt: trimmedDraft,
                destination: chatMessageDestinationOverride
            )
        }

        switch ChatDestinationMentionResolver.resolve(
            draft: trimmedDraft,
            destinations: chatDestinations
        ) {
        case let .resolved(destination, prompt):
            guard !prompt.isEmpty else { return nil }
            return ChatSubmission(prompt: prompt, destination: destination)
        case .none, .ambiguous, .unknown:
            return ChatSubmission(prompt: trimmedDraft, destination: chatDestination)
        }
    }

    private var lastRetryableChatTurn: (
        promptIndex: Int,
        responseIndex: Int,
        prompt: ChatMessage
    )? {
        guard let responseIndex = chatMessages.lastIndex(where: {
            $0.role == .assistant && ($0.state == .failed || $0.state == .stopped)
        }),
        responseIndex > 0,
        chatMessages[responseIndex - 1].role == .user else {
            return nil
        }
        return (responseIndex - 1, responseIndex, chatMessages[responseIndex - 1])
    }

    private func retryDestination(for prompt: ChatMessage) -> ChatDestination {
        guard let routeName = prompt.requestedGatewayRouteName else {
            return .automatic
        }
        if chatDestination.gatewayModelValue.caseInsensitiveCompare(routeName) == .orderedSame {
            return chatDestination
        }
        return ChatDestinationMentionResolver.destination(
            forGatewayModelValue: routeName,
            destinations: chatDestinations
        ) ?? ChatDestination(
            routeName: routeName,
            title: prompt.requestedDestinationTitle ?? routeName,
            detail: "Previously requested route",
            isAvailable: false
        )
    }

    private func submitChatMessage(prompt: String, destination: ChatDestination) {
        let requestMessages = Self.chatRequestMessages(from: chatMessages) + [
            ChatRequestMessage(role: "user", content: prompt)
        ]
        let responseID = UUID()
        chatDraft = ""
        chatMessageDestinationOverride = nil
        isSendingMessage = true
        chatMessages.append(ChatMessage(
            role: .user,
            text: prompt,
            requestedGatewayRouteName: destination.routeName,
            requestedDestinationTitle: destination.isAutomatic ? nil : destination.title
        ))
        chatMessages.append(ChatMessage(
            id: responseID,
            role: .assistant,
            text: "",
            state: .streaming
        ))
        saveActiveConversation()

        chatTask = Task {
            do {
                for try await event in client.streamChat(
                    messages: requestMessages,
                    destination: destination
                ) {
                    guard let index = self.chatMessages.firstIndex(where: { $0.id == responseID }) else {
                        continue
                    }
                    switch event {
                    case let .decision(decision):
                        self.chatMessages[index].decision = decision
                    case let .text(fragment):
                        self.chatMessages[index].text += fragment
                    case .completed:
                        self.chatMessages[index].state = self.chatMessages[index].text.isEmpty
                            ? .failed
                            : .complete
                        if self.chatMessages[index].text.isEmpty {
                            self.chatMessages[index].text = "No model reply was delivered. Check the configured endpoint in Settings."
                            self.chatMessages[index].recoverySettingsSection = .gateway
                        }
                    }
                }
                if Task.isCancelled {
                    self.finishStoppedChatMessage(id: responseID)
                    return
                }
                if let index = self.chatMessages.firstIndex(where: { $0.id == responseID }),
                   self.chatMessages[index].state == .streaming {
                    self.finishFailedChatMessage(
                        id: responseID,
                        message: WayfinderClientError.invalidChatStream.localizedDescription
                    )
                    return
                }
                self.isSendingMessage = false
                self.chatTask = nil
                self.saveActiveConversation()
                self.refreshStats()
            } catch {
                if Task.isCancelled || error is CancellationError {
                    self.finishStoppedChatMessage(id: responseID)
                } else {
                    self.finishFailedChatMessage(
                        id: responseID,
                        message: Self.chatErrorMessage(error, destination: destination),
                        recoverySettingsSection: Self.chatRecoverySettingsSection(
                            error,
                            destination: destination
                        )
                    )
                }
            }
        }
    }

    private func finishStoppedChatMessage(id: UUID) {
        if let index = chatMessages.firstIndex(where: { $0.id == id }) {
            chatMessages[index].state = .stopped
            if chatMessages[index].text.isEmpty {
                chatMessages[index].text = "Response stopped."
            }
        }
        isSendingMessage = false
        chatTask = nil
        saveActiveConversation()
    }

    private func finishFailedChatMessage(
        id: UUID,
        message: String,
        recoverySettingsSection: SettingsSection = .gateway
    ) {
        if let index = chatMessages.firstIndex(where: { $0.id == id }) {
            chatMessages[index].state = .failed
            chatMessages[index].text = message
            chatMessages[index].recoverySettingsSection = recoverySettingsSection
        }
        isSendingMessage = false
        chatTask = nil
        saveActiveConversation()
        refreshStats()
    }

    private func saveActiveConversation() {
        guard !chatMessages.isEmpty else { return }
        let now = Date()
        if let index = chatConversations.firstIndex(where: { $0.id == activeChatConversationID }) {
            chatConversations[index].messages = chatMessages
            chatConversations[index].updatedAt = now
        } else {
            chatConversations.append(ChatConversation(
                id: activeChatConversationID,
                messages: chatMessages,
                createdAt: chatMessages.first?.createdAt ?? now,
                updatedAt: now
            ))
        }
        chatConversations.sort { $0.updatedAt > $1.updatedAt }
        chatConversationStore.save(chatConversations)
    }
}

private struct ChatSubmission {
    let prompt: String
    let destination: ChatDestination
}

public enum SettingsSection: String, CaseIterable, Codable, Identifiable, Sendable {
    case connections = "Connections"
    case gateway = "Gateway"
    case routing = "Routing"
    case about = "About"

    public var id: String { rawValue }

    public var symbolName: String {
        switch self {
        case .gateway:
            return "server.rack"
        case .routing:
            return "point.topleft.down.curvedto.point.bottomright.up"
        case .connections:
            return "link"
        case .about:
            return "info.circle"
        }
    }

    public init(from decoder: Decoder) throws {
        let value = try decoder.singleValueContainer().decode(String.self)
        switch value {
        case "Gateway": self = .gateway
        case "Routing": self = .routing
        case "Connections", "Accounts", "Keys": self = .connections
        case "About", "Help", "Privacy": self = .about
        default:
            throw DecodingError.dataCorruptedError(
                in: try decoder.singleValueContainer(),
                debugDescription: "Unknown settings section"
            )
        }
    }

    public func encode(to encoder: Encoder) throws {
        var container = encoder.singleValueContainer()
        try container.encode(rawValue)
    }
}

public enum ProviderKind: String, CaseIterable, Identifiable, Sendable {
    case anthropic = "Anthropic"
    case openAI = "OpenAI API"
    case googleGemini = "Google Gemini"
    case ollama = "Ollama"
    case lmStudio = "LM Studio"
    case custom = "Custom"

    public var id: String { rawValue }
}

private extension RoutingStats {
    static var empty: RoutingStats {
        RoutingStats(
            localPercent: 0,
            cloudPercent: 0,
            totalTurns: 0,
            savedToday: 0,
            savedLast30Days: 0,
            cloudSpendToday: 0,
            percentVsAlwaysCloud: 0,
            isPriced: false,
            hasSavings: false,
            savedTodayDisplay: "Today: Not yet available",
            savedLast30DaysDisplay: "Last 30 days: Not yet available",
            updatedAt: Date(),
            isRunning: false
        )
    }
}

private extension GatewayOverview {
    static var checking: GatewayOverview {
        GatewayOverview(
            gateway: .checking(detail: "Checking gateway status"),
            hosted: .checking(detail: "Checking configured models"),
            routingStats: .empty,
            updatedAt: Date()
        )
    }

    static func unreachable(_ detail: String) -> GatewayOverview {
        GatewayOverview(
            gateway: .unreachable(detail: detail),
            hosted: .unavailable(detail: "Gateway is not reachable"),
            routingStats: .empty,
            updatedAt: Date()
        )
    }

    func preservingUnavailableEndpoints(
        from previousEndpoints: [EndpointDisplayStatus]
    ) -> GatewayOverview {
        guard endpoints.isEmpty, !gateway.isRunning, !previousEndpoints.isEmpty else {
            return self
        }
        return GatewayOverview(
            gateway: gateway,
            hosted: hosted,
            endpoints: previousEndpoints.map {
                EndpointDisplayStatus(
                    name: $0.name,
                    providerName: $0.providerName,
                    modelName: $0.modelName,
                    state: .unavailable
                )
            },
            routingStats: routingStats,
            updatedAt: updatedAt
        )
    }
}
