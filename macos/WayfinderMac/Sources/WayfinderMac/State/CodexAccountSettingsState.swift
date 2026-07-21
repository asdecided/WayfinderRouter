import Foundation

public enum CodexAccountViewState: Equatable, Sendable {
    case checking
    case setupRequired
    case signedOut
    case awaitingBrowser(CodexPendingLogin)
    case awaitingDeviceCode(CodexPendingLogin)
    case connected(profile: CodexAccountProfile, models: [CodexModel])
    case reauthenticationRequired(detail: String?)
    case unavailable(detail: String?)
    case failed(message: String)

    public var isAwaitingLogin: Bool {
        switch self {
        case .awaitingBrowser, .awaitingDeviceCode:
            return true
        default:
            return false
        }
    }

    public var pendingLogin: CodexPendingLogin? {
        switch self {
        case .awaitingBrowser(let login), .awaitingDeviceCode(let login):
            return login
        default:
            return nil
        }
    }
}

@MainActor
public final class CodexAccountSettingsState: ObservableObject {
    @Published public private(set) var state: CodexAccountViewState = .checking
    @Published public private(set) var isPerformingAction = false
    @Published public private(set) var modelCatalogError: String?

    private let client: any CodexAccountClient
    private let automaticallyPollLogin: Bool
    private let pollIntervalNanoseconds: UInt64
    private let maximumPollCount: Int
    private let onAccountStateChanged: @MainActor () -> Void
    private var pollingTask: Task<Void, Never>?

    public init(
        client: any CodexAccountClient = GatewayCodexAccountClient(),
        automaticallyPollLogin: Bool = true,
        pollIntervalNanoseconds: UInt64 = 1_000_000_000,
        maximumPollCount: Int = 300,
        onAccountStateChanged: @escaping @MainActor () -> Void = {}
    ) {
        self.client = client
        self.automaticallyPollLogin = automaticallyPollLogin
        self.pollIntervalNanoseconds = pollIntervalNanoseconds
        self.maximumPollCount = maximumPollCount
        self.onAccountStateChanged = onAccountStateChanged
    }

    deinit {
        pollingTask?.cancel()
    }

    public func refresh() async {
        pollingTask?.cancel()
        state = .checking
        do {
            let snapshot = try await client.account()
            await apply(snapshot, startPolling: true)
        } catch {
            applyFailure(error)
        }
    }

    @discardableResult
    public func beginLogin(flow: CodexLoginFlow) async -> URL? {
        guard !isPerformingAction else { return nil }
        pollingTask?.cancel()
        isPerformingAction = true
        defer { isPerformingAction = false }

        do {
            let snapshot = try await client.beginLogin(flow: flow)
            await apply(snapshot, startPolling: true)
            if case .awaitingBrowser(let login) = state {
                return login.url
            }
            return nil
        } catch {
            applyFailure(error)
            return nil
        }
    }

    public func cancelLogin() async {
        guard !isPerformingAction, let login = state.pendingLogin else { return }
        pollingTask?.cancel()
        isPerformingAction = true
        defer { isPerformingAction = false }

        do {
            let snapshot = try await client.cancelLogin(id: login.id)
            await apply(snapshot, startPolling: false)
        } catch {
            applyFailure(error)
        }
    }

    public func signOut() async {
        guard !isPerformingAction else { return }
        pollingTask?.cancel()
        isPerformingAction = true
        defer { isPerformingAction = false }

        do {
            let snapshot = try await client.logout()
            await apply(snapshot, startPolling: false)
        } catch {
            applyFailure(error)
        }
    }

    private func apply(_ snapshot: CodexAccountSnapshot, startPolling: Bool) async {
        modelCatalogError = nil
        switch snapshot {
        case .signedOut:
            state = .signedOut
        case .awaitingBrowser(let login):
            state = .awaitingBrowser(login)
            if startPolling { startLoginPolling() }
        case .awaitingDeviceCode(let login):
            state = .awaitingDeviceCode(login)
            if startPolling { startLoginPolling() }
        case .connected(let profile):
            do {
                let response = try await client.models()
                state = .connected(profile: profile, models: response.models)
            } catch {
                modelCatalogError = "Connected, but the model catalog could not be loaded."
                state = .connected(profile: profile, models: [])
            }
        case .reauthenticationRequired(let detail):
            state = .reauthenticationRequired(detail: detail)
        case .unavailable(let detail):
            state = .unavailable(detail: detail)
        }
        onAccountStateChanged()
    }

    private func startLoginPolling() {
        guard automaticallyPollLogin else { return }
        pollingTask?.cancel()
        pollingTask = Task { [weak self] in
            guard let self else { return }
            for _ in 0..<self.maximumPollCount {
                do {
                    try await Task.sleep(nanoseconds: self.pollIntervalNanoseconds)
                    try Task.checkCancellation()
                    let snapshot = try await self.client.account()
                    await self.apply(snapshot, startPolling: false)
                    if !self.state.isAwaitingLogin { return }
                } catch is CancellationError {
                    return
                } catch {
                    self.applyFailure(error)
                    return
                }
            }
            if self.state.isAwaitingLogin {
                self.state = .failed(message: "ChatGPT sign-in timed out. Start sign-in again.")
            }
        }
    }

    private static func message(for error: Error) -> String {
        (error as? LocalizedError)?.errorDescription
            ?? "The ChatGPT account request could not be completed."
    }

    private func applyFailure(_ error: Error) {
        if let clientError = error as? CodexAccountClientError,
           clientError == .gatewayStatus(404) {
            state = .setupRequired
        } else {
            state = .failed(message: Self.message(for: error))
        }
    }
}
