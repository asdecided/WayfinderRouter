import Foundation

public protocol CodexAccountClient: Sendable {
    func account() async throws -> CodexAccountSnapshot
    func models() async throws -> CodexModelsResponse
    func beginLogin(flow: CodexLoginFlow) async throws -> CodexAccountSnapshot
    func cancelLogin(id: String) async throws -> CodexAccountSnapshot
    func logout() async throws -> CodexAccountSnapshot
}

public struct GatewayCodexAccountClient: CodexAccountClient {
    public typealias RuntimeValidation = @Sendable () async throws -> Void

    public static let maximumResponseBytes = 64 * 1_024

    private let baseURL: URL
    private let session: URLSession
    private let runtimeValidation: RuntimeValidation

    public init(
        baseURL: URL = URL(string: "http://127.0.0.1:8088")!,
        session: URLSession = .shared,
        runtimeValidation: RuntimeValidation? = nil
    ) {
        self.baseURL = baseURL
        self.session = session
        let runtime = VerifiedGatewayRuntime()
        self.runtimeValidation = runtimeValidation ?? { try await runtime.validate() }
    }

    public func account() async throws -> CodexAccountSnapshot {
        try await request(path: "router/codex/account", method: "GET")
    }

    public func models() async throws -> CodexModelsResponse {
        try await request(path: "router/codex/models", method: "GET")
    }

    public func beginLogin(flow: CodexLoginFlow) async throws -> CodexAccountSnapshot {
        try await request(
            path: "router/codex/login",
            method: "POST",
            body: LoginRequest(flow: flow)
        )
    }

    public func cancelLogin(id: String) async throws -> CodexAccountSnapshot {
        try await request(
            path: "router/codex/login/cancel",
            method: "POST",
            body: CancelLoginRequest(loginID: id)
        )
    }

    public func logout() async throws -> CodexAccountSnapshot {
        try await request(
            path: "router/codex/logout",
            method: "POST",
            body: EmptyRequest()
        )
    }

    private func request<Response: Decodable>(
        path: String,
        method: String
    ) async throws -> Response {
        try await request(path: path, method: method, body: Optional<EmptyRequest>.none)
    }

    private func request<Response: Decodable, Body: Encodable>(
        path: String,
        method: String,
        body: Body?
    ) async throws -> Response {
        guard Self.isLiteralLoopback(baseURL) else {
            throw CodexAccountClientError.nonLoopbackControlURL
        }
        try await runtimeValidation()

        var request = URLRequest(url: baseURL.appending(path: path))
        request.cachePolicy = .reloadIgnoringLocalCacheData
        request.httpMethod = method
        request.timeoutInterval = 15
        request.setValue("1", forHTTPHeaderField: "X-Wayfinder-Local-Control")
        if let body {
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
            request.httpBody = try JSONEncoder().encode(body)
        }

        let (bytes, response) = try await session.bytes(for: request)
        guard let http = response as? HTTPURLResponse else {
            throw CodexAccountClientError.invalidResponse
        }
        guard (200..<300).contains(http.statusCode) else {
            throw CodexAccountClientError.gatewayStatus(http.statusCode)
        }
        guard
            http.expectedContentLength < 0
                || http.expectedContentLength <= Self.maximumResponseBytes
        else {
            throw CodexAccountClientError.responseTooLarge
        }

        var data = Data()
        data.reserveCapacity(
            min(max(Int(http.expectedContentLength), 0), Self.maximumResponseBytes)
        )
        for try await byte in bytes {
            guard data.count < Self.maximumResponseBytes else {
                throw CodexAccountClientError.responseTooLarge
            }
            data.append(byte)
        }
        do {
            return try JSONDecoder().decode(Response.self, from: data)
        } catch {
            throw CodexAccountClientError.invalidResponse
        }
    }

    static func isLiteralLoopback(_ url: URL) -> Bool {
        guard
            let scheme = url.scheme?.lowercased(),
            scheme == "http" || scheme == "https",
            url.user == nil,
            url.password == nil,
            let host = url.host?.lowercased()
        else {
            return false
        }
        let unwrapped = host.trimmingCharacters(in: CharacterSet(charactersIn: "[]"))
        if unwrapped == "::1" { return true }

        let octets = unwrapped.split(separator: ".", omittingEmptySubsequences: false)
        guard octets.count == 4 else { return false }
        let numbers = octets.compactMap { octet -> Int? in
            guard !octet.isEmpty, octet.allSatisfy(\.isNumber), let value = Int(octet), value <= 255 else {
                return nil
            }
            return value
        }
        return numbers.count == 4 && numbers.first == 127
    }
}

private struct LoginRequest: Encodable {
    let flow: CodexLoginFlow
}

private struct CancelLoginRequest: Encodable {
    let loginID: String

    private enum CodingKeys: String, CodingKey {
        case loginID = "login_id"
    }
}

private struct EmptyRequest: Encodable {}

public enum CodexAccountClientError: LocalizedError, Equatable, Sendable {
    case nonLoopbackControlURL
    case gatewayStatus(Int)
    case responseTooLarge
    case invalidResponse

    public var errorDescription: String? {
        switch self {
        case .nonLoopbackControlURL:
            return "ChatGPT account controls are available only through the local gateway."
        case .gatewayStatus(let status):
            switch status {
            case 404:
                return "ChatGPT has not been added to this gateway yet."
            case 501:
                return "This gateway build does not include ChatGPT account support."
            case 409:
                return "The gateway could not change ChatGPT sign-in in its current state. Refresh and try again."
            case 503:
                return "The Codex runtime is unavailable. Check the gateway and try again."
            default:
                return "The gateway could not complete the ChatGPT account request (HTTP \(status))."
            }
        case .responseTooLarge, .invalidResponse:
            return "The gateway returned an invalid ChatGPT account response."
        }
    }
}
