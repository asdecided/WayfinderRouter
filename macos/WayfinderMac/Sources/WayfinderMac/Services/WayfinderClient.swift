import Foundation

public protocol WayfinderClient: Sendable {
    func route(prompt: String) async throws -> RoutingDecision
    func streamChat(messages: [ChatRequestMessage]) -> AsyncThrowingStream<ChatStreamEvent, Error>
    func streamChat(
        messages: [ChatRequestMessage],
        destination: ChatDestination
    ) -> AsyncThrowingStream<ChatStreamEvent, Error>
    func loadStats(range: StatsRange) async throws -> RoutingStats
    func loadOverview() async throws -> GatewayOverview
}

public extension WayfinderClient {
    func streamChat(
        messages: [ChatRequestMessage],
        destination: ChatDestination
    ) -> AsyncThrowingStream<ChatStreamEvent, Error> {
        streamChat(messages: messages)
    }

    func streamChat(messages: [ChatRequestMessage]) -> AsyncThrowingStream<ChatStreamEvent, Error> {
        AsyncThrowingStream { continuation in
            let task = Task {
                do {
                    guard let prompt = messages.last(where: { $0.role == "user" })?.content else {
                        throw WayfinderClientError.emptyPrompt
                    }
                    let decision = try await route(prompt: prompt)
                    continuation.yield(.decision(decision))
                    continuation.yield(.text(decision.explanation))
                    continuation.yield(.completed)
                    continuation.finish()
                } catch {
                    continuation.finish(throwing: error)
                }
            }
            continuation.onTermination = { _ in task.cancel() }
        }
    }

    func loadOverview() async throws -> GatewayOverview {
        let stats = try await loadStats(range: .today)
        return GatewayOverview(
            gateway: stats.isRunning
                ? .running(detail: "Local router ready")
                : .stopped(detail: "Service stopped"),
            hosted: stats.isRunning
                ? .checkKeys(detail: "Provider keys in Settings")
                : .unavailable(detail: "Start the gateway service"),
            routingStats: stats,
            updatedAt: stats.updatedAt
        )
    }
}

public enum WayfinderClientError: LocalizedError, Equatable {
    case emptyPrompt
    case gatewayResponseMissingDecision
    case gatewayResponseTooLarge
    case gatewayStatus(Int, model: String? = nil)
    case invalidChatStream
    case chatTurnFailed
    case chatTurnInterrupted
    case chatProviderBusy
    case chatUsageLimitReached
    case chatAccountNotReady
    case conversationTooLarge

    public var errorDescription: String? {
        switch self {
        case .emptyPrompt:
            return "The prompt is empty."
        case .gatewayResponseMissingDecision:
            return "The gateway response did not include a Wayfinder decision."
        case .gatewayResponseTooLarge:
            return "The gateway response exceeded Wayfinder's safety limit."
        case .gatewayStatus(let status, let model):
            switch status {
            case 401, 403:
                return "The selected provider needs a valid key. Check Providers in Settings, then retry."
            case 402:
                return "The selected provider could not accept this request because of its account balance. Check Providers in Settings, then retry."
            case 502:
                return "The selected model endpoint could not be reached. Check Gateway in Settings, then retry."
            case 503:
                if model?.localizedCaseInsensitiveContains("apple") == true {
                    return "Apple Foundation Models aren't ready for this app. Check Apple Intelligence and app signing, or choose another model in Gateway Settings."
                }
                return "No configured model is ready to reply. Check Gateway in Settings, then retry."
            default:
                return "The gateway could not complete this request (HTTP \(status)). Check Settings, then retry."
            }
        case .invalidChatStream:
            return "The gateway returned an incomplete Chat stream. Try again."
        case .chatTurnFailed:
            return "The selected model could not complete this reply. Try again."
        case .chatTurnInterrupted:
            return "The selected model interrupted this reply before completion."
        case .chatProviderBusy:
            return "The selected ChatGPT account is already answering another request."
        case .chatUsageLimitReached:
            return "The selected account has reached its current ChatGPT usage limit."
        case .chatAccountNotReady:
            return "The selected ChatGPT account or model is not ready. Check Accounts in Settings, then retry."
        case .conversationTooLarge:
            return "This conversation is too large. Start a new Chat and try again."
        }
    }
}
