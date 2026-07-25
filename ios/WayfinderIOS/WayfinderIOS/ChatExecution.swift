import Foundation

enum ProviderExecutionMessageRole: String, Equatable, Sendable {
  case system
  case user
  case assistant
}

struct ProviderExecutionMessage: Equatable, Sendable {
  let role: ProviderExecutionMessageRole
  let content: String
}

struct ProviderExecutionRequest: Equatable, Sendable {
  let id: UUID
  let prompt: String
  let destinationID: String
  let messages: [ProviderExecutionMessage]

  init(
    id: UUID,
    prompt: String,
    destinationID: String,
    messages: [ProviderExecutionMessage]? = nil
  ) {
    self.id = id
    self.prompt = prompt
    self.destinationID = destinationID
    self.messages =
      messages ?? [
        ProviderExecutionMessage(role: .user, content: prompt)
      ]
  }
}

enum ProviderExecutionEvent: Equatable, Sendable {
  case delta(String)
  case completed
}

enum ProviderExecutionError: LocalizedError, Equatable, Sendable {
  case authenticationRequired
  case permissionDenied
  case usageLimited
  case networkUnavailable
  case timedOut
  case invalidResponse
  case requestTooLarge
  case responseTooLarge
  case interrupted
  case rejected(String)

  var errorDescription: String? {
    switch self {
    case .authenticationRequired:
      "Add or replace this provider's API key in Settings."
    case .permissionDenied:
      "This API key cannot use the selected model."
    case .usageLimited:
      "This provider is currently rate limited or out of API credit."
    case .networkUnavailable:
      "Wayfinder could not reach this provider. Check your connection."
    case .timedOut:
      "The provider took too long to respond."
    case .invalidResponse:
      "The provider returned a response Wayfinder could not read."
    case .requestTooLarge:
      "This message is too large to send."
    case .responseTooLarge:
      "The provider response exceeded Wayfinder's safety limit."
    case .interrupted:
      "The provider stopped before completing this reply."
    case .rejected(let message):
      message
    }
  }
}

protocol ProviderExecutor: Sendable {
  func stream(
    _ request: ProviderExecutionRequest
  ) async -> AsyncThrowingStream<ProviderExecutionEvent, Error>

  func cancel(requestID: UUID) async
}

actor DeterministicMockProvider: ProviderExecutor {
  enum Outcome: Equatable, Sendable {
    case response(chunks: [String])
    case failure(afterChunks: [String], message: String)
  }

  struct Configuration: Equatable, Sendable {
    let outcome: Outcome
    let delay: Duration

    static let conversational = Configuration(
      outcome: .response(
        chunks: [
          "This is a deterministic ",
          "preview response from Wayfinder. ",
          "No live provider was contacted.",
        ]
      ),
      delay: .milliseconds(120)
    )
  }

  private let configuration: Configuration
  private var tasks: [UUID: Task<Void, Never>] = [:]

  init(configuration: Configuration = .conversational) {
    self.configuration = configuration
  }

  func stream(
    _ request: ProviderExecutionRequest
  ) -> AsyncThrowingStream<ProviderExecutionEvent, Error> {
    let (stream, continuation) =
      AsyncThrowingStream<ProviderExecutionEvent, Error>.makeStream()
    let configuration = configuration

    let task = Task { [weak self] in
      do {
        switch configuration.outcome {
        case .response(let chunks):
          for chunk in chunks {
            try await Task.sleep(for: configuration.delay)
            try Task.checkCancellation()
            continuation.yield(.delta(chunk))
          }
          continuation.yield(.completed)
          continuation.finish()

        case .failure(let chunks, let message):
          for chunk in chunks {
            try await Task.sleep(for: configuration.delay)
            try Task.checkCancellation()
            continuation.yield(.delta(chunk))
          }
          continuation.finish(
            throwing: ProviderExecutionError.rejected(message)
          )
        }
      } catch is CancellationError {
        continuation.finish()
      } catch {
        continuation.finish(throwing: error)
      }

      await self?.removeTask(id: request.id)
    }

    tasks[request.id] = task
    continuation.onTermination = { @Sendable [weak self] _ in
      Task {
        await self?.cancel(requestID: request.id)
      }
    }
    return stream
  }

  func cancel(requestID: UUID) {
    tasks.removeValue(forKey: requestID)?.cancel()
  }

  private func removeTask(id: UUID) {
    tasks[id] = nil
  }
}
