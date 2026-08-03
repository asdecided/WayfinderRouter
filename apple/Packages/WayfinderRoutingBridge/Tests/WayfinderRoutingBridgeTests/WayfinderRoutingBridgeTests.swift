import XCTest
@testable import WayfinderRoutingBridge

final class WayfinderRoutingBridgeTests: XCTestCase {
    private func makeEngine() throws -> RoutingEngine {
        try RoutingEngine(
            configuration: RoutingConfiguration(
                tiers: [
                    RoutingTier(minScore: 0.0, model: "local"),
                    RoutingTier(minScore: 0.5, model: "cloud"),
                ]
            )
        )
    }

    private func makeDestination(
        id: String,
        boundary: ExecutionBoundary
    ) -> DestinationSnapshot {
        DestinationSnapshot(
            id: id,
            providerId: "provider",
            modelId: "model",
            displayName: "Model",
            routeTier: "local",
            executionBoundary: boundary,
            readiness: .ready,
            billingClass: .onDevice,
            contextWindow: 4_096,
            capabilities: DestinationCapabilities(
                text: true,
                streaming: true,
                imageInput: false,
                tools: false,
                embeddings: false,
                imageGeneration: false,
                audioInput: false,
                audioOutput: false,
                batch: false
            ),
            automaticEligible: true
        )
    }

    func testGeneratedBridgeScoresGoldenPrompts() throws {
        let engine = try makeEngine()

        XCTAssertEqual(bridgeAbiVersion(), WayfinderRoutingBridgeInfo.abiVersion)
        XCTAssertEqual(engine.runtimeContractVersion(), 1)

        let simple = try engine.score(prompt: "hi")
        XCTAssertEqual(simple.schemaVersion, "3")
        XCTAssertEqual(simple.score, 0.0)
        XCTAssertEqual(simple.recommendation, "local")
        XCTAssertEqual(simple.features.wordCount, 1)

        let structured = try engine.score(
            prompt: "# Plan\n\n## Steps\n- one\n- two\n- three\n1. first\n2. second"
        )
        XCTAssertEqual(structured.score, 0.15)
        XCTAssertEqual(structured.recommendation, "local")
        XCTAssertEqual(structured.features.headingCount, 2)
        XCTAssertEqual(structured.features.listItemCount, 5)
    }

    func testGeneratedBridgeFiltersPrivacyBeforeSelection() throws {
        let plan = try makeEngine().plan(
            request: RoutingRequest(
                schemaVersion: 1,
                requestId: "request-1",
                prompt: "hello",
                privacyPosture: .onDeviceOnly,
                requirements: RoutingRequirements(
                    contextTokens: 1_024,
                    imageInput: false,
                    tools: false,
                    streaming: true,
                    surface: .text
                )
            ),
            candidates: [
                makeDestination(id: "hosted", boundary: .hosted),
                makeDestination(id: "device", boundary: .onDevice),
            ]
        )

        XCTAssertEqual(plan.selectedDestinationId, "device")
        XCTAssertEqual(
            plan.candidates.first?.exclusions,
            [.privacyBoundaryDenied]
        )
    }

    func testGeneratedObjectLifetimeAndConcurrency() async throws {
        for _ in 0..<100 {
            let result = try makeEngine().score(prompt: "hello")
            XCTAssertEqual(result.recommendation, "local")
        }

        let engine = try makeEngine()
        try await withThrowingTaskGroup(of: String.self) { group in
            for _ in 0..<32 {
                group.addTask {
                    try engine.score(prompt: "hello").recommendation
                }
            }
            for try await recommendation in group {
                XCTAssertEqual(recommendation, "local")
            }
        }
    }

    func testOversizedErrorsDoNotEchoPromptContent() throws {
        let marker = "private-prompt-marker"
        let prompt = marker + String(repeating: "x", count: 256 * 1_024)

        XCTAssertThrowsError(try makeEngine().score(prompt: prompt)) { error in
            let rendered = String(describing: error)
            XCTAssertFalse(rendered.contains(marker))
            XCTAssertTrue(rendered.contains("bridge limit"))
        }
    }

    func testGeneratedBridgeRejectsRuntimeVersionSkew() throws {
        let requestID = "private-request-id"

        XCTAssertThrowsError(
            try makeEngine().plan(
                request: RoutingRequest(
                    schemaVersion: 999,
                    requestId: requestID,
                    prompt: "hello",
                    privacyPosture: .hostedAllowed,
                    requirements: RoutingRequirements(
                        contextTokens: nil,
                        imageInput: false,
                        tools: false,
                        streaming: false,
                        surface: .text
                    )
                ),
                candidates: []
            )
        ) { error in
            let rendered = String(describing: error)
            XCTAssertTrue(rendered.contains("unsupported runtime-contract version"))
            XCTAssertFalse(rendered.contains(requestID))
        }
    }
}
