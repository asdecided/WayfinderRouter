import Foundation
import XCTest
@testable import WayfinderMacCore

final class ChatDeliveryTests: XCTestCase {
    func testChatDestinationsUseGatewayAliasesAndKeepAutomaticAsDefault() {
        let endpoint = EndpointDisplayStatus(
            name: "chatgpt-sol",
            providerName: "ChatGPT",
            modelName: "gpt-5.6-sol",
            state: .ready
        )
        let destination = ChatDestination(endpoint: endpoint)

        XCTAssertEqual(ChatDestination.automatic.gatewayModelValue, "auto")
        XCTAssertTrue(ChatDestination.automatic.isAutomatic)
        XCTAssertEqual(destination.gatewayModelValue, "chatgpt-sol")
        XCTAssertEqual(destination.title, "GPT-5.6 Sol")
        XCTAssertEqual(destination.detail, "ChatGPT · GPT-5.6 Sol")
        XCTAssertFalse(destination.isAutomatic)
        XCTAssertTrue(destination.isChatGPTAccount)
    }

    func testAppleChatDestinationUsesFriendlyLocalPresentation() {
        let destination = ChatDestination(endpoint: EndpointDisplayStatus(
            name: "apple-local",
            providerName: "Apple Foundation Models",
            modelName: "system-default",
            state: .ready
        ))

        XCTAssertEqual(destination.title, "Apple Local")
        XCTAssertEqual(destination.defaultTitle, "Apple Local")
        XCTAssertEqual(destination.detail, "Apple Foundation Models · This Mac")
        XCTAssertEqual(destination.gatewayModelValue, "apple-local")
    }

    @MainActor
    func testPersonalDestinationNamePersistsWithoutChangingGatewayAlias() throws {
        let suiteName = "ChatDestinationNameStoreTests.\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suiteName))
        defer { defaults.removePersistentDomain(forName: suiteName) }
        let store = ChatDestinationNameStore(defaults: defaults)

        store.setName("  Mac   Local  ", for: "apple-local")
        let reloaded = ChatDestinationNameStore(defaults: defaults)
        let destination = ChatDestination(endpoint: EndpointDisplayStatus(
            name: "apple-local",
            providerName: "Apple Foundation Models",
            modelName: "system-default",
            state: .ready
        )).withTitle(reloaded.name(for: "apple-local", default: "Apple Local"))

        XCTAssertEqual(destination.title, "Mac Local")
        XCTAssertEqual(destination.defaultTitle, "Apple Local")
        XCTAssertEqual(destination.gatewayModelValue, "apple-local")

        reloaded.resetName(for: "apple-local")
        XCTAssertEqual(reloaded.name(for: "apple-local", default: destination.defaultTitle), "Apple Local")
    }

    func testPersonalDestinationNamesAreBoundedAndBlankNamesReset() async throws {
        await MainActor.run {
            let suiteName = "ChatDestinationNameStoreTests.\(UUID().uuidString)"
            let defaults = UserDefaults(suiteName: suiteName)!
            defer { defaults.removePersistentDomain(forName: suiteName) }
            let store = ChatDestinationNameStore(defaults: defaults)

            store.setName(String(repeating: "x", count: 80), for: "apple-local")
            XCTAssertEqual(store.override(for: "apple-local")?.count, ChatDestinationNameStore.maximumNameLength)

            store.setName("   \n\t", for: "apple-local")
            XCTAssertNil(store.override(for: "apple-local"))
        }
    }

    @MainActor
    func testAppStateAppliesPersonalNameWhenRefreshingDestinations() async throws {
        let suiteName = "ChatDestinationNameStoreTests.\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suiteName))
        defer { defaults.removePersistentDomain(forName: suiteName) }
        let nameStore = ChatDestinationNameStore(defaults: defaults)
        nameStore.setName("Mac Local", for: "apple-local")
        let overview = GatewayOverview(
            gateway: .running(detail: "ready"),
            hosted: .ready(detail: "ready"),
            endpoints: [EndpointDisplayStatus(
                name: "apple-local",
                providerName: "Apple Foundation Models",
                modelName: "system-default",
                state: .ready
            )],
            routingStats: .emptyForChatTests,
            updatedAt: Date()
        )
        let state = AppState(
            client: OverviewClient(overview: overview),
            chatDestinationNameStore: nameStore
        )

        state.refreshStats()
        try await waitUntil { !state.isRefreshingStats }

        XCTAssertEqual(state.chatDestinations.last?.title, "Mac Local")
        XCTAssertEqual(state.chatDestinations.last?.gatewayModelValue, "apple-local")
    }

    func testChatDestinationListOmitsUnadvertisedCodexRoutesAndReservesAuto() {
        let overview = GatewayOverview(
            gateway: .running(detail: "ready"),
            hosted: .ready(detail: "ready"),
            endpoints: [
                EndpointDisplayStatus(
                    name: "auto",
                    providerName: "Local",
                    state: .ready
                ),
                EndpointDisplayStatus(
                    name: "chatgpt-sol",
                    providerName: "ChatGPT",
                    modelName: "gpt-5.6-sol",
                    state: .unavailable,
                    isChatDestinationAvailable: false
                ),
                EndpointDisplayStatus(
                    name: "local",
                    providerName: "Apple Foundation Models",
                    modelName: "system-default",
                    state: .ready
                ),
            ],
            routingStats: .emptyForChatTests,
            updatedAt: Date()
        )

        XCTAssertEqual(
            AppState.chatDestinations(from: overview).map(\.gatewayModelValue),
            ["auto", "local"]
        )
    }

    func testPinnedChatGPTReadinessFailurePointsToAccountsWithoutLeakingDetail() {
        let destination = ChatDestination(
            routeName: "chatgpt-sol",
            title: "chatgpt-sol",
            detail: "ChatGPT · gpt-5.6-sol",
            providerName: "ChatGPT"
        )
        let message = AppState.chatErrorMessage(
            WayfinderClientError.gatewayStatus(503, model: "chatgpt-sol"),
            destination: destination
        )

        XCTAssertEqual(
            message,
            "ChatGPT is not connected or its Codex model is unavailable. Check Accounts in Settings, then retry."
        )
        XCTAssertEqual(
            AppState.chatErrorMessage(
                WayfinderClientError.chatAccountNotReady,
                destination: destination
            ),
            "ChatGPT is not connected or its Codex model is unavailable. Check Accounts in Settings, then retry."
        )
        XCTAssertEqual(
            AppState.chatRecoverySettingsSection(
                WayfinderClientError.chatAccountNotReady,
                destination: destination
            ),
            .connections
        )
        XCTAssertEqual(
            AppState.chatRecoverySettingsSection(
                WayfinderClientError.chatAccountNotReady,
                destination: .automatic
            ),
            .connections
        )
        XCTAssertEqual(
            AppState.chatRecoverySettingsSection(
                WayfinderClientError.invalidChatStream,
                destination: destination
            ),
            .gateway
        )
    }

    func testPinnedChatGPTTurnFailuresRemainDistinctFromAccountReadiness() {
        let destination = ChatDestination(
            routeName: "chatgpt-sol",
            title: "chatgpt-sol",
            detail: "ChatGPT · GPT-5.6 Sol",
            providerName: "ChatGPT"
        )

        XCTAssertEqual(
            AppState.chatErrorMessage(
                WayfinderClientError.gatewayStatus(502, model: "chatgpt-sol"),
                destination: destination
            ),
            "ChatGPT could not complete this reply. Retry, or choose another destination."
        )
        XCTAssertEqual(
            AppState.chatErrorMessage(
                WayfinderClientError.chatTurnFailed,
                destination: destination
            ),
            "ChatGPT could not complete this reply. Retry, or choose another destination."
        )
        XCTAssertEqual(
            AppState.chatErrorMessage(
                WayfinderClientError.gatewayStatus(409, model: "chatgpt-sol"),
                destination: destination
            ),
            "ChatGPT interrupted this reply before completion. Retry when you're ready."
        )
        XCTAssertEqual(
            AppState.chatErrorMessage(
                WayfinderClientError.chatTurnInterrupted,
                destination: destination
            ),
            "ChatGPT interrupted this reply before completion. Retry when you're ready."
        )
        XCTAssertEqual(
            AppState.chatErrorMessage(
                WayfinderClientError.chatProviderBusy,
                destination: destination
            ),
            "ChatGPT is already answering another request. Wait for it to finish, then retry."
        )
        XCTAssertEqual(
            AppState.chatErrorMessage(
                WayfinderClientError.gatewayStatus(429, model: "chatgpt-sol"),
                destination: destination
            ),
            "This ChatGPT account has reached its current usage limit. Try again later, or choose another destination."
        )
        XCTAssertEqual(
            AppState.chatErrorMessage(
                WayfinderClientError.chatUsageLimitReached,
                destination: destination
            ),
            "This ChatGPT account has reached its current usage limit. Try again later, or choose another destination."
        )
    }

    func testGatewayStreamDecoderPreservesDecisionTextAndCompletionOrder() throws {
        var decoder = GatewayStreamDecoder(prompt: "Hello")
        let metadata = #"data: {"wayfinder":{"model":"apple-local","score":0.1,"mode":"scored","features":{"word_count":1},"contributions":[],"tiers":[{"min_score":0.0,"model":"apple-local"},{"min_score":0.5,"model":"cloud"}]}}"#

        let first = try decoder.consume(line: metadata)
        let second = try decoder.consume(line: #"data: {"choices":[{"delta":{"content":"Hi "}}]}"#)
        let third = try decoder.consume(line: #"data: {"choices":[{"delta":{"content":"there"}}]}"#)
        let done = try decoder.consume(line: "data: [DONE]")

        guard case let .decision(decision) = first.first else {
            return XCTFail("Expected decision metadata first")
        }
        XCTAssertEqual(decision.prompt, "Hello")
        XCTAssertEqual(decision.provider, "apple-local")
        XCTAssertEqual(decision.route, .local)
        XCTAssertEqual(second, [.text("Hi ")])
        XCTAssertEqual(third, [.text("there")])
        XCTAssertEqual(done, [.completed])
        XCTAssertTrue(decoder.sawDecision)
        XCTAssertTrue(decoder.sawCompletion)
    }

    func testGatewayStreamDecoderRejectsErrorEventsWithoutEchoingPayload() {
        var decoder = GatewayStreamDecoder(prompt: "private prompt")

        XCTAssertThrowsError(
            try decoder.consume(line: #"data: {"error":{"message":"private upstream detail","type":"upstream"}}"#)
        ) { error in
            XCTAssertEqual(error as? WayfinderClientError, .invalidChatStream)
            XCTAssertFalse(error.localizedDescription.contains("private"))
        }
    }

    func testGatewayStreamDecoderPreservesCodexTerminalCategories() {
        for (type, expected) in [
            ("wayfinder_router_turn_failed", WayfinderClientError.chatTurnFailed),
            ("wayfinder_router_interrupted", WayfinderClientError.chatTurnInterrupted),
            ("wayfinder_router_busy", WayfinderClientError.chatProviderBusy),
            ("wayfinder_router_usage_limited", WayfinderClientError.chatUsageLimitReached),
            ("wayfinder_router_not_ready", WayfinderClientError.chatAccountNotReady),
        ] {
            var decoder = GatewayStreamDecoder(prompt: "private prompt")
            XCTAssertThrowsError(
                try decoder.consume(line: #"data: {"error":{"message":"sanitized","type":"\#(type)"}}"#)
            ) { error in
                XCTAssertEqual(error as? WayfinderClientError, expected)
            }
        }
    }

    func testGatewayStreamDecoderRequiresMetadataFirstAndBoundsOutput() throws {
        var missingMetadata = GatewayStreamDecoder(prompt: "Hello")
        XCTAssertThrowsError(
            try missingMetadata.consume(line: #"data: {"choices":[{"delta":{"content":"early"}}]}"#)
        )

        var bounded = GatewayStreamDecoder(prompt: "Hello", maximumResponseCharacters: 4)
        _ = try bounded.consume(line: #"data: {"wayfinder":{"model":"local","score":0.1,"mode":"scored","features":{},"contributions":[],"tiers":[{"min_score":0.0,"model":"local"}]}}"#)
        XCTAssertEqual(
            try bounded.consume(line: #"data: {"choices":[{"delta":{"content":"four"}}]}"#),
            [.text("four")]
        )
        XCTAssertThrowsError(
            try bounded.consume(line: #"data: {"choices":[{"delta":{"content":"!"}}]}"#)
        )
    }

    func testConversationBoundsKeepRecentCompleteTurnsAndRejectOversizedMessages() throws {
        let messages = (0..<24).map {
            ChatRequestMessage(role: $0.isMultiple(of: 2) ? "user" : "assistant", content: "message-\($0)")
        }

        let bounded = try GatewayWayfinderClient.boundedChatMessages(messages)

        XCTAssertEqual(bounded.count, 20)
        XCTAssertEqual(bounded.first?.content, "message-4")
        XCTAssertEqual(bounded.last?.content, "message-23")
        XCTAssertThrowsError(
            try GatewayWayfinderClient.boundedChatMessages([
                ChatRequestMessage(role: "user", content: String(repeating: "x", count: 32_769))
            ])
        ) { error in
            XCTAssertEqual(error as? WayfinderClientError, .conversationTooLarge)
        }
    }

    func testGatewayFailureUsesTheSelectedAppleModelWithoutReflectingResponseContent() throws {
        let response = try XCTUnwrap(HTTPURLResponse(
            url: URL(string: "http://127.0.0.1:8088/v1/chat/completions")!,
            statusCode: 503,
            httpVersion: nil,
            headerFields: ["X-Wayfinder-Router-Model": "apple-local"]
        ))

        let error = GatewayWayfinderClient.gatewayError(for: response)

        XCTAssertEqual(error, .gatewayStatus(503, model: "apple-local"))
        XCTAssertEqual(
            error.localizedDescription,
            "Apple Foundation Models aren't ready for this app. Check Apple Intelligence and app signing, or choose another model in Gateway Settings."
        )
    }

    func testGatewayHTTPErrorTypesPreserveCanonicalChatCategoriesWithoutReflectingMessages() throws {
        for (status, type, expected) in [
            (409, "wayfinder_router_interrupted", WayfinderClientError.chatTurnInterrupted),
            (409, "wayfinder_router_busy", WayfinderClientError.chatProviderBusy),
            (429, "wayfinder_router_usage_limited", WayfinderClientError.chatUsageLimitReached),
        ] {
            let response = try XCTUnwrap(HTTPURLResponse(
                url: URL(string: "http://127.0.0.1:8088/v1/chat/completions")!,
                statusCode: status,
                httpVersion: nil,
                headerFields: [
                    "Content-Type": "application/json; charset=utf-8",
                    "X-Wayfinder-Router-Model": "chatgpt-sol",
                ]
            ))
            let body = Data(
                #"{"error":{"message":"private upstream detail","type":"\#(type)"}}"#.utf8
            )

            let error = GatewayWayfinderClient.gatewayError(for: response, body: body)

            XCTAssertEqual(error, expected)
            XCTAssertFalse(error.localizedDescription.contains("private"))
        }

        let notReady = try XCTUnwrap(HTTPURLResponse(
            url: URL(string: "http://127.0.0.1:8088/v1/chat/completions")!,
            statusCode: 503,
            httpVersion: nil,
            headerFields: [
                "Content-Type": "application/json",
                "X-Wayfinder-Router-Model": "chatgpt-sol",
            ]
        ))
        let body = Data(
            #"{"error":{"message":"private upstream detail","type":"wayfinder_router_not_ready"}}"#.utf8
        )
        let error = GatewayWayfinderClient.gatewayError(
            for: notReady,
            body: body,
            isExplicitChatGPTDestination: true
        )
        XCTAssertEqual(error, .chatAccountNotReady)
        XCTAssertFalse(error.localizedDescription.contains("private"))
    }

    func testGatewayHTTPErrorTypeRejectsUnknownMismatchedAndOversizedBodies() throws {
        let response = try XCTUnwrap(HTTPURLResponse(
            url: URL(string: "http://127.0.0.1:8088/v1/chat/completions")!,
            statusCode: 409,
            httpVersion: nil,
            headerFields: [
                "Content-Type": "application/json",
                "X-Wayfinder-Router-Model": "chatgpt-sol",
            ]
        ))
        let fallback = WayfinderClientError.gatewayStatus(409, model: "chatgpt-sol")
        let unknown = Data(
            #"{"error":{"message":"private upstream detail","type":"upstream_busy"}}"#.utf8
        )
        let oversized = Data(
            (#"{"error":{"type":"wayfinder_router_busy","padding":""#
                + String(repeating: "x", count: GatewayWayfinderClient.maximumGatewayErrorBodyBytes)
                + #""}}"#).utf8
        )

        XCTAssertEqual(
            GatewayWayfinderClient.gatewayError(for: response, body: unknown),
            fallback
        )
        XCTAssertEqual(
            GatewayWayfinderClient.gatewayError(for: response, body: oversized),
            fallback
        )

        for (status, type) in [
            (500, "wayfinder_router_busy"),
            (409, "wayfinder_router_usage_limited"),
            (502, "wayfinder_router_turn_failed"),
        ] {
            let mismatch = try XCTUnwrap(HTTPURLResponse(
                url: response.url!,
                statusCode: status,
                httpVersion: nil,
                headerFields: [
                    "Content-Type": "application/json",
                    "X-Wayfinder-Router-Model": "chatgpt-sol",
                ]
            ))
            let mismatchBody = Data(#"{"error":{"type":"\#(type)"}}"#.utf8)
            XCTAssertEqual(
                GatewayWayfinderClient.gatewayError(for: mismatch, body: mismatchBody),
                .gatewayStatus(status, model: "chatgpt-sol")
            )
        }

        let missingContentType = try XCTUnwrap(HTTPURLResponse(
            url: response.url!,
            statusCode: 409,
            httpVersion: nil,
            headerFields: ["X-Wayfinder-Router-Model": "chatgpt-sol"]
        ))
        let busy = Data(#"{"error":{"type":"wayfinder_router_busy"}}"#.utf8)
        XCTAssertEqual(
            GatewayWayfinderClient.gatewayError(for: missingContentType, body: busy),
            fallback
        )
        XCTAssertFalse(fallback.localizedDescription.contains("private"))
    }

    func testAppleNotReadyHTTPEnvelopeRemainsAnAppleGatewayFailure() throws {
        let response = try XCTUnwrap(HTTPURLResponse(
            url: URL(string: "http://127.0.0.1:8088/v1/chat/completions")!,
            statusCode: 503,
            httpVersion: nil,
            headerFields: [
                "Content-Type": "application/json",
                "X-Wayfinder-Router-Model": "apple-local",
            ]
        ))
        let body = Data(
            #"{"error":{"message":"private native detail","type":"wayfinder_router_not_ready"}}"#.utf8
        )

        let error = GatewayWayfinderClient.gatewayError(for: response, body: body)

        XCTAssertEqual(error, .gatewayStatus(503, model: "apple-local"))
        XCTAssertTrue(error.localizedDescription.contains("Apple Foundation Models"))
        XCTAssertFalse(error.localizedDescription.contains("private"))
    }

    func testRouteRejectsDeclaredOversizedGatewayBodiesBeforeDecoding() async throws {
        for status in [200, 409] {
            ChatURLProtocolStub.install { request in
                let maximum = status == 200
                    ? GatewayWayfinderClient.maximumBufferedGatewayResponseBytes
                    : GatewayWayfinderClient.maximumGatewayErrorBodyBytes
                let response = try XCTUnwrap(HTTPURLResponse(
                    url: request.url!,
                    statusCode: status,
                    httpVersion: nil,
                    headerFields: [
                        "Content-Length": "\(maximum + 1)",
                        "Content-Type": "application/json",
                        "X-Wayfinder-Router-Model": "chatgpt-sol",
                    ]
                ))
                return (
                    response,
                    Data(#"{"error":{"type":"wayfinder_router_busy"}}"#.utf8)
                )
            }
            let configuration = URLSessionConfiguration.ephemeral
            configuration.protocolClasses = [ChatURLProtocolStub.self]
            let session = URLSession(configuration: configuration)
            let client = GatewayWayfinderClient(
                baseURL: URL(string: "http://127.0.0.1:8088")!,
                session: session,
                runtimeValidation: { URL(fileURLWithPath: "/test/wayfinder-router") },
                appleProductReadiness: { true }
            )

            do {
                _ = try await client.route(prompt: "Hello")
                XCTFail("A declared oversized gateway body must be rejected")
            } catch {
                if status == 200 {
                    XCTAssertEqual(error as? WayfinderClientError, .gatewayResponseTooLarge)
                } else {
                    XCTAssertEqual(
                        error as? WayfinderClientError,
                        .gatewayStatus(409, model: "chatgpt-sol")
                    )
                }
            }
            session.invalidateAndCancel()
            ChatURLProtocolStub.reset()
        }
    }

    func testRouteRejectsUnverifiedRuntimeBeforeAnyLoopbackRequest() async {
        ChatURLProtocolStub.install { _ in
            XCTFail("Runtime validation must happen before the request")
            throw URLError(.badServerResponse)
        }
        defer { ChatURLProtocolStub.reset() }
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [ChatURLProtocolStub.self]
        let session = URLSession(configuration: configuration)
        defer { session.invalidateAndCancel() }
        let client = GatewayWayfinderClient(
            baseURL: URL(string: "http://127.0.0.1:8088")!,
            session: session,
            runtimeValidation: { throw VerifiedGatewayRuntimeError.serviceNeedsRepair }
        )

        do {
            _ = try await client.route(prompt: "Hello")
            XCTFail("Expected the unverified runtime to be rejected")
        } catch {
            XCTAssertEqual(error as? VerifiedGatewayRuntimeError, .serviceNeedsRepair)
        }
    }

    func testStreamingHTTPBusyEnvelopeIsPreservedEndToEnd() async throws {
        ChatURLProtocolStub.install { request in
            XCTAssertEqual(request.httpMethod, "POST")
            XCTAssertEqual(request.url?.path, "/v1/chat/completions")
            let response = try XCTUnwrap(HTTPURLResponse(
                url: request.url!,
                statusCode: 409,
                httpVersion: nil,
                headerFields: [
                    "Content-Type": "application/json",
                    "X-Wayfinder-Router-Model": "chatgpt-sol",
                ]
            ))
            return (
                response,
                Data(
                    #"{"error":{"message":"private upstream detail","type":"wayfinder_router_busy"}}"#.utf8
                )
            )
        }
        defer { ChatURLProtocolStub.reset() }

        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [ChatURLProtocolStub.self]
        let session = URLSession(configuration: configuration)
        defer { session.invalidateAndCancel() }
        let client = GatewayWayfinderClient(
            baseURL: URL(string: "http://127.0.0.1:8088")!,
            session: session,
            runtimeValidation: { URL(fileURLWithPath: "/test/wayfinder-router") },
            appleProductReadiness: { true }
        )
        let destination = ChatDestination(
            routeName: "chatgpt-sol",
            title: "chatgpt-sol",
            detail: "ChatGPT · GPT-5.6 Sol",
            providerName: "ChatGPT"
        )

        do {
            for try await _ in client.streamChat(
                messages: [ChatRequestMessage(role: "user", content: "Hello")],
                destination: destination
            ) {
                XCTFail("A rejected request must not produce stream events")
            }
            XCTFail("Expected the HTTP Busy envelope to terminate the stream")
        } catch {
            XCTAssertEqual(error as? WayfinderClientError, .chatProviderBusy)
            XCTAssertFalse(error.localizedDescription.contains("private"))
        }
    }

    @MainActor
    func testAppStateBuildsARealAssistantReplyAndRetainsDecision() async throws {
        let decision = Self.decision
        let state = AppState(client: ScriptedChatClient(events: [
            .decision(decision),
            .text("Hello "),
            .text("from Wayfinder."),
            .completed,
        ]))
        state.chatDraft = "Hello"

        state.sendChatDraft()
        try await waitUntil { !state.isSendingMessage }

        XCTAssertEqual(state.chatMessages.count, 2)
        XCTAssertEqual(state.chatMessages[0].role, .user)
        XCTAssertEqual(state.chatMessages[1].role, .assistant)
        XCTAssertEqual(state.chatMessages[1].text, "Hello from Wayfinder.")
        XCTAssertEqual(state.chatMessages[1].decision, decision)
        XCTAssertEqual(state.chatMessages[1].state, .complete)
        XCTAssertEqual(AppState.chatRequestMessages(from: state.chatMessages), [
            ChatRequestMessage(role: "user", content: "Hello"),
            ChatRequestMessage(role: "assistant", content: "Hello from Wayfinder."),
        ])
    }

    @MainActor
    func testAppStatePinsTheSelectedGatewayAliasForTheWholeTurn() async throws {
        let recorder = DestinationRecorder()
        let state = AppState(client: DestinationRecordingClient(recorder: recorder))
        state.chatDestination = ChatDestination(
            routeName: "chatgpt-sol",
            title: "chatgpt-sol",
            detail: "ChatGPT · GPT-5.6 Sol",
            providerName: "ChatGPT"
        )
        state.chatDraft = "Hello"

        state.sendChatDraft()
        try await waitUntil { !state.isSendingMessage }

        let recorded = await recorder.value()
        XCTAssertEqual(recorded?.gatewayModelValue, "chatgpt-sol")
    }

    @MainActor
    func testUnavailablePinnedDestinationNeverResetsToAutomatic() async throws {
        let state = AppState(client: OverviewClient(overview: GatewayOverview(
            gateway: .running(detail: "ready"),
            hosted: .ready(detail: "ready"),
            endpoints: [],
            routingStats: .emptyForChatTests,
            updatedAt: Date()
        )))
        state.chatDestination = ChatDestination(
            routeName: "chatgpt-sol",
            title: "chatgpt-sol",
            detail: "ChatGPT · GPT-5.6 Sol",
            providerName: "ChatGPT"
        )
        state.chatDraft = "Keep this pinned"

        state.refreshStats()
        try await waitUntil { !state.isRefreshingStats }

        XCTAssertEqual(state.chatDestination.gatewayModelValue, "chatgpt-sol")
        XCTAssertFalse(state.chatDestination.isAvailable)
        XCTAssertFalse(state.canSendMessage)
        XCTAssertEqual(state.chatDestinations.last?.gatewayModelValue, "chatgpt-sol")
        state.sendChatDraft()
        XCTAssertTrue(state.chatMessages.isEmpty)
    }

    @MainActor
    func testUnavailablePinnedDestinationCannotDiscardAndRetryAFailedTurn() async throws {
        let state = AppState(client: ScriptedChatClient(events: []))
        let destination = ChatDestination(
            routeName: "chatgpt-sol",
            title: "chatgpt-sol",
            detail: "ChatGPT · GPT-5.6 Sol",
            providerName: "ChatGPT"
        )
        state.chatDestination = destination
        state.chatDraft = "Keep this turn"
        state.sendChatDraft()
        try await waitUntil { !state.isSendingMessage }
        XCTAssertEqual(state.chatMessages.last?.state, .failed)

        state.chatDestination = destination.withAvailability(false)
        let messages = state.chatMessages
        XCTAssertFalse(state.canRetryChat)
        state.retryLastChatTurn()

        XCTAssertEqual(state.chatMessages, messages)
        XCTAssertFalse(state.isSendingMessage)
    }

    @MainActor
    func testStopMarksPartialResponseAndAllowsRetry() async throws {
        let state = AppState(client: ScriptedChatClient(
            events: [.decision(Self.decision), .text("Partial")],
            delayNanoseconds: 5_000_000_000
        ))
        state.chatDraft = "Long answer"

        state.sendChatDraft()
        try await waitUntil { state.chatMessages.last?.text == "Partial" }
        state.stopChatResponse()
        try await waitUntil { !state.isSendingMessage }

        XCTAssertEqual(state.chatMessages.last?.state, .stopped)
        XCTAssertEqual(state.chatMessages.last?.text, "Partial")
        XCTAssertTrue(state.canRetryChat)
        XCTAssertEqual(AppState.chatRequestMessages(from: state.chatMessages), [
            ChatRequestMessage(role: "user", content: "Long answer")
        ])
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

    fileprivate static let decision = RoutingDecision(
        prompt: "Hello",
        route: .local,
        provider: "apple-local",
        score: 0.1,
        mode: "scored",
        explanation: "A short local turn.",
        features: []
    )
}

private extension RoutingStats {
    static var emptyForChatTests: RoutingStats {
        RoutingStats(
            localPercent: 0,
            cloudPercent: 0,
            totalTurns: 0,
            savedToday: 0,
            savedLast30Days: 0,
            cloudSpendToday: 0,
            percentVsAlwaysCloud: 0,
            updatedAt: Date(),
            isRunning: true
        )
    }
}

private struct ScriptedChatClient: WayfinderClient {
    let events: [ChatStreamEvent]
    var delayNanoseconds: UInt64 = 0

    func route(prompt: String) async throws -> RoutingDecision {
        ChatDeliveryTests.decision
    }

    func streamChat(messages: [ChatRequestMessage]) -> AsyncThrowingStream<ChatStreamEvent, Error> {
        AsyncThrowingStream { continuation in
            let task = Task {
                do {
                    for event in events {
                        continuation.yield(event)
                    }
                    if delayNanoseconds > 0 {
                        try await Task.sleep(nanoseconds: delayNanoseconds)
                    }
                    continuation.finish()
                } catch {
                    continuation.finish(throwing: error)
                }
            }
            continuation.onTermination = { _ in task.cancel() }
        }
    }

    func loadStats(range: StatsRange) async throws -> RoutingStats {
        RoutingStats(
            localPercent: 1,
            cloudPercent: 0,
            totalTurns: 1,
            savedToday: 0,
            savedLast30Days: 0,
            cloudSpendToday: 0,
            percentVsAlwaysCloud: 1,
            updatedAt: Date(),
            isRunning: true
        )
    }
}

private actor DestinationRecorder {
    private var destination: ChatDestination?

    func record(_ destination: ChatDestination) {
        self.destination = destination
    }

    func value() -> ChatDestination? {
        destination
    }
}

private struct DestinationRecordingClient: WayfinderClient {
    let recorder: DestinationRecorder

    func route(prompt: String) async throws -> RoutingDecision {
        ChatDeliveryTests.decision
    }

    func streamChat(messages: [ChatRequestMessage]) -> AsyncThrowingStream<ChatStreamEvent, Error> {
        streamChat(messages: messages, destination: .automatic)
    }

    func streamChat(
        messages: [ChatRequestMessage],
        destination: ChatDestination
    ) -> AsyncThrowingStream<ChatStreamEvent, Error> {
        AsyncThrowingStream { continuation in
            Task {
                await recorder.record(destination)
                continuation.yield(.decision(ChatDeliveryTests.decision))
                continuation.yield(.text("Hello"))
                continuation.yield(.completed)
                continuation.finish()
            }
        }
    }

    func loadStats(range: StatsRange) async throws -> RoutingStats {
        .emptyForChatTests
    }
}

private struct OverviewClient: WayfinderClient {
    let overview: GatewayOverview

    func route(prompt: String) async throws -> RoutingDecision {
        ChatDeliveryTests.decision
    }

    func loadStats(range: StatsRange) async throws -> RoutingStats {
        overview.routingStats
    }

    func loadOverview() async throws -> GatewayOverview {
        overview
    }
}

private final class ChatURLProtocolStub: URLProtocol {
    typealias Handler = (URLRequest) throws -> (HTTPURLResponse, Data)

    private static let lock = NSLock()
    private static var handler: Handler?

    static func install(_ handler: @escaping Handler) {
        lock.lock()
        self.handler = handler
        lock.unlock()
    }

    static func reset() {
        lock.lock()
        handler = nil
        lock.unlock()
    }

    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

    override func startLoading() {
        Self.lock.lock()
        let handler = Self.handler
        Self.lock.unlock()
        guard let handler else {
            client?.urlProtocol(self, didFailWithError: URLError(.badServerResponse))
            return
        }

        do {
            let (response, data) = try handler(request)
            client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
            client?.urlProtocol(self, didLoad: data)
            client?.urlProtocolDidFinishLoading(self)
        } catch {
            client?.urlProtocol(self, didFailWithError: error)
        }
    }

    override func stopLoading() {}
}
