import Foundation

public enum StatsRange: String, CaseIterable, Identifiable, Codable, Sendable {
    case today = "Today"
    case sevenDays = "7d"
    case thirtyDays = "30d"

    public var id: String { rawValue }
}

public struct RoutingStats: Codable, Equatable, Sendable {
    public let localPercent: Double
    public let cloudPercent: Double
    public let localRouteCount: Int?
    public let cloudRouteCount: Int?
    public let totalTurns: Int?
    public let savedToday: Decimal
    public let savedLast30Days: Decimal
    public let cloudSpendToday: Decimal
    public let percentVsAlwaysCloud: Double
    public let isPriced: Bool
    public let hasSavings: Bool
    public let savedTodayDisplay: String
    public let savedLast30DaysDisplay: String
    public let updatedAt: Date
    public let isRunning: Bool

    public init(
        localPercent: Double,
        cloudPercent: Double,
        localRouteCount: Int? = nil,
        cloudRouteCount: Int? = nil,
        totalTurns: Int? = nil,
        savedToday: Decimal,
        savedLast30Days: Decimal,
        cloudSpendToday: Decimal,
        percentVsAlwaysCloud: Double,
        isPriced: Bool = true,
        hasSavings: Bool = true,
        savedTodayDisplay: String? = nil,
        savedLast30DaysDisplay: String? = nil,
        updatedAt: Date,
        isRunning: Bool
    ) {
        self.localPercent = localPercent
        self.cloudPercent = cloudPercent
        self.localRouteCount = localRouteCount
        self.cloudRouteCount = cloudRouteCount
        self.totalTurns = totalTurns
        self.savedToday = savedToday
        self.savedLast30Days = savedLast30Days
        self.cloudSpendToday = cloudSpendToday
        self.percentVsAlwaysCloud = percentVsAlwaysCloud
        self.isPriced = isPriced
        self.hasSavings = hasSavings
        self.savedTodayDisplay = savedTodayDisplay ?? savedToday.currencyText
        self.savedLast30DaysDisplay = savedLast30DaysDisplay ?? savedLast30Days.currencyText
        self.updatedAt = updatedAt
        self.isRunning = isRunning
    }
}

public struct GatewayOverview: Equatable, Sendable {
    public let gateway: GatewayDisplayState
    public let hosted: HostedDisplayState
    public let endpoints: [EndpointDisplayStatus]
    public let routingStats: RoutingStats
    public let updatedAt: Date

    public init(
        gateway: GatewayDisplayState,
        hosted: HostedDisplayState,
        endpoints: [EndpointDisplayStatus] = [],
        routingStats: RoutingStats,
        updatedAt: Date
    ) {
        self.gateway = gateway
        self.hosted = hosted
        self.endpoints = endpoints
        self.routingStats = routingStats
        self.updatedAt = updatedAt
    }
}

public struct EndpointDisplayStatus: Equatable, Identifiable, Sendable {
    public let name: String
    public let providerName: String
    public let modelName: String?
    public let state: EndpointState
    public let isChatDestinationAvailable: Bool

    public var id: String { name }

    public init(
        name: String,
        providerName: String? = nil,
        modelName: String? = nil,
        state: EndpointState,
        isChatDestinationAvailable: Bool = true
    ) {
        self.name = name
        self.providerName = providerName ?? name
        self.modelName = modelName
        self.state = state
        self.isChatDestinationAvailable = isChatDestinationAvailable
    }

    public var detailText: String? {
        let alias = name.caseInsensitiveCompare(providerName) == .orderedSame
            ? nil
            : "route: \(name)"
        return [modelName, alias]
            .compactMap { $0 }
            .joined(separator: " · ")
            .nilIfEmpty
    }
}

private extension String {
    var nilIfEmpty: String? { isEmpty ? nil : self }
}

public enum EndpointState: Equatable, Sendable {
    case ready
    case signIn
    case checkKey
    case disabled
    case unavailable

    public var title: String {
        switch self {
        case .ready:
            return "Ready"
        case .signIn:
            return "Sign In"
        case .checkKey:
            return "Check Key"
        case .disabled:
            return "Disabled"
        case .unavailable:
            return "Unavailable"
        }
    }
}

public enum GatewayDisplayState: Equatable, Sendable {
    case checking(detail: String)
    case running(detail: String)
    case degraded(detail: String)
    case offline(detail: String)
    case stopped(detail: String)
    case unreachable(detail: String)
    case notInstalled(detail: String)

    public var title: String {
        switch self {
        case .checking:
            return "Checking"
        case .running:
            return "Running"
        case .degraded:
            return "Degraded"
        case .offline:
            return "Offline"
        case .stopped:
            return "Stopped"
        case .unreachable:
            return "Unreachable"
        case .notInstalled:
            return "Not Installed"
        }
    }

    public var detail: String {
        switch self {
        case .checking(let detail),
             .running(let detail),
             .degraded(let detail),
             .offline(let detail),
             .stopped(let detail),
             .unreachable(let detail),
             .notInstalled(let detail):
            return detail
        }
    }

    public var isRunning: Bool {
        switch self {
        case .running, .degraded, .offline:
            return true
        case .checking, .stopped, .unreachable, .notInstalled:
            return false
        }
    }
}

public enum HostedDisplayState: Equatable, Sendable {
    case checking(detail: String)
    case ready(detail: String)
    case checkKeys(detail: String)
    case disabled(detail: String)
    case noModels(detail: String)
    case unavailable(detail: String)

    public var title: String {
        switch self {
        case .checking:
            return "Checking"
        case .ready:
            return "Ready"
        case .checkKeys:
            return "Check Keys"
        case .disabled:
            return "Disabled"
        case .noModels:
            return "No Models"
        case .unavailable:
            return "Unavailable"
        }
    }

    public var detail: String {
        switch self {
        case .checking(let detail),
             .ready(let detail),
             .checkKeys(let detail),
             .disabled(let detail),
             .noModels(let detail),
             .unavailable(let detail):
            return detail
        }
    }
}
