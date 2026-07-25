import Foundation
import XCTest

@testable import WayfinderIOS

final class ProviderModelCatalogTests: XCTestCase {
  func testKimiCatalogUsesAuthenticatedEndpointAndNormalizesModels() async {
    let store = InMemoryCredentialStore()
    try? await store.save(
      secret: "kimi-key",
      for: OpenAICompatibleConfiguration.moonshotPlatform.credentialID
    )
    let client = StubHTTPDataClient(
      response: HTTPDataResponse(
        statusCode: 200,
        body: Data(
          """
          {
            "object": "list",
            "data": [
              {
                "id": "kimi-k2.6",
                "context_length": 262144
              },
              {
                "id": "kimi-k2.5",
                "context_length": 262144
              }
            ]
          }
          """.utf8
        )
      )
    )
    let catalog = OpenAICompatibleModelCatalog(
      configurations: [.moonshotPlatform],
      credentialStore: store,
      httpClient: client
    )

    let results = await catalog.refresh()

    XCTAssertEqual(
      results,
      [
        ProviderModelInventoryResult(
          providerID: "moonshot-platform",
          state: .loaded([
            DiscoveredProviderModel(
              providerID: "moonshot-platform",
              modelID: "kimi-k2.5",
              displayName: "Kimi K2.5",
              contextWindow: 262_144
            ),
            DiscoveredProviderModel(
              providerID: "moonshot-platform",
              modelID: "kimi-k2.6",
              displayName: "Kimi K2.6",
              contextWindow: 262_144
            ),
          ])
        )
      ]
    )
    let request = await client.lastRequest()
    XCTAssertEqual(
      request?.url,
      OpenAICompatibleConfiguration.moonshotPlatform.modelsEndpoint
    )
    XCTAssertEqual(request?.httpMethod, "GET")
    XCTAssertEqual(
      request?.value(forHTTPHeaderField: "Authorization"),
      "Bearer kimi-key"
    )
  }

  func testOpenRouterCatalogUsesProviderDisplayMetadata() async {
    let store = InMemoryCredentialStore()
    try? await store.save(
      secret: "openrouter-key",
      for: OpenAICompatibleConfiguration.openRouter.credentialID
    )
    let client = StubHTTPDataClient(
      response: HTTPDataResponse(
        statusCode: 200,
        body: Data(
          """
          {
            "data": [
              {
                "id": "anthropic/claude-sonnet",
                "name": "Claude Sonnet",
                "context_length": 200000
              }
            ]
          }
          """.utf8
        )
      )
    )
    let catalog = OpenAICompatibleModelCatalog(
      configurations: [.openRouter],
      credentialStore: store,
      httpClient: client
    )

    let results = await catalog.refresh()

    XCTAssertEqual(
      results.first?.state,
      .loaded([
        DiscoveredProviderModel(
          providerID: "openrouter",
          modelID: "anthropic/claude-sonnet",
          displayName: "Claude Sonnet",
          contextWindow: 200_000
        )
      ])
    )
  }

  func testMissingCredentialSkipsNetworkAndClearsInventory() async {
    let client = StubHTTPDataClient(
      response: HTTPDataResponse(statusCode: 200, body: Data())
    )
    let catalog = OpenAICompatibleModelCatalog(
      configurations: [.moonshotPlatform],
      credentialStore: InMemoryCredentialStore(),
      httpClient: client
    )

    let results = await catalog.refresh()

    XCTAssertEqual(
      results,
      [
        ProviderModelInventoryResult(
          providerID: "moonshot-platform",
          state: .notConfigured
        )
      ]
    )
    let requestCount = await client.requestCount()
    XCTAssertEqual(requestCount, 0)
  }

  func testMalformedOrOversizedCatalogReturnsSanitizedFailure() async {
    let store = InMemoryCredentialStore()
    try? await store.save(
      secret: "kimi-key",
      for: OpenAICompatibleConfiguration.moonshotPlatform.credentialID
    )
    let malformed = OpenAICompatibleModelCatalog(
      configurations: [.moonshotPlatform],
      credentialStore: store,
      httpClient: StubHTTPDataClient(
        response: HTTPDataResponse(
          statusCode: 200,
          body: Data("not-json".utf8)
        )
      )
    )
    let oversized = OpenAICompatibleModelCatalog(
      configurations: [.moonshotPlatform],
      credentialStore: store,
      httpClient: StubHTTPDataClient(
        response: HTTPDataResponse(
          statusCode: 200,
          body: Data(
            repeating: 65,
            count: OpenAICompatibleModelCatalog.maximumResponseBytes + 1
          )
        )
      )
    )

    let malformedResults = await malformed.refresh()
    let oversizedResults = await oversized.refresh()

    XCTAssertEqual(malformedResults.first?.state, .failed)
    XCTAssertEqual(oversizedResults.first?.state, .failed)
    XCTAssertFalse(String(reflecting: malformedResults).contains("kimi-key"))
  }
}

private actor StubHTTPDataClient: HTTPDataClient {
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
