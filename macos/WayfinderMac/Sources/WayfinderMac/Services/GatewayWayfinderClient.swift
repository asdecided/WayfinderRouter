import Foundation

public struct GatewayWayfinderClient: WayfinderClient {
    public typealias RuntimeValidation = @Sendable () async throws -> URL

    private let baseURL: URL
    private let session: URLSession
    private let serviceController: GatewayServiceController
    private let appleProductReadiness: @Sendable () -> Bool
    private let runtimeValidation: RuntimeValidation

    public init(
        baseURL: URL = URL(string: "http://127.0.0.1:8088")!,
        session: URLSession = .shared,
        serviceController: GatewayServiceController = GatewayServiceController(),
        runtimeValidation: RuntimeValidation? = nil,
        appleProductReadiness: @escaping @Sendable () -> Bool = {
            AppleFoundationModelsProductReadiness.current().isReady
        }
    ) {
        self.baseURL = baseURL
        self.session = session
        self.serviceController = serviceController
        let runtime = VerifiedGatewayRuntime(serviceController: serviceController)
        self.runtimeValidation = runtimeValidation ?? { try await runtime.validatedHelper() }
        self.appleProductReadiness = appleProductReadiness
    }

    public func route(prompt: String) async throws -> RoutingDecision {
        let trimmed = prompt.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else {
            throw WayfinderClientError.emptyPrompt
        }
        _ = try await runtimeValidation()

        var request = URLRequest(url: baseURL.appending(path: "v1/chat/completions"))
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.setValue("1", forHTTPHeaderField: "X-Wayfinder-Debug")
        request.httpBody = try JSONEncoder().encode(GatewayChatRequest(prompt: trimmed))

        let (bytes, response) = try await session.bytes(for: request)
        guard let http = response as? HTTPURLResponse else {
            throw WayfinderClientError.gatewayResponseMissingDecision
        }
        if !(200..<300).contains(http.statusCode) {
            let body = try await Self.boundedGatewayBody(
                from: bytes,
                response: http,
                maximumBytes: Self.maximumGatewayErrorBodyBytes
            )
            throw Self.gatewayError(for: http, body: body)
        }
        guard let data = try await Self.boundedGatewayBody(
            from: bytes,
            response: http,
            maximumBytes: Self.maximumBufferedGatewayResponseBytes
        ) else {
            throw WayfinderClientError.gatewayResponseTooLarge
        }

        let decoded = try JSONDecoder().decode(GatewayChatResponse.self, from: data)
        guard let wayfinder = decoded.wayfinder else {
            throw WayfinderClientError.gatewayResponseMissingDecision
        }
        return wayfinder.routingDecision(prompt: trimmed)
    }

    public func streamChat(messages: [ChatRequestMessage]) -> AsyncThrowingStream<ChatStreamEvent, Error> {
        streamChat(messages: messages, destination: .automatic)
    }

    public func streamChat(
        messages: [ChatRequestMessage],
        destination: ChatDestination
    ) -> AsyncThrowingStream<ChatStreamEvent, Error> {
        AsyncThrowingStream { continuation in
            let task = Task {
                do {
                    _ = try await runtimeValidation()
                    let bounded = try Self.boundedChatMessages(messages)
                    guard let prompt = bounded.last(where: { $0.role == "user" })?.content else {
                        throw WayfinderClientError.emptyPrompt
                    }

                    var request = URLRequest(url: baseURL.appending(path: "v1/chat/completions"))
                    request.httpMethod = "POST"
                    request.setValue("application/json", forHTTPHeaderField: "Content-Type")
                    request.setValue("1", forHTTPHeaderField: "X-Wayfinder-Debug")
                    request.httpBody = try JSONEncoder().encode(
                        GatewayStreamingChatRequest(
                            model: destination.gatewayModelValue,
                            messages: bounded
                        )
                    )

                    let (bytes, response) = try await session.bytes(for: request)
                    guard let http = response as? HTTPURLResponse else {
                        throw WayfinderClientError.invalidChatStream
                    }
                    guard (200..<300).contains(http.statusCode) else {
                        let body = try await Self.boundedGatewayBody(
                            from: bytes,
                            response: http,
                            maximumBytes: Self.maximumGatewayErrorBodyBytes
                        )
                        throw Self.gatewayError(
                            for: http,
                            body: body,
                            isExplicitChatGPTDestination: destination.isChatGPTAccount
                        )
                    }
                    guard http.value(forHTTPHeaderField: "Content-Type")?.contains("text/event-stream") == true else {
                        throw WayfinderClientError.invalidChatStream
                    }

                    var decoder = GatewayStreamDecoder(prompt: prompt)
                    for try await line in bytes.lines {
                        try Task.checkCancellation()
                        for event in try decoder.consume(line: line) {
                            continuation.yield(event)
                        }
                    }
                    guard decoder.sawDecision, decoder.sawCompletion else {
                        throw WayfinderClientError.invalidChatStream
                    }
                    continuation.finish()
                } catch {
                    continuation.finish(throwing: error)
                }
            }
            continuation.onTermination = { _ in task.cancel() }
        }
    }

    static func boundedChatMessages(_ messages: [ChatRequestMessage]) throws -> [ChatRequestMessage] {
        let maximumMessages = 20
        let maximumMessageCharacters = 32_768
        let maximumConversationCharacters = 65_536
        var bounded = Array(messages.suffix(maximumMessages))

        guard bounded.allSatisfy({ $0.content.count <= maximumMessageCharacters }) else {
            throw WayfinderClientError.conversationTooLarge
        }
        while bounded.reduce(0, { $0 + $1.content.count }) > maximumConversationCharacters,
              bounded.count > 1 {
            bounded.removeFirst()
        }
        while bounded.first?.role == "assistant" {
            bounded.removeFirst()
        }
        guard !bounded.isEmpty,
              bounded.reduce(0, { $0 + $1.content.count }) <= maximumConversationCharacters else {
            throw WayfinderClientError.conversationTooLarge
        }
        return bounded
    }

    static func gatewayError(
        for response: HTTPURLResponse,
        body: Data? = nil,
        isExplicitChatGPTDestination: Bool = false
    ) -> WayfinderClientError {
        if let body,
           responseHasJSONContentType(response),
           body.count <= maximumGatewayErrorBodyBytes,
           let envelope = try? JSONDecoder().decode(GatewayErrorEnvelope.self, from: body),
           let error = mappedGatewayHTTPChatError(
               status: response.statusCode,
               type: envelope.error.type,
               isExplicitChatGPTDestination: isExplicitChatGPTDestination
           ) {
            return error
        }
        return .gatewayStatus(
            response.statusCode,
            model: response.value(forHTTPHeaderField: "X-Wayfinder-Router-Model")
        )
    }

    static let maximumBufferedGatewayResponseBytes = 1_048_576
    static let maximumGatewayErrorBodyBytes = 16_384

    private static func boundedGatewayBody(
        from bytes: URLSession.AsyncBytes,
        response: HTTPURLResponse,
        maximumBytes: Int
    ) async throws -> Data? {
        guard response.expectedContentLength < 0
                || response.expectedContentLength <= maximumBytes else {
            return nil
        }
        var body = Data()
        body.reserveCapacity(min(max(Int(response.expectedContentLength), 0), maximumBytes))
        for try await byte in bytes {
            try Task.checkCancellation()
            guard body.count < maximumBytes else {
                return nil
            }
            body.append(byte)
        }
        return body
    }

    private static func responseHasJSONContentType(_ response: HTTPURLResponse) -> Bool {
        response.value(forHTTPHeaderField: "Content-Type")?
            .split(separator: ";", maxSplits: 1)
            .first?
            .trimmingCharacters(in: .whitespacesAndNewlines)
            .lowercased() == "application/json"
    }

    public func loadStats(range: StatsRange) async throws -> RoutingStats {
        try await loadOverview().routingStats
    }

    public func loadOverview() async throws -> GatewayOverview {
        let helper = try await runtimeValidation()
        let serviceStatus = await serviceController.status(expectedGateway: helper)
        async let modelsTask = optionalFetch(GatewayModelsResponse.self, path: "router/models")
        let codexClient = GatewayCodexAccountClient(
            baseURL: baseURL,
            session: session,
            runtimeValidation: {}
        )
        async let codexModelsTask: CodexModelsResponse? = try? codexClient.models()
        async let codexAccountTask: CodexAccountSnapshot? = try? codexClient.account()
        async let recentTask = optionalFetch(GatewayRecentResponse.self, path: "router/recent")
        async let savingsTodayTask = optionalFetch(GatewaySavingsResponse.self, path: "v1/savings", queryItems: [
            URLQueryItem(name: "period", value: "today")
        ])
        async let savingsThirtyDaysTask = optionalFetch(GatewaySavingsResponse.self, path: "v1/savings", queryItems: [
            URLQueryItem(name: "period", value: "30d")
        ])

        let models = await modelsTask
        let codexModelIDs = Set((await codexModelsTask)?.models.map(\.id) ?? [])
        let codexAccountConnected = (await codexAccountTask)?.isConnected
        let recent = await recentTask
        let savingsToday = await savingsTodayTask
        let savingsThirtyDays = await savingsThirtyDaysTask
        let updatedAt = Date()
        let gateway = Self.gatewayDisplayState(from: serviceStatus)
        let hosted = Self.hostedDisplayState(
            gateway: gateway,
            health: serviceStatus.health,
            models: models?.models ?? []
        )
        let stats = Self.routingStats(
            gateway: gateway,
            models: models?.models ?? [],
            recent: recent,
            savingsToday: savingsToday,
            savingsThirtyDays: savingsThirtyDays,
            updatedAt: updatedAt
        )

        return GatewayOverview(
            gateway: gateway,
            hosted: hosted,
            endpoints: Self.endpointDisplayStatuses(
                gateway: gateway,
                models: models?.models ?? [],
                appleFoundationModelsReady: appleProductReadiness(),
                codexRuntimeModelIDs: codexModelIDs,
                codexAccountConnected: codexAccountConnected
            ),
            routingStats: stats,
            updatedAt: updatedAt
        )
    }

    static func endpointDisplayStatuses(
        gateway: GatewayDisplayState,
        models: [GatewayModelInfo],
        appleFoundationModelsReady: Bool = true,
        codexRuntimeModelIDs: Set<String>? = nil,
        codexAccountConnected: Bool? = nil
    ) -> [EndpointDisplayStatus] {
        models.map { model in
            let codexModelAvailable = !model.isCodexAppServer
                || codexRuntimeModelIDs?.contains(model.model) == true
            let state: EndpointState
            if model.isAppleFoundationModels && !appleFoundationModelsReady {
                state = .unavailable
            } else if !codexModelAvailable {
                state = .unavailable
            } else { switch gateway {
            case .checking, .stopped, .unreachable, .notInstalled:
                state = .unavailable
            case .offline:
                state = model.isProvenLocal ? .ready : .disabled
            case .running, .degraded:
                if model.isCodexAppServer {
                    state = codexAccountConnected == true
                        ? .ready
                        : (codexAccountConnected == false ? .signIn : .unavailable)
                } else {
                    state = model.keyOK ? .ready : .checkKey
                }
            } }
            return EndpointDisplayStatus(
                name: model.name,
                providerName: model.providerDisplayName,
                modelName: model.model,
                state: state,
                isChatDestinationAvailable: codexModelAvailable
            )
        }
        .sorted { $0.name.localizedCaseInsensitiveCompare($1.name) == .orderedAscending }
    }

    static func gatewayDisplayState(from status: GatewayServiceStatus) -> GatewayDisplayState {
        guard let health = status.health else {
            guard status.installed else {
                return .notInstalled(detail: "Install the launch agent in Settings")
            }
            guard status.loaded else {
                return .stopped(detail: "Launch agent is not loaded")
            }
            return .unreachable(detail: status.launchConfiguration.healthURLString)
        }
        if health.offline {
            return .offline(detail: "Offline mode keeps delivery local")
        }
        if health.status == "degraded" || !health.missingKeys.isEmpty {
            let detail = health.missingKeys.isEmpty
                ? "Gateway is reporting degraded health"
                : "Missing \(health.missingKeys.joined(separator: ", "))"
            return .degraded(detail: detail)
        }
        return .running(detail: health.models.isEmpty ? "No models configured" : "\(health.models.count) configured model\(health.models.count == 1 ? "" : "s")")
    }

    static func hostedDisplayState(
        gateway: GatewayDisplayState,
        health: GatewayHealth?,
        models: [GatewayModelInfo]
    ) -> HostedDisplayState {
        switch gateway {
        case .checking:
            return .checking(detail: "Checking gateway status")
        case .offline:
            return .disabled(detail: "Gateway offline mode is on")
        case .stopped, .unreachable, .notInstalled:
            return .unavailable(detail: "Gateway is not reachable")
        case .running, .degraded:
            break
        }

        if health?.models.isEmpty == true || models.isEmpty {
            return .noModels(detail: "Add models in Settings")
        }

        let missingKeyNames = models
            .filter { !$0.keyOK }
            .map { $0.apiKeyEnv ?? $0.name }
        if !(health?.missingKeys ?? []).isEmpty || !missingKeyNames.isEmpty {
            let names = missingKeyNames.isEmpty ? (health?.missingKeys ?? []) : missingKeyNames
            return .checkKeys(detail: names.joined(separator: ", "))
        }

        return .ready(detail: "\(models.count) configured model\(models.count == 1 ? "" : "s")")
    }

    static func routingStats(
        gateway: GatewayDisplayState,
        models: [GatewayModelInfo],
        recent: GatewayRecentResponse?,
        savingsToday: GatewaySavingsResponse?,
        savingsThirtyDays: GatewaySavingsResponse?,
        updatedAt: Date
    ) -> RoutingStats {
        let cheapestModel = models.first?.name
        let totalTurns = recent?.total ?? 0
        let byModel = recent?.byModel ?? [:]
        let localCount = cheapestModel.map { byModel[$0, default: 0] } ?? 0
        let cloudCount = max(0, totalTurns - localCount)
        let localPercent = totalTurns > 0 ? Double(localCount) / Double(totalTurns) : 0
        let cloudPercent = totalTurns > 0 ? Double(cloudCount) / Double(totalTurns) : 0
        let hasSavings = savingsToday?.hasDisplayableSavings == true || savingsThirtyDays?.hasDisplayableSavings == true

        return RoutingStats(
            localPercent: localPercent,
            cloudPercent: cloudPercent,
            localRouteCount: totalTurns > 0 ? localCount : nil,
            cloudRouteCount: totalTurns > 0 ? cloudCount : nil,
            totalTurns: totalTurns,
            savedToday: savingsToday?.savedDecimal ?? 0,
            savedLast30Days: savingsThirtyDays?.savedDecimal ?? 0,
            cloudSpendToday: savingsToday?.baselineDecimal ?? 0,
            percentVsAlwaysCloud: savingsToday?.savedPercentFraction ?? 0,
            isPriced: savingsToday?.priced == true || savingsThirtyDays?.priced == true,
            hasSavings: hasSavings,
            savedTodayDisplay: savingsToday?.displayLine(period: "Today") ?? "Today: Not yet available",
            savedLast30DaysDisplay: savingsThirtyDays?.displayLine(period: "Last 30 days") ?? "Last 30 days: Not yet available",
            updatedAt: updatedAt,
            isRunning: gateway.isRunning
        )
    }

    private func optionalFetch<T: Decodable>(
        _ type: T.Type,
        path: String,
        queryItems: [URLQueryItem] = []
    ) async -> T? {
        do {
            return try await fetch(type, path: path, queryItems: queryItems)
        } catch {
            return nil
        }
    }

    private func fetch<T: Decodable>(
        _ type: T.Type,
        path: String,
        queryItems: [URLQueryItem] = []
    ) async throws -> T {
        var components = URLComponents(url: baseURL.appending(path: path), resolvingAgainstBaseURL: false)!
        components.queryItems = queryItems.isEmpty ? nil : queryItems
        let url = components.url!
        let (data, response) = try await session.data(from: url)
        if let http = response as? HTTPURLResponse, !(200..<300).contains(http.statusCode) {
            throw Self.gatewayError(for: http)
        }
        return try JSONDecoder().decode(T.self, from: data)
    }
}

private struct GatewayChatRequest: Encodable {
    let model = "auto"
    let messages: [GatewayMessage]
    let stream = false

    init(prompt: String) {
        self.messages = [GatewayMessage(role: "user", content: prompt)]
    }
}

private struct GatewayStreamingChatRequest: Encodable {
    let model: String
    let messages: [ChatRequestMessage]
    let stream = true
}

private struct GatewayMessage: Encodable {
    let role: String
    let content: String
}

private struct GatewayChatResponse: Decodable {
    let wayfinder: GatewayDecision?
}

private struct GatewayDecision: Decodable {
    let model: String?
    let score: Double
    let mode: String?
    let features: [String: Int]?
    let contributions: [GatewayContribution]?
    let tiers: [GatewayTier]?

    func routingDecision(prompt: String) -> RoutingDecision {
        let selectedModel = model ?? naturalModel
        let firstTierModel = tiers?.sorted { $0.minScore < $1.minScore }.first?.model
        let route: RouteTarget = selectedModel == firstTierModel ? .local : .cloud
        let routingFeatures = routedFeatures()

        return RoutingDecision(
            prompt: prompt,
            route: route,
            provider: selectedModel,
            score: score,
            mode: mode ?? "scored",
            explanation: explanation(features: routingFeatures, route: route),
            features: routingFeatures
        )
    }

    private var naturalModel: String {
        let sortedTiers = (tiers ?? []).sorted { $0.minScore < $1.minScore }
        return sortedTiers.reduce(sortedTiers.first?.model ?? "?") { chosen, tier in
            score >= tier.minScore ? tier.model : chosen
        }
    }

    private func routedFeatures() -> [RoutingFeature] {
        if let contributions, !contributions.isEmpty {
            return contributions
                .map {
                    RoutingFeature(
                        name: $0.name,
                        value: "\($0.value)",
                        contribution: $0.contribution
                    )
                }
                .sorted {
                    ($0.contribution ?? 0) == ($1.contribution ?? 0)
                        ? $0.name < $1.name
                        : ($0.contribution ?? 0) > ($1.contribution ?? 0)
                }
        }

        return (features ?? [:])
            .map { RoutingFeature(name: $0.key, value: "\($0.value)", contribution: nil) }
            .sorted { $0.name < $1.name }
    }

    private func explanation(features: [RoutingFeature], route: RouteTarget) -> String {
        let signals = features
            .prefix(3)
            .map(\.label)
            .joined(separator: ", ")

        if signals.isEmpty {
            return "The gateway returned a \(route.displayName.lowercased()) decision."
        }
        return "The gateway returned a \(route.displayName.lowercased()) decision from \(signals)."
    }
}

struct GatewayStreamDecoder {
    private let prompt: String
    private let maximumResponseCharacters: Int
    private var responseCharacters = 0
    private(set) var sawDecision = false
    private(set) var sawCompletion = false

    init(prompt: String, maximumResponseCharacters: Int = 1_048_576) {
        self.prompt = prompt
        self.maximumResponseCharacters = maximumResponseCharacters
    }

    mutating func consume(line: String) throws -> [ChatStreamEvent] {
        guard line.hasPrefix("data:") else {
            return []
        }
        guard !sawCompletion else {
            throw WayfinderClientError.invalidChatStream
        }
        let payload = line.dropFirst(5).trimmingCharacters(in: .whitespaces)
        if payload == "[DONE]" {
            sawCompletion = true
            return [.completed]
        }
        guard let data = payload.data(using: .utf8) else {
            throw WayfinderClientError.invalidChatStream
        }
        let envelope = try JSONDecoder().decode(GatewayStreamEnvelope.self, from: data)
        if let error = envelope.error {
            throw mappedGatewaySSEChatError(for: error.type) ?? WayfinderClientError.invalidChatStream
        }

        var events: [ChatStreamEvent] = []
        if let decision = envelope.wayfinder {
            guard !sawDecision else {
                throw WayfinderClientError.invalidChatStream
            }
            sawDecision = true
            events.append(.decision(decision.routingDecision(prompt: prompt)))
        }
        for choice in envelope.choices ?? [] {
            if let text = choice.delta.content, !text.isEmpty {
                guard sawDecision else {
                    throw WayfinderClientError.invalidChatStream
                }
                responseCharacters += text.count
                guard responseCharacters <= maximumResponseCharacters else {
                    throw WayfinderClientError.invalidChatStream
                }
                events.append(.text(text))
            }
        }
        return events
    }
}

private struct GatewayStreamEnvelope: Decodable {
    let wayfinder: GatewayDecision?
    let choices: [GatewayStreamChoice]?
    let error: GatewayStreamError?
}

private struct GatewayErrorEnvelope: Decodable {
    let error: GatewayStreamError
}

private struct GatewayStreamChoice: Decodable {
    let delta: GatewayStreamDelta
}

private struct GatewayStreamDelta: Decodable {
    let content: String?
}

private struct GatewayStreamError: Decodable {
    let type: String?
}

private func mappedGatewaySSEChatError(for type: String?) -> WayfinderClientError? {
    switch type {
    case "wayfinder_router_turn_failed":
        .chatTurnFailed
    case "wayfinder_router_interrupted":
        .chatTurnInterrupted
    case "wayfinder_router_busy":
        .chatProviderBusy
    case "wayfinder_router_usage_limited":
        .chatUsageLimitReached
    case "wayfinder_router_not_ready":
        .chatAccountNotReady
    default:
        nil
    }
}

private func mappedGatewayHTTPChatError(
    status: Int,
    type: String?,
    isExplicitChatGPTDestination: Bool
) -> WayfinderClientError? {
    switch (status, type) {
    case (409, "wayfinder_router_busy"):
        .chatProviderBusy
    case (409, "wayfinder_router_interrupted"):
        .chatTurnInterrupted
    case (429, "wayfinder_router_usage_limited"):
        .chatUsageLimitReached
    case (503, "wayfinder_router_not_ready") where isExplicitChatGPTDestination:
        .chatAccountNotReady
    default:
        nil
    }
}

private struct GatewayContribution: Decodable {
    let name: String
    let value: Int
    let contribution: Double
}

private struct GatewayTier: Decodable {
    let minScore: Double
    let model: String

    private enum CodingKeys: String, CodingKey {
        case minScore = "min_score"
        case model
    }
}

struct GatewayModelsResponse: Decodable, Equatable, Sendable {
    let models: [GatewayModelInfo]
    let dryRun: Bool

    private enum CodingKeys: String, CodingKey {
        case models
        case dryRun = "dry_run"
    }
}

struct GatewayModelInfo: Decodable, Equatable, Sendable {
    let name: String
    let endpoint: String
    let model: String
    let provider: String
    let tier: String?
    let apiKeyEnv: String?
    let keyOK: Bool

    init(
        name: String,
        endpoint: String,
        model: String,
        provider: String = "openai-compatible",
        tier: String? = nil,
        apiKeyEnv: String?,
        keyOK: Bool
    ) {
        self.name = name
        self.endpoint = endpoint
        self.model = model
        self.provider = provider
        self.tier = tier
        self.apiKeyEnv = apiKeyEnv
        self.keyOK = keyOK
    }

    private enum CodingKeys: String, CodingKey {
        case name
        case endpoint
        case model
        case provider
        case tier
        case apiKeyEnv = "api_key_env"
        case keyOK = "key_ok"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        name = try container.decode(String.self, forKey: .name)
        endpoint = try container.decode(String.self, forKey: .endpoint)
        model = try container.decode(String.self, forKey: .model)
        provider = try container.decodeIfPresent(String.self, forKey: .provider) ?? "openai-compatible"
        tier = try container.decodeIfPresent(String.self, forKey: .tier)
        apiKeyEnv = try container.decodeIfPresent(String.self, forKey: .apiKeyEnv)
        keyOK = try container.decode(Bool.self, forKey: .keyOK)
    }

    var isLocalEndpoint: Bool {
        guard let url = URL(string: endpoint), let host = url.host?.lowercased() else {
            return false
        }
        return host == "localhost" || host == "127.0.0.1" || host == "::1" || host == "[::1]"
    }

    var isProvenLocal: Bool {
        (isAppleFoundationModels && tier == "local") || isLocalEndpoint
    }

    var isAppleFoundationModels: Bool {
        provider == "apple-foundation-models"
    }

    var isCodexAppServer: Bool {
        provider == "codex-app-server"
    }

    var providerDisplayName: String {
        if provider == "apple-foundation-models" {
            return "Apple Foundation Models"
        }
        if provider == "codex-app-server" {
            return "ChatGPT"
        }
        let endpointHost = URL(string: endpoint)?.host?.lowercased() ?? ""
        let hint = [name, endpointHost, apiKeyEnv ?? ""]
            .joined(separator: " ")
            .lowercased()

        let knownProviders: [(tokens: [String], displayName: String)] = [
            (["anthropic"], "Anthropic"),
            (["openrouter"], "OpenRouter"),
            (["openai", "azure.com"], "OpenAI"),
            (["gemini", "googleapis", "generativelanguage", "vertex"], "Google Gemini"),
            (["mistral"], "Mistral"),
            (["groq"], "Groq"),
            (["cohere"], "Cohere"),
            (["perplexity"], "Perplexity"),
            (["together"], "Together AI"),
            (["xai", "grok"], "xAI"),
        ]
        if let provider = knownProviders.first(where: { provider in
            provider.tokens.contains(where: hint.contains)
        }) {
            return provider.displayName
        }

        if isLocalEndpoint {
            if hint.contains("ollama") || URL(string: endpoint)?.port == 11_434 {
                return "Ollama"
            }
            if hint.contains("lmstudio") || hint.contains("lm-studio")
                || URL(string: endpoint)?.port == 1_234 {
                return "LM Studio"
            }
            return "Local"
        }

        if !endpointHost.isEmpty {
            return endpointHost
                .replacingOccurrences(of: "api.", with: "", options: .anchored)
                .replacingOccurrences(of: "www.", with: "", options: .anchored)
        }
        return name
    }
}

struct GatewayRecentResponse: Decodable, Equatable, Sendable {
    let total: Int
    let byModel: [String: Int]
    let recent: [GatewayRecentRoute]

    private enum CodingKeys: String, CodingKey {
        case total
        case byModel = "by_model"
        case recent
    }
}

struct GatewayRecentRoute: Decodable, Equatable, Sendable {
    let requestID: String
    let model: String
    let score: Double
    let mode: String
    let timestamp: Double

    private enum CodingKeys: String, CodingKey {
        case requestID = "request_id"
        case model
        case score
        case mode
        case timestamp = "ts"
    }
}

struct GatewaySavingsResponse: Decodable, Equatable, Sendable {
    let saved: Double
    let savedPercent: Double
    let priced: Bool
    let requests: Int
    let baseline: Double?

    private enum CodingKeys: String, CodingKey {
        case saved
        case savedPercent = "saved_pct"
        case priced
        case requests
        case baseline
    }

    var savedDecimal: Decimal {
        Decimal(saved)
    }

    var baselineDecimal: Decimal {
        Decimal(baseline ?? 0)
    }

    var savedPercentFraction: Double {
        savedPercent / 100
    }

    var hasDisplayableSavings: Bool {
        priced && requests > 0 && saved > 0
    }

    func displayLine(period: String) -> String {
        guard hasDisplayableSavings else {
            return "\(period): Not yet available"
        }
        let savedText = saved < 0.01 ? "<$0.01" : savedDecimal.currencyText
        return "\(period): \(savedText) · \(savedPercentFraction.percentText) vs always-cloud"
    }
}
