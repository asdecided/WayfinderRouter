import Foundation
import XCTest

@testable import WayfinderIOS

final class OpenRouterAccountTests: XCTestCase {
  func testOpenRouterAuthorizationUsesDocumentedPKCEContract() async throws {
    let store = InMemoryCredentialStore()
    let client = OpenRouterOAuthStubHTTPClient(
      response: HTTPDataResponse(
        statusCode: 200,
        body: Data(#"{"key":"openrouter-user-key"}"#.utf8)
      )
    )
    let verifier = String(repeating: "r", count: 43)
    let controller = AuthorizationCodePKCEController(
      configuration: .openRouter,
      credentialStore: store,
      httpClient: client,
      nonceGenerator: OpenRouterOAuthNonceGenerator(
        stateValue: "state-value",
        verifierValue: verifier
      )
    )

    let challenge = try await controller.beginAuthorization()
    let authorizationComponents = try XCTUnwrap(
      URLComponents(
        url: challenge.authorizationURL,
        resolvingAgainstBaseURL: false
      )
    )
    let authorizationQuery = Dictionary(
      uniqueKeysWithValues: (authorizationComponents.queryItems ?? []).compactMap {
        item in item.value.map { (item.name, $0) }
      }
    )
    let callbackURL = try XCTUnwrap(
      URL(string: try XCTUnwrap(authorizationQuery["callback_url"]))
    )

    XCTAssertEqual(
      challenge.authorizationURL.host,
      "openrouter.ai"
    )
    XCTAssertEqual(
      callbackURL.absoluteString,
      "https://github.com/asdecided/WayfinderRouter/oauth/openrouter/state-value"
    )
    XCTAssertEqual(
      challenge.callback,
      .https(
        host: "github.com",
        path: "/asdecided/WayfinderRouter/oauth/openrouter/state-value"
      )
    )
    XCTAssertNil(authorizationQuery["state"])
    XCTAssertEqual(authorizationQuery["code_challenge_method"], "S256")
    XCTAssertNotNil(authorizationQuery["code_challenge"])

    var callbackComponents = try XCTUnwrap(
      URLComponents(url: callbackURL, resolvingAgainstBaseURL: false)
    )
    callbackComponents.queryItems = [URLQueryItem(name: "code", value: "approved")]
    let state = try await controller.completeAuthorization(
      challenge.id,
      callbackURL: try XCTUnwrap(callbackComponents.url)
    )

    XCTAssertEqual(state.readiness, .ready)
    let credential = await store.readSecret(
      for: OpenAICompatibleConfiguration.openRouter.credentialID
    )
    XCTAssertEqual(credential, "openrouter-user-key")

    let capturedRequest = await client.lastRequest()
    let request = try XCTUnwrap(capturedRequest)
    XCTAssertEqual(
      request.url,
      URL(string: "https://openrouter.ai/api/v1/auth/keys")
    )
    let body = try XCTUnwrap(request.httpBody)
    let fields = try XCTUnwrap(
      JSONSerialization.jsonObject(with: body) as? [String: String]
    )
    XCTAssertEqual(
      fields,
      [
        "code": "approved",
        "code_verifier": verifier,
        "code_challenge_method": "S256",
      ]
    )
  }

  @MainActor
  func testAccountConnectionReadiesOpenRouterDestinationsWithoutChangingAutomatic() async throws {
    let store = InMemoryCredentialStore()
    let controller = AuthorizationCodePKCEController(
      configuration: .openRouter,
      credentialStore: store,
      httpClient: OpenRouterOAuthStubHTTPClient(
        response: HTTPDataResponse(
          statusCode: 200,
          body: Data(#"{"key":"account-secret"}"#.utf8)
        )
      ),
      nonceGenerator: OpenRouterOAuthNonceGenerator(
        stateValue: "state-value",
        verifierValue: String(repeating: "r", count: 43)
      )
    )
    let model = AppModel(
      credentialStore: store,
      openRouterAccountController: controller,
      destinations: .liveDirectProviders
    )

    let pendingChallenge = await model.beginOpenRouterAuthorization()
    let challenge = try XCTUnwrap(pendingChallenge)
    let authorizationComponents = try XCTUnwrap(
      URLComponents(
        url: challenge.authorizationURL,
        resolvingAgainstBaseURL: false
      )
    )
    let callbackString = try XCTUnwrap(
      authorizationComponents.queryItems?.first {
        $0.name == "callback_url"
      }?.value
    )
    var callbackComponents = try XCTUnwrap(
      URLComponents(string: callbackString)
    )
    callbackComponents.queryItems = [URLQueryItem(name: "code", value: "approved")]

    let connected = await model.completeOpenRouterAuthorization(
      challenge.id,
      callbackURL: try XCTUnwrap(callbackComponents.url)
    )

    XCTAssertTrue(connected)
    XCTAssertEqual(model.openRouterAccountState.readiness, .ready)
    let openRouterDestinations = model.destinations.filter {
      $0.providerID == "openrouter"
    }
    XCTAssertEqual(
      Set(openRouterDestinations.map(\.modelID)),
      ["openrouter/auto", "openrouter/free"]
    )
    XCTAssertTrue(
      openRouterDestinations.allSatisfy {
        $0.readiness == .ready && !$0.automaticEligible
      }
    )
    XCTAssertNil(model.selectedDestinationID)
    XCTAssertFalse(String(reflecting: model.openRouterAccountState).contains("account-secret"))
  }

  @MainActor
  func testDisconnectRemovesCredentialAndMakesFreeDestinationUnavailable() async throws {
    let store = InMemoryCredentialStore()
    try await store.save(
      secret: "account-secret",
      for: OpenAICompatibleConfiguration.openRouter.credentialID
    )
    let controller = AuthorizationCodePKCEController(
      configuration: .openRouter,
      credentialStore: store
    )
    let model = AppModel(
      credentialStore: store,
      openRouterAccountController: controller,
      destinations: .liveDirectProviders
    )
    await model.restoreCredentialStatuses()

    await model.disconnectOpenRouterAccount()

    let credentialExists = await store.contains(
      OpenAICompatibleConfiguration.openRouter.credentialID
    )
    XCTAssertFalse(credentialExists)
    XCTAssertEqual(model.openRouterAccountState.readiness, .signedOut)
    XCTAssertEqual(
      model.destinations.first {
        $0.id == OpenAICompatibleConfiguration.openRouterFree.destinationID
      }?.readiness,
      .signedOut
    )
  }

  func testFreeDestinationIsExplicitRateLimitedAndNeverAutomatic() {
    let destination = [RoutingDestination].liveDirectProviders.first {
      $0.id == OpenAICompatibleConfiguration.openRouterFree.destinationID
    }

    XCTAssertEqual(destination?.modelID, "openrouter/free")
    XCTAssertEqual(destination?.detail, "No-cost models · rate limited")
    XCTAssertEqual(destination?.billingClass, .apiMetered)
    XCTAssertEqual(destination?.automaticEligible, false)
    XCTAssertEqual(
      destination?.credentialID,
      OpenAICompatibleConfiguration.openRouter.credentialID
    )
  }
}

private struct OpenRouterOAuthNonceGenerator: OAuthNonceGenerator {
  let stateValue: String
  let verifierValue: String

  func state() -> String {
    stateValue
  }

  func verifier() -> String {
    verifierValue
  }
}

private actor OpenRouterOAuthStubHTTPClient: HTTPDataClient {
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
}
