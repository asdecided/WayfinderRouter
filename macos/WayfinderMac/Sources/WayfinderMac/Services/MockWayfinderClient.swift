import Foundation

public struct MockWayfinderClient: WayfinderClient {
    private let localClient = LocalWayfinderClient()

    public init() {}

    public func route(prompt: String) async throws -> RoutingDecision {
        try await analyse(prompt: prompt)
    }

    public func analyse(prompt: String) async throws -> RoutingDecision {
        try await Task.sleep(nanoseconds: 120_000_000)
        return try await localClient.analyse(prompt: prompt)
    }

    public func streamChat(messages: [ChatRequestMessage]) -> AsyncThrowingStream<ChatStreamEvent, Error> {
        AsyncThrowingStream { continuation in
            let task = Task {
                do {
                    guard let prompt = messages.last(where: { $0.role == "user" })?.content else {
                        throw WayfinderClientError.emptyPrompt
                    }
                    let decision = try await analyse(prompt: prompt)
                    continuation.yield(.decision(decision))
                    continuation.yield(.text("This preview reply was delivered through Wayfinder's streaming Chat contract."))
                    continuation.yield(.completed)
                    continuation.finish()
                } catch {
                    continuation.finish(throwing: error)
                }
            }
            continuation.onTermination = { _ in task.cancel() }
        }
    }

    public func loadStats(range: StatsRange) async throws -> RoutingStats {
        try await Task.sleep(nanoseconds: 80_000_000)
        let local: Double
        let cloud: Double
        switch range {
        case .today:
            local = 0.68
            cloud = 0.32
        case .sevenDays:
            local = 0.71
            cloud = 0.29
        case .thirtyDays:
            local = 0.74
            cloud = 0.26
        }

        return RoutingStats(
            localPercent: local,
            cloudPercent: cloud,
            savedToday: Decimal(string: "0.01") ?? 0,
            savedLast30Days: Decimal(string: "0.01") ?? 0,
            cloudSpendToday: Decimal(string: "0.00") ?? 0,
            percentVsAlwaysCloud: 0.29,
            updatedAt: Date(),
            isRunning: true
        )
    }
}
