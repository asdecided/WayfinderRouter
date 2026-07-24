import Foundation

struct ProviderExecutionRequest: Equatable, Sendable {
  let id: UUID
  let prompt: String
  let destinationID: String
}

enum ProviderExecutionEvent: Equatable, Sendable {
  case delta(String)
  case completed
}

enum ProviderExecutionError: LocalizedError, Equatable, Sendable {
  case rejected(String)

  var errorDescription: String? {
    switch self {
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
