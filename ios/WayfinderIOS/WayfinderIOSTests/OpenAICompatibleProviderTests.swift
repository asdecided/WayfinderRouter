import Foundation
import XCTest

@testable import WayfinderIOS

final class OpenAICompatibleProviderTests: XCTestCase {
  func testFragmentedStreamProducesOrderedDeltasAndOneCompletion() async {
    let store = InMemoryCredentialStore()
    try? await store.save(
      secret: "test-api-key",
      for: OpenAICompatibleConfiguration.openAIPlatform.credentialID
    )
    let client = StubHTTPStreamingClient(
      statusCode: 200,
      chunks: [
        Data("data: {\"choices\":[{\"delta\":{\"cont".utf8),
        Data("ent\":\"Hello \"}}]}\n\n".utf8),
        Data("data: {\"choices\":[{\"delta\":{\"content\":\"world\"}}]}\n".utf8),
        Data("\ndata: [DONE]\n\n".utf8),
        Data(
          "data: {\"choices\":[{\"delta\":{\"content\":\"ignored\"}}]}\n\n"
            .utf8
        ),
      ]
    )
    let provider = OpenAICompatibleProvider(
      configuration: .openAIPlatform,
      credentialStore: store,
      httpClient: client
    )

    let result = await collect(
      from: await provider.stream(
        ProviderExecutionRequest(
          id: UUID(),
          prompt: "Say hello",
          destinationID:
            OpenAICompatibleConfiguration.openAIPlatform.destinationID,
          messages: [
            ProviderExecutionMessage(role: .user, content: "Earlier question"),
            ProviderExecutionMessage(
              role: .assistant,
              content: "Earlier answer"
            ),
            ProviderExecutionMessage(role: .user, content: "Say hello"),
          ]
        )
      )
    )

    XCTAssertEqual(
      result.events,
      [.delta("Hello "), .delta("world"), .completed]
    )
    XCTAssertNil(result.error)

    let captured = await client.capturedRequest()
    XCTAssertEqual(
      captured?.url,
      OpenAICompatibleConfiguration.openAIPlatform.endpoint
    )
    XCTAssertEqual(captured?.httpMethod, "POST")
    XCTAssertEqual(
      captured?.value(forHTTPHeaderField: "Authorization"),
      "Bearer test-api-key"
    )
    let body = try? XCTUnwrap(captured?.httpBody)
    let object = body.flatMap {
      try? JSONSerialization.jsonObject(with: $0) as? [String: Any]
    }
    XCTAssertEqual(object?["model"] as? String, "gpt-5.6")
    XCTAssertEqual(object?["stream"] as? Bool, true)
    let messages = object?["messages"] as? [[String: String]]
    XCTAssertEqual(messages?.map { $0["role"] }, ["user", "assistant", "user"])
    XCTAssertEqual(
      messages?.map { $0["content"] },
      ["Earlier question", "Earlier answer", "Say hello"]
    )
  }

  func testCompiledPresetsUseTheirOwnEndpointModelAndCredential() async {
    let cases: [(OpenAICompatibleConfiguration, String)] = [
      (.openAIPlatform, "openai-key"),
      (.moonshotPlatform, "moonshot-key"),
      (.openRouter, "openrouter-key"),
    ]

    for (configuration, key) in cases {
      let store = InMemoryCredentialStore()
      try? await store.save(secret: key, for: configuration.credentialID)
      let client = StubHTTPStreamingClient(
        statusCode: 200,
        chunks: [Data("data: [DONE]\n\n".utf8)]
      )
      let provider = OpenAICompatibleProvider(
        configurations: OpenAICompatibleConfiguration.supported,
        credentialStore: store,
        httpClient: client
      )

      let result = await collect(
        from: await provider.stream(
          ProviderExecutionRequest(
            id: UUID(),
            prompt: "Hello",
            destinationID: configuration.destinationID
          )
        )
      )

      XCTAssertEqual(result.events, [.completed])
      XCTAssertNil(result.error)
      let request = await client.capturedRequest()
      XCTAssertEqual(request?.url, configuration.endpoint)
      XCTAssertEqual(
        request?.value(forHTTPHeaderField: "Authorization"),
        "Bearer \(key)"
      )
      let body = try? XCTUnwrap(request?.httpBody)
      let object = body.flatMap {
        try? JSONSerialization.jsonObject(with: $0) as? [String: Any]
      }
      XCTAssertEqual(object?["model"] as? String, configuration.modelID)
    }
  }

  func testPresetCannotBorrowAnotherProvidersCredential() async {
    let store = InMemoryCredentialStore()
    try? await store.save(
      secret: "openai-key",
      for: OpenAICompatibleConfiguration.openAIPlatform.credentialID
    )
    let client = StubHTTPStreamingClient(statusCode: 200, chunks: [])
    let provider = OpenAICompatibleProvider(
      configurations: OpenAICompatibleConfiguration.supported,
      credentialStore: store,
      httpClient: client
    )

    let result = await collect(
      from: await provider.stream(
        ProviderExecutionRequest(
          id: UUID(),
          prompt: "Hello",
          destinationID:
            OpenAICompatibleConfiguration.moonshotPlatform.destinationID
        )
      )
    )

    XCTAssertEqual(
      result.error as? ProviderExecutionError,
      .authenticationRequired
    )
    let requestCount = await client.requestCount()
    XCTAssertEqual(requestCount, 0)
  }

  func testDiscoveredModelUsesItsProviderEndpointAndRequestedModelID() async {
    let store = InMemoryCredentialStore()
    try? await store.save(
      secret: "openrouter-key",
      for: OpenAICompatibleConfiguration.openRouter.credentialID
    )
    let client = StubHTTPStreamingClient(
      statusCode: 200,
      chunks: [Data("data: [DONE]\n\n".utf8)]
    )
    let provider = OpenAICompatibleProvider(
      configurations: OpenAICompatibleConfiguration.supported,
      credentialStore: store,
      httpClient: client
    )

    let result = await collect(
      from: await provider.stream(
        ProviderExecutionRequest(
          id: UUID(),
          prompt: "Hello",
          destinationID: "openrouter:anthropic/claude-sonnet"
        )
      )
    )

    XCTAssertEqual(result.events, [.completed])
    XCTAssertNil(result.error)
    let request = await client.capturedRequest()
    XCTAssertEqual(
      request?.url,
      OpenAICompatibleConfiguration.openRouter.endpoint
    )
    let body = try? XCTUnwrap(request?.httpBody)
    let object = body.flatMap {
      try? JSONSerialization.jsonObject(with: $0) as? [String: Any]
    }
    XCTAssertEqual(object?["model"] as? String, "anthropic/claude-sonnet")
  }

  func testMissingCredentialFailsBeforeNetworkRequest() async {
    let client = StubHTTPStreamingClient(statusCode: 200, chunks: [])
    let provider = OpenAICompatibleProvider(
      configuration: .openAIPlatform,
      credentialStore: InMemoryCredentialStore(),
      httpClient: client
    )

    let result = await collect(
      from: await provider.stream(makeRequest())
    )

    XCTAssertEqual(
      result.error as? ProviderExecutionError,
      .authenticationRequired
    )
    let requestCount = await client.requestCount()
    XCTAssertEqual(requestCount, 0)
  }

  func testHTTPFailuresAreSanitizedAndClassified() async {
    let cases: [(Int, ProviderExecutionError)] = [
      (401, .authenticationRequired),
      (403, .permissionDenied),
      (408, .timedOut),
      (429, .usageLimited),
      (503, .rejected("The provider is temporarily unavailable.")),
      (400, .rejected("The provider rejected this request.")),
    ]

    for (statusCode, expected) in cases {
      let result = await execute(statusCode: statusCode, chunks: [])
      XCTAssertEqual(
        result.error as? ProviderExecutionError,
        expected,
        "Unexpected classification for HTTP \(statusCode)"
      )
    }
  }

  func testNetworkTimeoutIsClassifiedWithoutRawProviderDetails() async {
    let result = await execute(throwing: URLError(.timedOut))

    XCTAssertEqual(
      result.error as? ProviderExecutionError,
      .timedOut
    )
    XCTAssertFalse(
      String(describing: result.error).contains("test-api-key")
    )
  }

  func testMalformedJSONFailsWithoutCompleting() async {
    let result = await execute(
      statusCode: 200,
      chunks: [Data("data: not-json\n\n".utf8)]
    )

    XCTAssertTrue(result.events.isEmpty)
    XCTAssertEqual(
      result.error as? ProviderExecutionError,
      .invalidResponse
    )
  }

  func testStreamWithoutDoneIsInterrupted() async {
    let result = await execute(
      statusCode: 200,
      chunks: [
        Data(
          "data: {\"choices\":[{\"delta\":{\"content\":\"Partial\"}}]}\n\n"
            .utf8
        )
      ]
    )

    XCTAssertEqual(result.events, [.delta("Partial")])
    XCTAssertEqual(
      result.error as? ProviderExecutionError,
      .interrupted
    )
  }

  func testOversizedPromptIsRejectedBeforeNetworkRequest() async {
    let store = InMemoryCredentialStore()
    try? await store.save(
      secret: "test-api-key",
      for: OpenAICompatibleConfiguration.openAIPlatform.credentialID
    )
    let client = StubHTTPStreamingClient(statusCode: 200, chunks: [])
    let provider = OpenAICompatibleProvider(
      configuration: .openAIPlatform,
      credentialStore: store,
      httpClient: client
    )
    let request = ProviderExecutionRequest(
      id: UUID(),
      prompt: String(
        repeating: "x",
        count: OpenAICompatibleProvider.maximumRequestBytes + 1
      ),
      destinationID:
        OpenAICompatibleConfiguration.openAIPlatform.destinationID
    )

    let result = await collect(from: await provider.stream(request))

    XCTAssertEqual(
      result.error as? ProviderExecutionError,
      .requestTooLarge
    )
    let requestCount = await client.requestCount()
    XCTAssertEqual(requestCount, 0)
  }

  func testOversizedResponseIsRejected() async {
    let result = await execute(
      statusCode: 200,
      chunks: [
        Data(
          repeating: 65,
          count: ServerSentEventParser.maximumEventBytes + 1
        )
      ]
    )

    XCTAssertEqual(
      result.error as? ProviderExecutionError,
      .responseTooLarge
    )
  }

  func testCancellationFinishesWithoutTerminalOrFailureEvent() async {
    let store = InMemoryCredentialStore()
    try? await store.save(
      secret: "test-api-key",
      for: OpenAICompatibleConfiguration.openAIPlatform.credentialID
    )
    let client = StubHTTPStreamingClient(
      statusCode: 200,
      chunks: [],
      holdsOpen: true
    )
    let provider = OpenAICompatibleProvider(
      configuration: .openAIPlatform,
      credentialStore: store,
      httpClient: client
    )
    let request = makeRequest()
    let stream = await provider.stream(request)
    let collection = Task {
      var events: [ProviderExecutionEvent] = []
      do {
        for try await event in stream {
          events.append(event)
        }
        return (events: events, error: nil as Error?)
      } catch {
        return (events: events, error: error as Error?)
      }
    }

    await Task.yield()
    await provider.cancel(requestID: request.id)
    let result = await collection.value

    XCTAssertTrue(result.events.isEmpty)
    XCTAssertNil(result.error)
    let cancellationCount = await client.cancellationCount()
    XCTAssertGreaterThanOrEqual(cancellationCount, 1)
  }

  private func execute(
    statusCode: Int,
    chunks: [Data]
  ) async -> (events: [ProviderExecutionEvent], error: Error?) {
    let store = InMemoryCredentialStore()
    try? await store.save(
      secret: "test-api-key",
      for: OpenAICompatibleConfiguration.openAIPlatform.credentialID
    )
    let provider = OpenAICompatibleProvider(
      configuration: .openAIPlatform,
      credentialStore: store,
      httpClient: StubHTTPStreamingClient(
        statusCode: statusCode,
        chunks: chunks
      )
    )
    return await collect(from: await provider.stream(makeRequest()))
  }

  private func execute(
    throwing error: Error
  ) async -> (events: [ProviderExecutionEvent], error: Error?) {
    let store = InMemoryCredentialStore()
    try? await store.save(
      secret: "test-api-key",
      for: OpenAICompatibleConfiguration.openAIPlatform.credentialID
    )
    let provider = OpenAICompatibleProvider(
      configuration: .openAIPlatform,
      credentialStore: store,
      httpClient: StubHTTPStreamingClient(error: error)
    )
    return await collect(from: await provider.stream(makeRequest()))
  }

  private func makeRequest() -> ProviderExecutionRequest {
    ProviderExecutionRequest(
      id: UUID(),
      prompt: "Hello",
      destinationID:
        OpenAICompatibleConfiguration.openAIPlatform.destinationID
    )
  }

  private func collect(
    from stream: AsyncThrowingStream<ProviderExecutionEvent, Error>
  ) async -> (events: [ProviderExecutionEvent], error: Error?) {
    var events: [ProviderExecutionEvent] = []
    do {
      for try await event in stream {
        events.append(event)
      }
      return (events, nil)
    } catch {
      return (events, error)
    }
  }
}

private actor StubHTTPStreamingClient: HTTPStreamingClient {
  private let statusCode: Int
  private let chunks: [Data]
  private let error: Error?
  private let holdsOpen: Bool
  private var requests: [URLRequest] = []
  private var cancellations = 0
  private var continuations: [UUID: AsyncThrowingStream<Data, Error>.Continuation] = [:]

  init(
    statusCode: Int,
    chunks: [Data],
    holdsOpen: Bool = false
  ) {
    self.statusCode = statusCode
    self.chunks = chunks
    self.error = nil
    self.holdsOpen = holdsOpen
  }

  init(error: Error) {
    statusCode = 0
    chunks = []
    self.error = error
    holdsOpen = false
  }

  func stream(
    _ request: URLRequest,
    requestID: UUID
  ) throws -> HTTPStreamResponse {
    requests.append(request)
    if let error {
      throw error
    }

    let (stream, continuation) =
      AsyncThrowingStream<Data, Error>.makeStream()
    for chunk in chunks {
      continuation.yield(chunk)
    }
    if holdsOpen {
      continuations[requestID] = continuation
    } else {
      continuation.finish()
    }
    return HTTPStreamResponse(statusCode: statusCode, body: stream)
  }

  func cancel(requestID: UUID) {
    cancellations += 1
    continuations.removeValue(forKey: requestID)?.finish()
  }

  func capturedRequest() -> URLRequest? {
    requests.last
  }

  func requestCount() -> Int {
    requests.count
  }

  func cancellationCount() -> Int {
    cancellations
  }
}
