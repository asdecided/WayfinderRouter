import Foundation
import XCTest
@testable import WayfinderMacCore

final class ChatDestinationMentionTests: XCTestCase {
    func testLocalMentionResolvesToPreferLocalAndStripsToken() {
        XCTAssertEqual(
            ChatDestinationMentionResolver.resolve(
                draft: "@local Summarise this",
                destinations: []
            ),
            .resolved(destination: .preferLocal, prompt: "Summarise this")
        )
    }

    func testHostedMentionResolvesToPreferHosted() {
        XCTAssertEqual(
            ChatDestinationMentionResolver.resolve(
                draft: "@HOSTED Use the strongest model",
                destinations: []
            ),
            .resolved(destination: .preferHosted, prompt: "Use the strongest model")
        )
    }

    func testCanonicalRouteMentionRetainsCanonicalDestination() {
        let destination = Self.appleDestination

        XCTAssertEqual(
            ChatDestinationMentionResolver.resolve(
                draft: "@APPLE-LOCAL Explain this",
                destinations: [destination]
            ),
            .resolved(destination: destination, prompt: "Explain this")
        )
    }

    func testUniqueFriendlyAliasResolvesToCanonicalRoute() {
        let destination = Self.appleDestination.withTitle("Mac Local")

        XCTAssertEqual(
            ChatDestinationMentionResolver.resolve(
                draft: "@mac-local Explain this",
                destinations: [destination]
            ),
            .resolved(destination: destination, prompt: "Explain this")
        )
    }

    func testUniqueCodexMentionResolvesToAvailableChatGPTDestination() {
        let destination = Self.codexDestination(routeName: "chatgpt-sol")

        XCTAssertEqual(
            ChatDestinationMentionResolver.resolve(
                draft: "@codex Review this",
                destinations: [destination]
            ),
            .resolved(destination: destination, prompt: "Review this")
        )
    }

    func testMultipleCodexDestinationsRemainAmbiguous() {
        let destinations = [
            Self.codexDestination(routeName: "chatgpt-sol"),
            Self.codexDestination(routeName: "chatgpt-fast"),
        ]

        XCTAssertEqual(
            ChatDestinationMentionResolver.resolve(
                draft: "@codex Review this",
                destinations: destinations
            ),
            .ambiguous(token: "codex", candidates: destinations)
        )
    }

    func testUnknownMentionRemainsLiteralPromptText() {
        XCTAssertEqual(
            ChatDestinationMentionResolver.resolve(
                draft: "@someone Please review this",
                destinations: [Self.appleDestination]
            ),
            .unknown(token: "someone")
        )
    }

    func testAmbiguousFriendlyAliasDoesNotChooseArbitrarily() {
        let destinations = [
            Self.appleDestination.withTitle("My Model"),
            Self.codexDestination(routeName: "chatgpt-sol").withTitle("My Model"),
        ]

        XCTAssertEqual(
            ChatDestinationMentionResolver.resolve(
                draft: "@my-model Review this",
                destinations: destinations
            ),
            .ambiguous(token: "my-model", candidates: destinations)
        )
    }

    func testSuggestionsRetainCanonicalDestinationsAndIncludeUnavailableRows() {
        let available = Self.codexDestination(routeName: "chatgpt-sol")
        let unavailable = Self.appleDestination.withAvailability(false)

        XCTAssertEqual(
            ChatDestinationMentionResolver.suggestions(
                for: "@",
                destinations: [available, unavailable]
            ),
            [.preferLocal, .preferHosted, available, unavailable]
        )
    }

    @MainActor
    func testTypedOverrideDeliversCleanPromptWithoutMutatingSessionThenReturnsToAutomatic() async throws {
        let recorder = MentionDestinationRecorder()
        let state = AppState(client: MentionRecordingClient(recorder: recorder))
        state.chatDraft = "@local Summarise this"

        state.sendChatDraft()
        try await waitUntil { !state.isSendingMessage }

        XCTAssertEqual(state.chatMessages.first?.text, "Summarise this")
        XCTAssertEqual(
            state.chatMessages.first?.requestedGatewayRouteName,
            "prefer-local"
        )
        XCTAssertEqual(state.chatDestination, .automatic)
        XCTAssertNil(state.chatMessageDestinationOverride)

        state.chatDraft = "And now continue"
        state.sendChatDraft()
        try await waitUntil { !state.isSendingMessage }

        let deliveries = await recorder.deliveries()
        XCTAssertEqual(deliveries.map(\.destination.gatewayModelValue), [
            "prefer-local",
            "auto",
        ])
        XCTAssertEqual(deliveries.map { $0.messages.last?.content }, [
            "Summarise this",
            "And now continue",
        ])
    }

    @MainActor
    func testUnknownMentionIsDeliveredLiterallyThroughTheSessionDestination() async throws {
        let recorder = MentionDestinationRecorder()
        let state = AppState(client: MentionRecordingClient(recorder: recorder))
        state.chatDraft = "@someone Please review this"

        state.sendChatDraft()
        try await waitUntil { !state.isSendingMessage }

        let recordedDeliveries = await recorder.deliveries()
        let delivery = try XCTUnwrap(recordedDeliveries.first)
        XCTAssertEqual(delivery.destination, .automatic)
        XCTAssertEqual(delivery.messages.last?.content, "@someone Please review this")
        XCTAssertEqual(state.chatMessages.first?.text, "@someone Please review this")
    }

    @MainActor
    func testUnavailableExplicitOverrideFailsClosed() async {
        let recorder = MentionDestinationRecorder()
        let state = AppState(client: MentionRecordingClient(recorder: recorder))
        state.selectChatMessageDestinationOverride(
            Self.appleDestination.withAvailability(false)
        )
        state.chatDraft = "Do not fall back"

        XCTAssertFalse(state.canSendMessage)
        state.sendChatDraft()

        XCTAssertTrue(state.chatMessages.isEmpty)
        let recordedDeliveries = await recorder.deliveries()
        XCTAssertTrue(recordedDeliveries.isEmpty)
        XCTAssertEqual(
            state.chatMessageDestinationOverride?.gatewayModelValue,
            "apple-local"
        )
    }

    @MainActor
    func testSelectingSuggestionStripsTokenAndShowsCanonicalPendingOverride() {
        let state = AppState(
            client: MentionRecordingClient(recorder: MentionDestinationRecorder())
        )
        state.chatDraft = "@mac-local"
        let destination = Self.appleDestination.withTitle("Mac Local")

        state.selectChatMessageDestinationOverride(destination)

        XCTAssertTrue(state.chatDraft.isEmpty)
        XCTAssertEqual(state.chatMessageDestinationOverride, destination)
        XCTAssertEqual(
            state.chatMessageDestinationOverride?.gatewayModelValue,
            "apple-local"
        )
    }

    @MainActor
    func testRetryReusesOriginalRequestedDestinationInsteadOfCurrentSessionDestination() async throws {
        let recorder = MentionDestinationRecorder()
        let client = MentionRecordingClient(
            recorder: recorder,
            overview: Self.overview(with: [Self.appleEndpoint])
        )
        let state = AppState(client: client)
        state.refreshStats()
        try await waitUntil { !state.isRefreshingStats }
        let destination = try XCTUnwrap(
            state.chatDestinations.first { $0.gatewayModelValue == "apple-local" }
        )
        state.selectChatMessageDestinationOverride(destination)
        state.chatDraft = "Keep the original route"

        state.sendChatDraft()
        try await waitUntil { !state.isSendingMessage }
        XCTAssertEqual(state.chatMessages.last?.state, .failed)

        state.chatDestination = .preferHosted
        state.retryLastChatTurn()
        try await waitUntil { !state.isSendingMessage }

        let deliveries = await recorder.deliveries()
        XCTAssertEqual(deliveries.map(\.destination.gatewayModelValue), [
            "apple-local",
            "apple-local",
        ])
        XCTAssertEqual(state.chatDestination.gatewayModelValue, "prefer-hosted")
    }

    func testConversationWithoutRequestedDestinationFieldsStillDecodes() throws {
        let message = ChatMessage(
            role: .user,
            text: "Old saved prompt",
            requestedGatewayRouteName: "apple-local",
            requestedDestinationTitle: "Apple Local"
        )
        let conversation = ChatConversation(messages: [message])
        let encoded = try JSONEncoder().encode([conversation])
        var root = try XCTUnwrap(
            JSONSerialization.jsonObject(with: encoded) as? [[String: Any]]
        )
        var messages = try XCTUnwrap(root[0]["messages"] as? [[String: Any]])
        messages[0].removeValue(forKey: "requestedGatewayRouteName")
        messages[0].removeValue(forKey: "requestedDestinationTitle")
        root[0]["messages"] = messages
        let legacyData = try JSONSerialization.data(withJSONObject: root)

        let decoded = try JSONDecoder().decode([ChatConversation].self, from: legacyData)

        XCTAssertEqual(decoded.first?.messages.first?.text, "Old saved prompt")
        XCTAssertNil(decoded.first?.messages.first?.requestedGatewayRouteName)
        XCTAssertNil(decoded.first?.messages.first?.requestedDestinationTitle)
    }

    @MainActor
    func testExistingPinnedComposerDestinationRemainsTheDeliveryDestination() async throws {
        let recorder = MentionDestinationRecorder()
        let state = AppState(client: MentionRecordingClient(recorder: recorder))
        state.chatDestination = .preferHosted
        state.chatDraft = "Use the pinned session destination"

        state.sendChatDraft()
        try await waitUntil { !state.isSendingMessage }

        let recordedDeliveries = await recorder.deliveries()
        let delivery = try XCTUnwrap(recordedDeliveries.first)
        XCTAssertEqual(delivery.destination, .preferHosted)
        XCTAssertEqual(state.chatDestination.gatewayModelValue, "prefer-hosted")
        XCTAssertEqual(
            state.chatMessages.first?.requestedGatewayRouteName,
            "prefer-hosted"
        )
    }

    @MainActor
    private func waitUntil(
        timeoutNanoseconds: UInt64 = 1_000_000_000,
        condition: @escaping @MainActor () -> Bool
    ) async throws {
        let started = ContinuousClock.now
        while !condition() {
            if ContinuousClock.now - started > .nanoseconds(Int64(timeoutNanoseconds)) {
                XCTFail("Timed out waiting for Chat state")
                return
            }
            try await Task.sleep(nanoseconds: 10_000_000)
        }
    }

    private static let appleEndpoint = EndpointDisplayStatus(
        name: "apple-local",
        providerName: "Apple Foundation Models",
        modelName: "system-default",
        state: .ready
    )

    private static let appleDestination = ChatDestination(endpoint: appleEndpoint)

    private static func codexDestination(routeName: String) -> ChatDestination {
        ChatDestination(
            routeName: routeName,
            title: routeName,
            detail: "ChatGPT · GPT-5.6 Sol",
            providerName: "ChatGPT"
        )
    }

    fileprivate static func overview(
        with endpoints: [EndpointDisplayStatus]
    ) -> GatewayOverview {
        GatewayOverview(
            gateway: .running(detail: "ready"),
            hosted: .ready(detail: "ready"),
            endpoints: endpoints,
            routingStats: RoutingStats(
                localPercent: 0,
                cloudPercent: 0,
                totalTurns: 0,
                savedToday: 0,
                savedLast30Days: 0,
                cloudSpendToday: 0,
                percentVsAlwaysCloud: 0,
                updatedAt: Date(),
                isRunning: true
            ),
            updatedAt: Date()
        )
    }
}

private actor MentionDestinationRecorder {
    struct Delivery: Sendable {
        let destination: ChatDestination
        let messages: [ChatRequestMessage]
    }

    private var recordedDeliveries: [Delivery] = []

    func record(destination: ChatDestination, messages: [ChatRequestMessage]) {
        recordedDeliveries.append(Delivery(
            destination: destination,
            messages: messages
        ))
    }

    func deliveries() -> [Delivery] {
        recordedDeliveries
    }
}

private struct MentionRecordingClient: WayfinderClient {
    let recorder: MentionDestinationRecorder
    var overview: GatewayOverview?

    func route(prompt: String) async throws -> RoutingDecision {
        throw WayfinderClientError.invalidChatStream
    }

    func streamChat(
        messages: [ChatRequestMessage]
    ) -> AsyncThrowingStream<ChatStreamEvent, Error> {
        streamChat(messages: messages, destination: .automatic)
    }

    func streamChat(
        messages: [ChatRequestMessage],
        destination: ChatDestination
    ) -> AsyncThrowingStream<ChatStreamEvent, Error> {
        AsyncThrowingStream { continuation in
            Task {
                await recorder.record(destination: destination, messages: messages)
                continuation.finish()
            }
        }
    }

    func loadStats(range: StatsRange) async throws -> RoutingStats {
        overview?.routingStats ?? ChatDestinationMentionTests.overview(with: []).routingStats
    }

    func loadOverview() async throws -> GatewayOverview {
        overview ?? ChatDestinationMentionTests.overview(with: [])
    }
}
