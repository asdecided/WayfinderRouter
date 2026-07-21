import AppKit
import XCTest
@testable import WayfinderMacCore

@MainActor
final class ChatStateTests: XCTestCase {
    func testChatWindowPersistsWhenTheAccessoryAppDeactivates() {
        let window = NSWindow()
        window.hidesOnDeactivate = true

        ChatWindowBehavior.apply(to: window)

        XCTAssertFalse(window.hidesOnDeactivate)
        XCTAssertFalse(window.isReleasedWhenClosed)
        XCTAssertTrue(window.collectionBehavior.contains(.managed))
    }

    func testAppStateStartsWithAnEmptyChatSession() {
        let state = AppState(client: MockWayfinderClient())

        XCTAssertTrue(state.chatMessages.isEmpty)
        XCTAssertTrue(state.chatDraft.isEmpty)
        XCTAssertFalse(state.isSendingMessage)
        XCTAssertEqual(state.gatewayOverview.gateway.title, "Checking")
        XCTAssertEqual(state.gatewayOverview.hosted.title, "Checking")
        XCTAssertEqual(state.routingStats.totalTurns, 0)
    }

    func testNewChatRetainsPreviousConversationForSelection() throws {
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString, isDirectory: true)
        let store = ChatConversationStore(
            fileURL: directory.appendingPathComponent("chat-history.json")
        )
        let previous = ChatConversation(messages: [
            ChatMessage(role: .user, text: "Keep this conversation"),
            ChatMessage(role: .assistant, text: "It will remain available.")
        ])
        store.save([previous])
        defer { try? FileManager.default.removeItem(at: directory) }

        let state = AppState(
            client: MockWayfinderClient(),
            chatConversationStore: store
        )

        XCTAssertEqual(state.chatMessages, previous.messages)
        state.startNewChat()
        XCTAssertTrue(state.chatMessages.isEmpty)
        XCTAssertEqual(state.chatConversations.map(\.id), [previous.id])

        state.selectChatConversation(previous.id)
        XCTAssertEqual(state.activeChatConversationID, previous.id)
        XCTAssertEqual(state.chatMessages, previous.messages)
    }

    func testConversationStorePersistsNewestFirst() throws {
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString, isDirectory: true)
        let store = ChatConversationStore(
            fileURL: directory.appendingPathComponent("chat-history.json")
        )
        defer { try? FileManager.default.removeItem(at: directory) }
        let older = ChatConversation(
            messages: [ChatMessage(role: .user, text: "Older")],
            updatedAt: Date(timeIntervalSince1970: 1)
        )
        let newer = ChatConversation(
            messages: [ChatMessage(role: .user, text: "Newer")],
            updatedAt: Date(timeIntervalSince1970: 2)
        )

        store.save([older, newer])

        XCTAssertEqual(store.load().map(\.id), [newer.id, older.id])
        XCTAssertEqual(store.load().map(\.title), ["Newer", "Older"])
    }

    func testFailedResponseRemainsDistinctFromPendingTurn() {
        let prompt = ChatMessage(role: .user, text: "Route this")
        let failure = ChatMessage(role: .assistant, text: "Gateway unavailable", state: .failed)

        let pending = ChatTurn.make(from: [prompt])
        let failed = ChatTurn.make(from: [prompt, failure])

        XCTAssertNil(pending.first?.response)
        XCTAssertEqual(failed.first?.response, failure)
        XCTAssertNil(failed.first?.response?.decision)
    }

    func testRoutingInspectorStateCoversTheTurnLifecycle() {
        let prompt = ChatMessage(role: .user, text: "Route this")
        let decision = RoutingDecision(
            prompt: prompt.text,
            route: .local,
            provider: "apple-local",
            score: 0.2,
            mode: "balanced",
            explanation: "The prompt is suitable for local inference.",
            features: []
        )

        XCTAssertEqual(turn(prompt: prompt, response: nil).routingInspectionState, .waiting)
        XCTAssertEqual(
            turn(
                prompt: prompt,
                response: ChatMessage(role: .assistant, text: "Gateway unavailable", state: .failed)
            ).routingInspectionState,
            .failed("Gateway unavailable", nil)
        )
        XCTAssertEqual(
            turn(prompt: prompt, response: ChatMessage(role: .assistant, text: "Partial", state: .stopped))
                .routingInspectionState,
            .stopped(nil)
        )
        XCTAssertEqual(
            turn(prompt: prompt, response: ChatMessage(role: .assistant, text: "Reply"))
                .routingInspectionState,
            .unavailable
        )
        XCTAssertEqual(
            turn(
                prompt: prompt,
                response: ChatMessage(role: .assistant, text: "Reply", decision: decision)
            ).routingInspectionState,
            .routed(decision)
        )
        XCTAssertEqual(
            turn(
                prompt: prompt,
                response: ChatMessage(
                    role: .assistant,
                    text: "Provider failed after routing",
                    decision: decision,
                    state: .failed
                )
            ).routingInspectionState,
            .failed("Provider failed after routing", decision)
        )
        XCTAssertEqual(
            turn(
                prompt: prompt,
                response: ChatMessage(
                    role: .assistant,
                    text: "Partial",
                    decision: decision,
                    state: .stopped
                )
            ).routingInspectionState,
            .stopped(decision)
        )
    }

    func testNavigatorFiltersNeverAlterTheTranscript() {
        let localPrompt = ChatMessage(role: .user, text: "Summarize this")
        let cloudPrompt = ChatMessage(role: .user, text: "Design a migration strategy")
        let pendingPrompt = ChatMessage(role: .user, text: "Still routing")
        let localTurn = turn(
            prompt: localPrompt,
            response: ChatMessage(
                role: .assistant,
                text: "Local reply",
                decision: decision(prompt: localPrompt.text, route: .local)
            )
        )
        let cloudTurn = turn(
            prompt: cloudPrompt,
            response: ChatMessage(
                role: .assistant,
                text: "Cloud reply",
                decision: decision(prompt: cloudPrompt.text, route: .cloud)
            )
        )
        let pendingTurn = turn(prompt: pendingPrompt, response: nil)
        let turns = [localTurn, cloudTurn, pendingTurn]

        let localWorkspace = ChatWorkspaceContent(turns: turns, routeFilter: .local, searchText: "")
        XCTAssertEqual(localWorkspace.transcriptTurns, turns)
        XCTAssertEqual(localWorkspace.navigatorTurns, [localTurn])

        let searchWorkspace = ChatWorkspaceContent(
            turns: turns,
            routeFilter: .all,
            searchText: "migration"
        )
        XCTAssertEqual(searchWorkspace.transcriptTurns, turns)
        XCTAssertEqual(searchWorkspace.navigatorTurns, [cloudTurn])

        let replySearchWorkspace = ChatWorkspaceContent(
            turns: turns,
            routeFilter: .all,
            searchText: "cloud reply"
        )
        XCTAssertEqual(replySearchWorkspace.transcriptTurns, turns)
        XCTAssertEqual(replySearchWorkspace.navigatorTurns, [cloudTurn])
    }

    func testManualTurnSelectionIsNotStolenByANewerTurn() {
        let older = UUID()
        let newer = UUID()

        XCTAssertEqual(
            ChatWorkspaceSelectionPolicy.resolvedTurnID(
                current: older,
                followsLatest: false,
                turnIDs: [older, newer]
            ),
            older
        )
        XCTAssertEqual(
            ChatWorkspaceSelectionPolicy.resolvedTurnID(
                current: older,
                followsLatest: true,
                turnIDs: [older, newer]
            ),
            newer
        )
        XCTAssertEqual(
            ChatWorkspaceSelectionPolicy.resolvedTurnID(
                current: UUID(),
                followsLatest: false,
                turnIDs: [older, newer]
            ),
            newer
        )
        XCTAssertNil(
            ChatWorkspaceSelectionPolicy.resolvedTurnID(
                current: older,
                followsLatest: false,
                turnIDs: []
            )
        )
    }

    func testChatWorkspaceSizingKeepsTheConversationPrimary() {
        let defaultConversationWidth = ChatWorkspaceChrome.initialWindowWidth
            - ChatWorkspaceChrome.sidebarWidth
        let minimumConversationWidth = ChatWorkspaceChrome.minimumWindowWidth
            - ChatWorkspaceChrome.sidebarMinimumWidth

        XCTAssertGreaterThanOrEqual(defaultConversationWidth, ChatWorkspaceChrome.conversationWidth)
        XCTAssertGreaterThanOrEqual(minimumConversationWidth, 640)
    }

    func testStreamingOnlyFollowsAnExplicitlySelectedLatestTurn() {
        let older = UUID()
        let latest = UUID()

        XCTAssertTrue(
            ChatScrollFollowPolicy.shouldFollowLatest(
                isNearBottom: true,
                selectedTurnID: latest,
                latestTurnID: latest
            )
        )
        XCTAssertFalse(
            ChatScrollFollowPolicy.shouldFollowLatest(
                isNearBottom: true,
                selectedTurnID: older,
                latestTurnID: latest
            )
        )
        XCTAssertFalse(
            ChatScrollFollowPolicy.shouldFollowLatest(
                isNearBottom: false,
                selectedTurnID: latest,
                latestTurnID: latest
            )
        )
    }

    private func turn(prompt: ChatMessage, response: ChatMessage?) -> ChatTurn {
        ChatTurn(id: prompt.id, prompt: prompt, response: response)
    }

    private func decision(prompt: String, route: RouteTarget) -> RoutingDecision {
        RoutingDecision(
            prompt: prompt,
            route: route,
            provider: route == .local ? "apple-local" : "gpt-5.6",
            score: route == .local ? 0.2 : 0.8,
            mode: "balanced",
            explanation: "Test routing decision.",
            features: []
        )
    }
}
