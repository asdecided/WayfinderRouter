import Foundation

struct OpenAICompatibleConfiguration: Equatable, Sendable {
  static let openAIPlatform = OpenAICompatibleConfiguration(
    destinationID: "openai-platform:gpt-5.6",
    providerID: "openai-platform",
    displayName: "GPT-5.6",
    modelID: "gpt-5.6",
    endpoint: URL(string: "https://api.openai.com/v1/chat/completions")!,
    credentialID: CredentialID(rawValue: "openai-platform.api-key")
  )

  let destinationID: String
  let providerID: String
  let displayName: String
  let modelID: String
  let endpoint: URL
  let credentialID: CredentialID
}

private struct OpenAIChatRequest: Encodable {
  struct Message: Encodable {
    let role: String
    let content: String
  }

  let model: String
  let messages: [Message]
  let stream: Bool
}

struct HTTPStreamResponse: Sendable {
  let statusCode: Int
  let body: AsyncThrowingStream<Data, Error>
}

protocol HTTPStreamingClient: Sendable {
  func stream(
    _ request: URLRequest,
    requestID: UUID
  ) async throws -> HTTPStreamResponse

  func cancel(requestID: UUID) async
}

actor URLSessionHTTPStreamingClient: HTTPStreamingClient {
  private let session: URLSession
  private var tasks: [UUID: Task<Void, Never>] = [:]

  init(session: URLSession = URLSession(configuration: .ephemeral)) {
    self.session = session
  }

  func stream(
    _ request: URLRequest,
    requestID: UUID
  ) async throws -> HTTPStreamResponse {
    tasks.removeValue(forKey: requestID)?.cancel()

    let (bytes, response) = try await session.bytes(for: request)
    guard let httpResponse = response as? HTTPURLResponse else {
      throw ProviderExecutionError.invalidResponse
    }

    let (stream, continuation) =
      AsyncThrowingStream<Data, Error>.makeStream()
    let task = Task { [weak self] in
      do {
        var chunk = Data()
        chunk.reserveCapacity(4_096)

        for try await byte in bytes {
          try Task.checkCancellation()
          chunk.append(byte)

          if chunk.count == 4_096 {
            continuation.yield(chunk)
            chunk.removeAll(keepingCapacity: true)
          }
        }

        if !chunk.isEmpty {
          continuation.yield(chunk)
        }
        continuation.finish()
      } catch is CancellationError {
        continuation.finish()
      } catch {
        continuation.finish(throwing: error)
      }

      await self?.removeTask(requestID)
    }

    tasks[requestID] = task
    continuation.onTermination = { @Sendable [weak self] _ in
      Task {
        await self?.cancel(requestID: requestID)
      }
    }

    return HTTPStreamResponse(
      statusCode: httpResponse.statusCode,
      body: stream
    )
  }

  func cancel(requestID: UUID) {
    tasks.removeValue(forKey: requestID)?.cancel()
  }

  private func removeTask(_ requestID: UUID) {
    tasks[requestID] = nil
  }
}

struct ServerSentEventParser: Sendable {
  static let maximumEventBytes = 524_288

  private var buffer = Data()

  mutating func append(_ data: Data) throws -> [String] {
    buffer.append(data)
    guard buffer.count <= Self.maximumEventBytes else {
      throw ProviderExecutionError.responseTooLarge
    }

    var payloads: [String] = []
    while let separator = separatorRange() {
      let event = buffer[..<separator.lowerBound]
      buffer.removeSubrange(..<separator.upperBound)

      if let payload = try parseEvent(Data(event)) {
        payloads.append(payload)
      }
    }
    return payloads
  }

  mutating func finish() throws -> [String] {
    guard !buffer.isEmpty else {
      return []
    }

    defer { buffer.removeAll() }
    if buffer.allSatisfy({ $0 == 10 || $0 == 13 }) {
      return []
    }
    if let payload = try parseEvent(buffer) {
      return [payload]
    }
    return []
  }

  private func separatorRange() -> Range<Data.Index>? {
    let lf = Data([10, 10])
    let crlf = Data([13, 10, 13, 10])
    let lfRange = buffer.range(of: lf)
    let crlfRange = buffer.range(of: crlf)

    switch (lfRange, crlfRange) {
    case (.none, .none):
      return nil
    case (.some(let range), .none), (.none, .some(let range)):
      return range
    case (.some(let lhs), .some(let rhs)):
      return lhs.lowerBound < rhs.lowerBound ? lhs : rhs
    }
  }

  private func parseEvent(_ data: Data) throws -> String? {
    guard let event = String(data: data, encoding: .utf8) else {
      throw ProviderExecutionError.invalidResponse
    }

    let payloadLines = event.split(
      omittingEmptySubsequences: false,
      whereSeparator: \.isNewline
    ).compactMap { line -> Substring? in
      guard line.hasPrefix("data:") else {
        return nil
      }

      var payload = line.dropFirst(5)
      if payload.first == " " {
        payload = payload.dropFirst()
      }
      return payload
    }

    guard !payloadLines.isEmpty else {
      return nil
    }
    return payloadLines.joined(separator: "\n")
  }
}

actor OpenAICompatibleProvider: ProviderExecutor {
  static let maximumRequestBytes = 1_048_576
  static let maximumResponseBytes = 4_194_304

  private let configuration: OpenAICompatibleConfiguration
  private let credentialStore: any CredentialStore
  private let httpClient: any HTTPStreamingClient
  private var tasks: [UUID: Task<Void, Never>] = [:]

  init(
    configuration: OpenAICompatibleConfiguration,
    credentialStore: any CredentialStore,
    httpClient: any HTTPStreamingClient = URLSessionHTTPStreamingClient()
  ) {
    self.configuration = configuration
    self.credentialStore = credentialStore
    self.httpClient = httpClient
  }

  func stream(
    _ request: ProviderExecutionRequest
  ) -> AsyncThrowingStream<ProviderExecutionEvent, Error> {
    let (stream, continuation) =
      AsyncThrowingStream<ProviderExecutionEvent, Error>.makeStream()
    tasks.removeValue(forKey: request.id)?.cancel()

    let task = Task { [weak self] in
      guard let self else {
        continuation.finish(throwing: ProviderExecutionError.interrupted)
        return
      }

      do {
        try await self.execute(request, continuation: continuation)
        continuation.finish()
      } catch is CancellationError {
        continuation.finish()
      } catch {
        continuation.finish(throwing: Self.normalized(error))
      }

      await self.httpClient.cancel(requestID: request.id)
      await self.removeTask(request.id)
    }

    tasks[request.id] = task
    continuation.onTermination = { @Sendable [weak self] _ in
      Task {
        await self?.cancel(requestID: request.id)
      }
    }
    return stream
  }

  func cancel(requestID: UUID) async {
    tasks.removeValue(forKey: requestID)?.cancel()
    await httpClient.cancel(requestID: requestID)
  }

  private func execute(
    _ request: ProviderExecutionRequest,
    continuation: AsyncThrowingStream<
      ProviderExecutionEvent,
      Error
    >.Continuation
  ) async throws {
    guard request.destinationID == configuration.destinationID else {
      throw ProviderExecutionError.rejected(
        "The selected destination is not available."
      )
    }
    guard
      let apiKey = try await credentialStore.readSecret(
        for: configuration.credentialID
      ),
      !apiKey.isEmpty
    else {
      throw ProviderExecutionError.authenticationRequired
    }

    let urlRequest = try makeRequest(
      messages: request.messages,
      apiKey: apiKey
    )
    let response = try await httpClient.stream(
      urlRequest,
      requestID: request.id
    )
    try validate(statusCode: response.statusCode)

    var parser = ServerSentEventParser()
    var receivedBytes = 0
    var reachedTerminalEvent = false

    for try await chunk in response.body {
      try Task.checkCancellation()
      receivedBytes += chunk.count
      guard receivedBytes <= Self.maximumResponseBytes else {
        throw ProviderExecutionError.responseTooLarge
      }

      for payload in try parser.append(chunk) {
        if try consume(payload, continuation: continuation) {
          reachedTerminalEvent = true
          break
        }
      }

      if reachedTerminalEvent {
        break
      }
    }

    if !reachedTerminalEvent {
      for payload in try parser.finish() {
        if try consume(payload, continuation: continuation) {
          reachedTerminalEvent = true
          break
        }
      }
    }

    try Task.checkCancellation()
    guard reachedTerminalEvent else {
      throw ProviderExecutionError.interrupted
    }
  }

  private func makeRequest(
    messages: [ProviderExecutionMessage],
    apiKey: String
  ) throws -> URLRequest {
    let body = try JSONEncoder().encode(
      OpenAIChatRequest(
        model: configuration.modelID,
        messages: messages.map {
          OpenAIChatRequest.Message(
            role: $0.role.rawValue,
            content: $0.content
          )
        },
        stream: true
      )
    )
    guard body.count <= Self.maximumRequestBytes else {
      throw ProviderExecutionError.requestTooLarge
    }

    var request = URLRequest(url: configuration.endpoint)
    request.httpMethod = "POST"
    request.timeoutInterval = 60
    request.httpBody = body
    request.setValue("application/json", forHTTPHeaderField: "Content-Type")
    request.setValue("text/event-stream", forHTTPHeaderField: "Accept")
    request.setValue(
      "Bearer \(apiKey)",
      forHTTPHeaderField: "Authorization"
    )
    return request
  }

  private func validate(statusCode: Int) throws {
    switch statusCode {
    case 200..<300:
      return
    case 401:
      throw ProviderExecutionError.authenticationRequired
    case 403:
      throw ProviderExecutionError.permissionDenied
    case 408:
      throw ProviderExecutionError.timedOut
    case 429:
      throw ProviderExecutionError.usageLimited
    case 500..<600:
      throw ProviderExecutionError.rejected(
        "The provider is temporarily unavailable."
      )
    default:
      throw ProviderExecutionError.rejected(
        "The provider rejected this request."
      )
    }
  }

  private func consume(
    _ payload: String,
    continuation: AsyncThrowingStream<
      ProviderExecutionEvent,
      Error
    >.Continuation
  ) throws -> Bool {
    if payload == "[DONE]" {
      continuation.yield(.completed)
      return true
    }

    struct ChatChunk: Decodable {
      struct Choice: Decodable {
        struct Delta: Decodable {
          let content: String?
        }

        let delta: Delta
      }

      struct ErrorEnvelope: Decodable {}

      let choices: [Choice]?
      let error: ErrorEnvelope?
    }

    guard let data = payload.data(using: .utf8) else {
      throw ProviderExecutionError.invalidResponse
    }
    let chunk: ChatChunk
    do {
      chunk = try JSONDecoder().decode(ChatChunk.self, from: data)
    } catch {
      throw ProviderExecutionError.invalidResponse
    }

    if chunk.error != nil {
      throw ProviderExecutionError.rejected(
        "The provider rejected this request."
      )
    }
    for content in chunk.choices?.compactMap(\.delta.content) ?? [] {
      if !content.isEmpty {
        continuation.yield(.delta(content))
      }
    }
    return false
  }

  nonisolated private static func normalized(_ error: Error) -> Error {
    if let providerError = error as? ProviderExecutionError {
      return providerError
    }
    if let urlError = error as? URLError {
      switch urlError.code {
      case .timedOut:
        return ProviderExecutionError.timedOut
      case .notConnectedToInternet,
        .networkConnectionLost,
        .cannotConnectToHost,
        .cannotFindHost,
        .dnsLookupFailed:
        return ProviderExecutionError.networkUnavailable
      case .cancelled:
        return CancellationError()
      default:
        return ProviderExecutionError.rejected(
          "Wayfinder could not complete this provider request."
        )
      }
    }
    return ProviderExecutionError.rejected(
      "Wayfinder could not complete this provider request."
    )
  }

  private func removeTask(_ requestID: UUID) {
    tasks[requestID] = nil
  }
}
