import CryptoKit
import Foundation
import XCTest

@testable import WayfinderIOS

final class AuthorizationCodePKCETests: XCTestCase {
  private let credentialID = CredentialID(rawValue: "test.oauth-token")

  func testBeginAuthorizationBuildsS256ChallengeWithoutExposingVerifier() async throws {
    let verifier = String(repeating: "v", count: 43)
    let controller = makeController(
      nonceGenerator: StubOAuthNonceGenerator(
        stateValue: "expected-state",
        verifierValue: verifier
      )
    )

    let challenge = try await controller.beginAuthorization()
    let components = try XCTUnwrap(
      URLComponents(
        url: challenge.authorizationURL,
        resolvingAgainstBaseURL: false
      )
    )
    let query = Dictionary(
      uniqueKeysWithValues: (components.queryItems ?? []).compactMap {
        item in item.value.map { (item.name, $0) }
      }
    )
    let expectedChallenge = Data(
      SHA256.hash(data: Data(verifier.utf8))
    )
    .base64EncodedString()
    .replacingOccurrences(of: "+", with: "-")
    .replacingOccurrences(of: "/", with: "_")
    .replacingOccurrences(of: "=", with: "")

    XCTAssertEqual(challenge.providerID, "test-account")
    XCTAssertEqual(challenge.callback, .customScheme("wayfinder"))
    XCTAssertEqual(query["client_id"], "test-client")
    XCTAssertEqual(query["redirect_uri"], "wayfinder://oauth/test")
    XCTAssertEqual(query["scope"], "models.read chat.write")
    XCTAssertEqual(query["state"], "expected-state")
    XCTAssertEqual(query["code_challenge"], expectedChallenge)
    XCTAssertEqual(query["code_challenge_method"], "S256")
    XCTAssertFalse(String(reflecting: challenge).contains(verifier))
  }

  func testCompleteAuthorizationValidatesCallbackAndStoresCredential() async throws {
    let store = InMemoryCredentialStore()
    let client = OAuthStubHTTPClient(
      response: HTTPDataResponse(
        statusCode: 200,
        body: Data(#"{"access_token":"provider-secret"}"#.utf8)
      )
    )
    let controller = makeController(
      store: store,
      client: client,
      nonceGenerator: StubOAuthNonceGenerator(
        stateValue: "expected-state",
        verifierValue: String(repeating: "x", count: 43)
      )
    )
    let challenge = try await controller.beginAuthorization()

    let state = try await controller.completeAuthorization(
      challenge.id,
      callbackURL: try XCTUnwrap(
        URL(string: "wayfinder://oauth/test?code=approved&state=expected-state")
      )
    )

    XCTAssertEqual(
      state,
      ProviderAccountState(providerID: "test-account", readiness: .ready)
    )
    let savedCredential = await store.readSecret(for: credentialID)
    XCTAssertEqual(savedCredential, "provider-secret")

    let capturedRequest = await client.lastRequest()
    let request = try XCTUnwrap(capturedRequest)
    XCTAssertEqual(request.url, URL(string: "https://provider.example/token"))
    XCTAssertEqual(request.httpMethod, "POST")
    XCTAssertEqual(
      request.value(forHTTPHeaderField: "Content-Type"),
      "application/json"
    )
    let body = try XCTUnwrap(request.httpBody)
    let fields = try XCTUnwrap(
      JSONSerialization.jsonObject(with: body) as? [String: String]
    )
    XCTAssertEqual(fields["code"], "approved")
    XCTAssertEqual(fields["code_verifier"], String(repeating: "x", count: 43))
    XCTAssertEqual(fields["client_id"], "test-client")
    XCTAssertEqual(fields["redirect_uri"], "wayfinder://oauth/test")
    XCTAssertNil(fields["provider-secret"])
  }

  func testCallbackMismatchFailsClosedWithoutNetworkOrCredential() async throws {
    let store = InMemoryCredentialStore()
    let client = OAuthStubHTTPClient(
      response: HTTPDataResponse(statusCode: 200, body: Data())
    )
    let controller = makeController(store: store, client: client)
    let challenge = try await controller.beginAuthorization()

    do {
      _ = try await controller.completeAuthorization(
        challenge.id,
        callbackURL: try XCTUnwrap(
          URL(string: "other://oauth/test?code=approved&state=expected-state")
        )
      )
      XCTFail("Expected callback mismatch")
    } catch {
      XCTAssertEqual(error as? AuthorizationCodePKCEError, .callbackMismatch)
    }

    let containsCredential = await store.contains(credentialID)
    let requestCount = await client.requestCount()
    let accountState = await controller.state()
    XCTAssertFalse(containsCredential)
    XCTAssertEqual(requestCount, 0)
    XCTAssertEqual(accountState.readiness, .failed)
  }

  func testStateMismatchAndDuplicateCodeFailClosed() async throws {
    for callback in [
      "wayfinder://oauth/test?code=approved&state=unexpected",
      "wayfinder://oauth/test?code=first&code=second&state=expected-state",
    ] {
      let controller = makeController()
      let challenge = try await controller.beginAuthorization()

      do {
        _ = try await controller.completeAuthorization(
          challenge.id,
          callbackURL: try XCTUnwrap(URL(string: callback))
        )
        XCTFail("Expected callback validation failure")
      } catch {
        XCTAssertTrue(
          error as? AuthorizationCodePKCEError == .stateMismatch
            || error as? AuthorizationCodePKCEError == .missingCode
        )
      }
    }
  }

  func testProviderDenialIsDistinctAndLeavesNoCredential() async throws {
    let store = InMemoryCredentialStore()
    let controller = makeController(store: store)
    let challenge = try await controller.beginAuthorization()

    do {
      _ = try await controller.completeAuthorization(
        challenge.id,
        callbackURL: try XCTUnwrap(
          URL(
            string:
              "wayfinder://oauth/test?error=access_denied&state=expected-state"
          )
        )
      )
      XCTFail("Expected denial")
    } catch {
      XCTAssertEqual(error as? AuthorizationCodePKCEError, .authorizationDenied)
    }

    let containsCredential = await store.contains(credentialID)
    XCTAssertFalse(containsCredential)
  }

  func testCancellationClearsPendingAuthorizationAndDoesNotWriteCredential() async throws {
    let store = InMemoryCredentialStore()
    let controller = makeController(store: store)
    let challenge = try await controller.beginAuthorization()

    await controller.cancelAuthorization(challenge.id)

    let accountState = await controller.state()
    let containsCredential = await store.contains(credentialID)
    XCTAssertEqual(accountState.readiness, .signedOut)
    XCTAssertFalse(containsCredential)
    _ = try await controller.beginAuthorization()
  }

  func testRejectedMalformedAndOversizedResponsesAreSanitized() async throws {
    let responses = [
      HTTPDataResponse(statusCode: 401, body: Data("raw-secret".utf8)),
      HTTPDataResponse(statusCode: 200, body: Data(#"{"wrong":"field"}"#.utf8)),
      HTTPDataResponse(
        statusCode: 200,
        body: Data(
          repeating: 65,
          count: AuthorizationCodePKCEController.maximumTokenResponseBytes + 1
        )
      ),
    ]

    for response in responses {
      let store = InMemoryCredentialStore()
      let controller = makeController(
        store: store,
        client: OAuthStubHTTPClient(response: response)
      )
      let challenge = try await controller.beginAuthorization()

      do {
        _ = try await controller.completeAuthorization(
          challenge.id,
          callbackURL: try XCTUnwrap(
            URL(
              string:
                "wayfinder://oauth/test?code=approved&state=expected-state"
            )
          )
        )
        XCTFail("Expected exchange failure")
      } catch {
        XCTAssertFalse(error.localizedDescription.contains("raw-secret"))
      }
      let containsCredential = await store.contains(credentialID)
      XCTAssertFalse(containsCredential)
    }
  }

  func testRefreshAndSignOutExposeReadinessOnly() async throws {
    let store = InMemoryCredentialStore()
    try await store.save(secret: "never-expose-this", for: credentialID)
    let controller = makeController(store: store)

    let ready = await controller.refresh()
    XCTAssertEqual(ready.readiness, .ready)
    XCTAssertFalse(String(reflecting: ready).contains("never-expose-this"))

    try await controller.signOut()

    let signedOut = await controller.state()
    let containsCredential = await store.contains(credentialID)
    XCTAssertEqual(signedOut.readiness, .signedOut)
    XCTAssertFalse(containsCredential)
  }

  private func makeController(
    store: any CredentialStore = InMemoryCredentialStore(),
    client: any HTTPDataClient = OAuthStubHTTPClient(
      response: HTTPDataResponse(
        statusCode: 200,
        body: Data(#"{"access_token":"provider-secret"}"#.utf8)
      )
    ),
    nonceGenerator: any OAuthNonceGenerator = StubOAuthNonceGenerator(
      stateValue: "expected-state",
      verifierValue: String(repeating: "v", count: 43)
    )
  ) -> AuthorizationCodePKCEController {
    AuthorizationCodePKCEController(
      configuration: AuthorizationCodePKCEConfiguration(
        providerID: "test-account",
        authorizationEndpoint: URL(string: "https://provider.example/auth")!,
        tokenEndpoint: URL(string: "https://provider.example/token")!,
        callbackURL: URL(string: "wayfinder://oauth/test")!,
        clientID: "test-client",
        scopes: ["models.read", "chat.write"],
        tokenRequestEncoding: .json,
        credentialID: credentialID
      ),
      credentialStore: store,
      httpClient: client,
      nonceGenerator: nonceGenerator
    )
  }
}

private struct StubOAuthNonceGenerator: OAuthNonceGenerator {
  let stateValue: String
  let verifierValue: String

  func state() -> String {
    stateValue
  }

  func verifier() -> String {
    verifierValue
  }
}

private actor OAuthStubHTTPClient: HTTPDataClient {
  private let response: HTTPDataResponse
  private var requests: [URLRequest] = []

  init(response: HTTPDataResponse) {
    self.response = response
  }

  func data(_ request: URLRequest) -> HTTPDataResponse {
    requests.append(request)
    return response
  }

  func lastRequest() -> URLRequest? {
    requests.last
  }

  func requestCount() -> Int {
    requests.count
  }
}
