import Foundation

public struct LocalWayfinderClient: WayfinderClient {
    private let scorer = LocalPromptScorer()

    public init() {}

    public func route(prompt: String) async throws -> RoutingDecision {
        try await analyse(prompt: prompt)
    }

    public func analyse(prompt: String) async throws -> RoutingDecision {
        try Task.checkCancellation()
        return try scorer.analyse(prompt: prompt)
    }

    public func loadStats(range: StatsRange) async throws -> RoutingStats {
        RoutingStats(
            localPercent: 0.68,
            cloudPercent: 0.32,
            savedToday: Decimal(string: "0.01") ?? 0,
            savedLast30Days: Decimal(string: "0.01") ?? 0,
            cloudSpendToday: Decimal(string: "0.00") ?? 0,
            percentVsAlwaysCloud: 0.29,
            updatedAt: Date(),
            isRunning: true
        )
    }
}

public struct LocalPromptScorer: Sendable {
    private let featureOrder = [
        "word_count",
        "heading_count",
        "max_heading_depth",
        "list_item_count",
        "link_count",
        "code_block_count",
        "table_row_count",
        "reasoning_term_count",
        "math_symbol_count",
        "constraint_term_count",
        "question_count",
    ]

    private let defaultWeights: [String: Double] = [
        "word_count": 3.0,
        "list_item_count": 2.0,
        "heading_count": 1.5,
        "code_block_count": 1.5,
        "table_row_count": 1.0,
        "link_count": 1.0,
        "max_heading_depth": 1.0,
        "reasoning_term_count": 0.0,
        "math_symbol_count": 0.0,
        "constraint_term_count": 0.0,
        "question_count": 0.0,
    ]

    private let saturation: [String: Double] = [
        "word_count": 400.0,
        "heading_count": 8.0,
        "max_heading_depth": 4.0,
        "list_item_count": 15.0,
        "link_count": 10.0,
        "code_block_count": 4.0,
        "table_row_count": 12.0,
        "reasoning_term_count": 2.0,
        "math_symbol_count": 6.0,
        "constraint_term_count": 3.0,
        "question_count": 3.0,
    ]

    private let reasoningTerms: Set<String> = [
        "prove", "proof", "proofs", "proven", "derive", "derives", "derivation",
        "theorem", "theorems", "lemma", "lemmas", "corollary", "axiom", "axioms",
        "irrational", "undecidable", "undecidability", "decidable", "infinitely",
        "asymptotic", "complexity", "invariant", "invariants", "concurrency",
        "concurrent", "deadlock", "induction", "contradiction", "optimal",
        "optimality", "optimize", "optimise", "minimise", "minimize", "maximise",
        "maximize", "recurrence", "halting", "eigenvalue", "eigenvalues", "integral",
        "derivative", "polynomial", "prime", "primes", "modulo", "isomorphism",
        "monotonic", "bijection", "injective", "surjective", "combinatorial",
    ]

    private let constraintTerms: Set<String> = [
        "must", "without", "only", "ensure", "exactly", "guarantee", "constraint",
        "constraints", "subject", "preserving", "preserve",
    ]

    private let mathSymbolScalars: Set<Unicode.Scalar> = [
        "\u{2211}", "\u{222B}", "\u{221A}", "\u{2264}", "\u{2265}", "\u{2260}",
        "\u{2248}", "\u{221E}", "\u{2202}", "\u{2208}", "\u{2209}", "\u{2200}",
        "\u{2203}", "\u{2286}", "\u{2282}", "\u{222A}", "\u{2229}", "\u{2207}",
        "\u{00B1}", "\u{00D7}", "\u{00F7}", "\u{03C0}", "\u{03B8}", "\u{03BB}",
        "\u{03BC}", "\u{03C3}", "\u{03A3}", "\u{03A0}",
    ]

    public init() {}

    public func analyse(
        prompt: String,
        threshold: Double = 0.5,
        tiers: [RoutingTierRow] = RoutingSettingsState().tiers,
        weights: [RoutingWeightRow]? = nil
    ) throws -> RoutingDecision {
        let trimmed = prompt.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else {
            throw WayfinderClientError.emptyPrompt
        }

        let features = extractFeatures(from: trimmed)
        let activeWeights = weightsByID(from: weights)
        let score = scalarScore(features: features, weights: activeWeights)
        let provider = model(for: score, threshold: threshold, tiers: tiers)
        let route: RouteTarget = provider == "local" ? .local : .cloud
        let unsortedFeatures: [RoutingFeature] = featureOrder.map { name in
            let value = features[name, default: 0]
            let share = contribution(for: name, features: features, weights: activeWeights)
            return RoutingFeature(name: name, value: "\(value)", contribution: share)
        }
        let routingFeatures = unsortedFeatures.sorted { lhs, rhs in
            let lhsContribution = lhs.contribution ?? 0
            let rhsContribution = rhs.contribution ?? 0
            if lhsContribution == rhsContribution {
                return lhs.name < rhs.name
            }
            return lhsContribution > rhsContribution
        }

        return RoutingDecision(
            prompt: trimmed,
            route: route,
            provider: provider,
            score: score,
            mode: tiers.count > 2 ? "tiered" : "binary",
            explanation: explanation(for: features, score: score, route: route, threshold: threshold, weights: activeWeights),
            features: routingFeatures
        )
    }

    public func extractFeatures(from text: String) -> [String: Int] {
        let body = stripFrontmatter(from: text)
        var features = Dictionary(uniqueKeysWithValues: featureOrder.map { ($0, 0) })
        features["word_count"] = body.split { $0.isWhitespace }.count

        var inFence = false
        for line in body.components(separatedBy: .newlines) {
            let trimmedLeading = line.trimmingCharacters(in: .whitespaces)
            if trimmedLeading.hasPrefix("```") || trimmedLeading.hasPrefix("~~~") {
                if !inFence {
                    features["code_block_count", default: 0] += 1
                }
                inFence.toggle()
                continue
            }

            if inFence {
                continue
            }

            if let depth = headingDepth(in: line) {
                features["heading_count", default: 0] += 1
                features["max_heading_depth"] = max(features["max_heading_depth", default: 0], depth)
            } else if matches(line, pattern: #"^\s*(?:[-*+]|\d+[.)])\s+\S"#) {
                features["list_item_count", default: 0] += 1
            } else if matches(line, pattern: #"^\s*\|.*\|\s*$"#) {
                features["table_row_count", default: 0] += 1
            }

            features["link_count", default: 0] += countMatches(
                in: line,
                pattern: #"\[[^\]]+\]\([^)]+\)"#
            )
        }

        let tokens = matches(in: body.lowercased(), pattern: #"[a-zA-Z][a-zA-Z'\-]*"#)
        features["reasoning_term_count"] = tokens.filter { reasoningTerms.contains($0) }.count
        features["constraint_term_count"] = tokens.filter { constraintTerms.contains($0) }.count
        features["math_symbol_count"] = mathSymbolCount(in: body)
        features["question_count"] = body.filter { $0 == "?" }.count

        return features
    }

    public func scalarScore(features: [String: Int], weights: [String: Double]? = nil) -> Double {
        let activeWeights = weights ?? defaultWeights
        let totalWeight = activeWeights.values.reduce(0, +)
        guard totalWeight > 0 else {
            return 0
        }

        let accumulated = featureOrder.reduce(0.0) { partial, name in
            let rawValue = Double(features[name, default: 0])
            let normalized = min(rawValue / saturation[name, default: 1], 1)
            return partial + activeWeights[name, default: 0] * normalized
        }

        return ((accumulated / totalWeight) * 100).rounded(.toNearestOrEven) / 100
    }

    private func stripFrontmatter(from text: String) -> String {
        let lines = text.components(separatedBy: "\n")
        guard lines.first?.trimmingCharacters(in: .whitespacesAndNewlines) == "---" else {
            return text
        }

        for index in lines.indices.dropFirst() {
            let line = lines[index].trimmingCharacters(in: .whitespacesAndNewlines)
            if line == "---" || line == "..." {
                return lines.dropFirst(index + 1).joined(separator: "\n")
            }
        }
        return text
    }

    private func contribution(for name: String, features: [String: Int], weights: [String: Double]) -> Double {
        let totalWeight = weights.values.reduce(0, +)
        guard totalWeight > 0 else {
            return 0
        }

        let rawValue = Double(features[name, default: 0])
        let normalized = min(rawValue / saturation[name, default: 1], 1)
        let value = weights[name, default: 0] * normalized / totalWeight
        return (value * 10_000).rounded(.toNearestOrEven) / 10_000
    }

    private func headingDepth(in line: String) -> Int? {
        let pattern = #"^(#{1,6})\s+\S"#
        guard let match = firstMatch(in: line, pattern: pattern), match.numberOfRanges > 1 else {
            return nil
        }
        let range = match.range(at: 1)
        return range.location == NSNotFound ? nil : range.length
    }

    private func explanation(
        for features: [String: Int],
        score: Double,
        route: RouteTarget,
        threshold: Double,
        weights: [String: Double]
    ) -> String {
        let strongest = featureOrder
            .map { name in (name, contribution(for: name, features: features, weights: weights)) }
            .filter { $0.1 > 0 }
            .sorted { $0.1 > $1.1 }
            .prefix(3)
            .map { $0.0.replacingOccurrences(of: "_", with: " ") }

        if strongest.isEmpty {
            return "Very little structure was detected, so this stays on the local route."
        }

        let signals = strongest.joined(separator: ", ")
        switch route {
        case .local:
            return "Score \(formattedScore(score)) stays below threshold \(formattedScore(threshold)); strongest signals: \(signals)."
        case .cloud:
            return "Score \(formattedScore(score)) reaches threshold \(formattedScore(threshold)); strongest signals: \(signals)."
        }
    }

    private func weightsByID(from rows: [RoutingWeightRow]?) -> [String: Double] {
        guard let rows else {
            return defaultWeights
        }
        var weights = defaultWeights
        for row in rows {
            weights[row.id] = max(0, row.value)
        }
        return weights
    }

    private func model(for score: Double, threshold: Double, tiers: [RoutingTierRow]) -> String {
        if tiers.count <= 2 {
            return score >= threshold ? "cloud" : "local"
        }

        return tiers
            .sorted { $0.minScore < $1.minScore }
            .last { score >= $0.minScore }?
            .model ?? tiers.first?.model ?? "local"
    }

    private func formattedScore(_ score: Double) -> String {
        score.formatted(.number.precision(.fractionLength(2)))
    }

    private func mathSymbolCount(in text: String) -> Int {
        let scalarCount = text.unicodeScalars.filter { mathSymbolScalars.contains($0) }.count
        let latexCount = countMatches(in: text, pattern: #"\\[a-zA-Z]+"#)
        return scalarCount + latexCount
    }

    private func matches(_ text: String, pattern: String) -> Bool {
        firstMatch(in: text, pattern: pattern) != nil
    }

    private func countMatches(in text: String, pattern: String) -> Int {
        matches(in: text, pattern: pattern).count
    }

    private func matches(in text: String, pattern: String) -> [String] {
        guard let regex = try? NSRegularExpression(pattern: pattern) else {
            return []
        }
        let range = NSRange(text.startIndex..<text.endIndex, in: text)
        return regex.matches(in: text, range: range).compactMap { match in
            guard let range = Range(match.range, in: text) else {
                return nil
            }
            return String(text[range])
        }
    }

    private func firstMatch(in text: String, pattern: String) -> NSTextCheckingResult? {
        guard let regex = try? NSRegularExpression(pattern: pattern) else {
            return nil
        }
        let range = NSRange(text.startIndex..<text.endIndex, in: text)
        return regex.firstMatch(in: text, range: range)
    }
}
