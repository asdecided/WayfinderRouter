import Foundation
import XCTest
@testable import WayfinderMacCore

final class CodexAccountClientTests: XCTestCase {
    override func tearDown() {
        AccountURLProtocolStub.reset()
        super.tearDown()
    }

    func testEveryControlRequestUsesLocalHeaderAndExactJSONContract() async throws {
        AccountURLProtocolStub.install { request in
            let path = request.url?.path ?? ""
            let body: String
            switch path {
            case "/router/codex/models":
                body = #"{"models":[{"id":"gpt-5.6-sol"}]}"#
            case "/router/codex/login":
                body = #"{"status":"awaiting_device_code","login":{"id":"login-1","flow":"device_code","url":"https://auth.openai.com/device","user_code":"CODE"}}"#
            default:
                body = #"{"status":"signed_out"}"#
            }
            return Self.response(for: request, body: body)
        }
        let client = makeClient()

        _ = try await client.account()
        _ = try await client.models()
        _ = try await client.beginLogin(flow: .deviceCode)
        _ = try await client.cancelLogin(id: "login-1")
        _ = try await client.logout()

        let requests = AccountURLProtocolStub.requests
        XCTAssertEqual(
            requests.compactMap { $0.url?.path },
            [
                "/router/codex/account",
                "/router/codex/models",
                "/router/codex/login",
                "/router/codex/login/cancel",
                "/router/codex/logout",
            ]
        )
        XCTAssertTrue(requests.allSatisfy {
            $0.value(forHTTPHeaderField: "X-Wayfinder-Local-Control") == "1"
        })
        XCTAssertTrue(requests.allSatisfy {
            $0.cachePolicy == .reloadIgnoringLocalCacheData
        })
        XCTAssertEqual(requests.map(\.httpMethod), ["GET", "GET", "POST", "POST", "POST"])
        let requestBodies = AccountURLProtocolStub.requestBodies
        XCTAssertNil(requestBodies[0])
        XCTAssertNil(requestBodies[1])
        XCTAssertEqual(jsonObject(requestBodies[2]), ["flow": "device-code"])
        XCTAssertEqual(jsonObject(requestBodies[3]), ["login_id": "login-1"])
        XCTAssertEqual(jsonObject(requestBodies[4]), [:])
        XCTAssertTrue(requests.suffix(3).allSatisfy {
            $0.value(forHTTPHeaderField: "Content-Type") == "application/json"
        })
    }

    func testControlClientRejectsHostnamesThatOnlyLookLikeLoopback() async {
        for host in ["127.evil.com", "127.0.0.1.evil.com", "localhost", "example.com"] {
            let client = GatewayCodexAccountClient(baseURL: URL(string: "https://\(host):8088")!)
            do {
                _ = try await client.account()
                XCTFail("Expected \(host) to be rejected")
            } catch {
                XCTAssertEqual(error as? CodexAccountClientError, .nonLoopbackControlURL)
            }
        }
    }

    func testControlClientAcceptsNumericIPv4AndIPv6LoopbackOnly() {
        XCTAssertTrue(GatewayCodexAccountClient.isLiteralLoopback(URL(string: "http://127.0.0.1:8088")!))
        XCTAssertTrue(GatewayCodexAccountClient.isLiteralLoopback(URL(string: "https://127.255.10.4")!))
        XCTAssertTrue(GatewayCodexAccountClient.isLiteralLoopback(URL(string: "http://[::1]:8088")!))
        XCTAssertFalse(GatewayCodexAccountClient.isLiteralLoopback(URL(string: "ftp://127.0.0.1")!))
        XCTAssertFalse(GatewayCodexAccountClient.isLiteralLoopback(URL(string: "http://user@127.0.0.1")!))
        XCTAssertFalse(GatewayCodexAccountClient.isLiteralLoopback(URL(string: "http://127.0.0.256")!))
    }

    func testControlClientRejectsOversizedAndMalformedResponses() async {
        AccountURLProtocolStub.install { request in
            (
                HTTPURLResponse(
                    url: request.url!,
                    statusCode: 200,
                    httpVersion: nil,
                    headerFields: [
                        "Content-Type": "application/json",
                        "Content-Length": "\(GatewayCodexAccountClient.maximumResponseBytes + 1)",
                    ]
                )!,
                Data()
            )
        }
        let client = makeClient()
        do {
            _ = try await client.account()
            XCTFail("Expected declared oversized response rejection")
        } catch {
            XCTAssertEqual(error as? CodexAccountClientError, .responseTooLarge)
        }

        AccountURLProtocolStub.install { request in
            Self.response(
                for: request,
                data: Data(repeating: 0x20, count: GatewayCodexAccountClient.maximumResponseBytes + 1)
            )
        }
        do {
            _ = try await client.account()
            XCTFail("Expected oversized response rejection")
        } catch {
            XCTAssertEqual(error as? CodexAccountClientError, .responseTooLarge)
        }

        AccountURLProtocolStub.install { request in
            Self.response(for: request, body: #"{"status":"mystery"}"#)
        }
        do {
            _ = try await client.account()
            XCTFail("Expected malformed response rejection")
        } catch {
            XCTAssertEqual(error as? CodexAccountClientError, .invalidResponse)
        }
    }

    func testControlClientRejectsUnverifiedRuntimeBeforeAnyLoopbackRequest() async {
        AccountURLProtocolStub.install { _ in
            XCTFail("Runtime validation must happen before the control request")
            throw URLError(.badServerResponse)
        }
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [AccountURLProtocolStub.self]
        let client = GatewayCodexAccountClient(
            baseURL: URL(string: "http://127.0.0.1:8088")!,
            session: URLSession(configuration: configuration),
            runtimeValidation: { throw VerifiedGatewayRuntimeError.serviceNeedsRepair }
        )

        do {
            _ = try await client.account()
            XCTFail("Expected the unverified runtime to be rejected")
        } catch {
            XCTAssertEqual(error as? VerifiedGatewayRuntimeError, .serviceNeedsRepair)
        }
    }

    func testMissingAccountRouteExplainsTheOptInConfigurationBoundary() {
        XCTAssertEqual(
            CodexAccountClientError.gatewayStatus(404).localizedDescription,
            "ChatGPT has not been added to this gateway yet."
        )
        XCTAssertEqual(
            CodexAccountClientError.gatewayStatus(501).localizedDescription,
            "This gateway build does not include ChatGPT account support."
        )
    }

    private func makeClient() -> GatewayCodexAccountClient {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [AccountURLProtocolStub.self]
        return GatewayCodexAccountClient(
            baseURL: URL(string: "http://127.0.0.1:8088")!,
            session: URLSession(configuration: configuration),
            runtimeValidation: {}
        )
    }

    private func jsonObject(_ data: Data?) -> [String: String] {
        guard let data,
              let object = try? JSONSerialization.jsonObject(with: data) as? [String: String] else {
            return [:]
        }
        return object
    }

    private static func response(for request: URLRequest, body: String) -> (HTTPURLResponse, Data) {
        response(for: request, data: Data(body.utf8))
    }

    private static func response(for request: URLRequest, data: Data) -> (HTTPURLResponse, Data) {
        (
            HTTPURLResponse(
                url: request.url!,
                statusCode: 200,
                httpVersion: nil,
                headerFields: ["Content-Type": "application/json"]
            )!,
            data
        )
    }
}

private final class AccountURLProtocolStub: URLProtocol {
    typealias Handler = (URLRequest) throws -> (HTTPURLResponse, Data)

    private static let lock = NSLock()
    private static var handler: Handler?
    private static var capturedRequests: [URLRequest] = []
    private static var capturedRequestBodies: [Data?] = []

    static var requests: [URLRequest] {
        lock.withLock { capturedRequests }
    }

    static var requestBodies: [Data?] {
        lock.withLock { capturedRequestBodies }
    }

    static func install(_ handler: @escaping Handler) {
        lock.withLock {
            self.handler = handler
            capturedRequests = []
            capturedRequestBodies = []
        }
    }

    static func reset() {
        lock.withLock {
            handler = nil
            capturedRequests = []
            capturedRequestBodies = []
        }
    }

    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

    override func startLoading() {
        let handler: Handler? = Self.lock.withLock {
            Self.capturedRequests.append(request)
            Self.capturedRequestBodies.append(Self.bodyData(from: request))
            return Self.handler
        }
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

    private static func bodyData(from request: URLRequest) -> Data? {
        if let body = request.httpBody { return body }
        guard let stream = request.httpBodyStream else { return nil }

        stream.open()
        defer { stream.close() }
        var data = Data()
        let bufferSize = 1_024
        let buffer = UnsafeMutablePointer<UInt8>.allocate(capacity: bufferSize)
        defer { buffer.deallocate() }
        while stream.hasBytesAvailable {
            let count = stream.read(buffer, maxLength: bufferSize)
            guard count > 0 else { break }
            data.append(buffer, count: count)
        }
        return data
    }
}

private extension NSLock {
    func withLock<T>(_ body: () throws -> T) rethrows -> T {
        lock()
        defer { unlock() }
        return try body()
    }
}
