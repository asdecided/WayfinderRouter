import Foundation
import WayfinderRoutingBridge

#if canImport(FoundationModels)
import FoundationModels
#endif

enum AppleFoundationModelsAvailability:
  String, Codable, CaseIterable, Equatable, Sendable
{
  case available
  case deviceNotEligible
  case appleIntelligenceNotEnabled
  case modelNotReady
  case unsupported
  case unavailable

  var readiness: ProviderReadiness {
    switch self {
    case .available:
      .ready
    case .deviceNotEligible:
      .modelUnavailable
    case .appleIntelligenceNotEnabled, .unavailable:
      .unavailable
    case .modelNotReady:
      .checking
    case .unsupported:
      .unsupportedPlatform
    }
  }

  var detail: String {
    switch self {
    case .available:
      "On this device · no network required"
    case .deviceNotEligible:
      "Apple Intelligence is not supported on this device"
    case .appleIntelligenceNotEnabled:
      "Enable Apple Intelligence in Settings"
    case .modelNotReady:
      "Apple's on-device model is still preparing"
    case .unsupported:
      "Requires a supported iOS or iPadOS version"
    case .unavailable:
      "Apple's on-device model is currently unavailable"
    }
  }
}

enum AppleFoundationModelsNativeAvailability: Equatable, Sendable {
  case available
  case deviceNotEligible
  case appleIntelligenceNotEnabled
  case modelNotReady
  case unknownUnavailable
}

enum AppleFoundationModelsAvailabilityQuery {
  static func map(
    frameworkSupported: Bool,
    native: AppleFoundationModelsNativeAvailability?
  ) -> AppleFoundationModelsAvailability {
    guard frameworkSupported, let native else {
      return .unsupported
    }
    switch native {
    case .available:
      return .available
    case .deviceNotEligible:
      return .deviceNotEligible
    case .appleIntelligenceNotEnabled:
      return .appleIntelligenceNotEnabled
    case .modelNotReady:
      return .modelNotReady
    case .unknownUnavailable:
      return .unavailable
    }
  }
}

protocol AppleFoundationModelsRuntime: Sendable {
  func availability() async -> AppleFoundationModelsAvailability
  func responseSnapshots(
    prompt: String,
    instructions: String
  ) async throws -> AsyncThrowingStream<String, Error>
}

actor NativeAppleFoundationModelsRuntime: AppleFoundationModelsRuntime {
  func availability() -> AppleFoundationModelsAvailability {
    #if canImport(FoundationModels)
    if #available(iOS 26.0, *) {
      return AppleFoundationModelsAvailabilityQuery.map(
        frameworkSupported: true,
        native: nativeAvailability()
      )
    }
    #endif
    return .unsupported
  }

  func responseSnapshots(
    prompt: String,
    instructions: String
  ) async throws -> AsyncThrowingStream<String, Error> {
    #if canImport(FoundationModels)
    if #available(iOS 26.0, *) {
      guard nativeAvailability() == .available else {
        throw ProviderExecutionError.appleModelUnavailable(
          AppleFoundationModelsAvailabilityQuery.map(
            frameworkSupported: true,
            native: nativeAvailability()
          )
        )
      }

      return AsyncThrowingStream { continuation in
        let task = Task {
          do {
            let session = LanguageModelSession(instructions: instructions)
            let stream = session.streamResponse(to: prompt)
            for try await snapshot in stream {
              try Task.checkCancellation()
              continuation.yield(snapshot.content)
            }
            continuation.finish()
          } catch is CancellationError {
            continuation.finish()
          } catch {
            continuation.finish(
              throwing: ProviderExecutionError.rejected(
                "Apple On-Device could not finish this reply."
              )
            )
          }
        }
        continuation.onTermination = { @Sendable _ in
          task.cancel()
        }
      }
    }
    #endif
    throw ProviderExecutionError.appleModelUnavailable(.unsupported)
  }

  #if canImport(FoundationModels)
  @available(iOS 26.0, *)
  private func nativeAvailability() -> AppleFoundationModelsNativeAvailability {
    switch SystemLanguageModel.default.availability {
    case .available:
      return .available
    case .unavailable(.deviceNotEligible):
      return .deviceNotEligible
    case .unavailable(.appleIntelligenceNotEnabled):
      return .appleIntelligenceNotEnabled
    case .unavailable(.modelNotReady):
      return .modelNotReady
    @unknown default:
      return .unknownUnavailable
    }
  }
  #endif
}

protocol AppleFoundationModelsProvider: ProviderExecutor {
  func availability() async -> AppleFoundationModelsAvailability
}

actor NativeAppleFoundationModelsProvider: AppleFoundationModelsProvider {
  static let destinationID = "apple-foundation-models:system-default"
  static let maximumMessages = 64
  static let maximumMessageBytes = 65_536
  static let maximumInstructionsBytes = 16_384
  static let maximumRequestBytes = 1_048_576
  static let maximumResponseBytes = 1_048_576

  private let runtime: any AppleFoundationModelsRuntime
  private var tasks: [UUID: Task<Void, Never>] = [:]

  init(runtime: any AppleFoundationModelsRuntime = NativeAppleFoundationModelsRuntime()) {
    self.runtime = runtime
  }

  func availability() async -> AppleFoundationModelsAvailability {
    await runtime.availability()
  }

  func stream(
    _ request: ProviderExecutionRequest
  ) -> AsyncThrowingStream<ProviderExecutionEvent, Error> {
    let (stream, continuation) =
      AsyncThrowingStream<ProviderExecutionEvent, Error>.makeStream()
    tasks.removeValue(forKey: request.id)?.cancel()

    let task = Task { [weak self, runtime] in
      do {
        guard request.destinationID == Self.destinationID else {
          throw ProviderExecutionError.rejected(
            "The selected on-device destination is not available."
          )
        }
        let availability = await runtime.availability()
        guard availability == .available else {
          throw ProviderExecutionError.appleModelUnavailable(availability)
        }
        let normalized = try Self.normalizedContent(request)
        let snapshots = try await runtime.responseSnapshots(
          prompt: normalized.prompt,
          instructions: normalized.instructions
        )
        var accumulated = ""

        for try await snapshot in snapshots {
          try Task.checkCancellation()
          guard snapshot.utf8.count <= Self.maximumResponseBytes else {
            throw ProviderExecutionError.responseTooLarge
          }
          guard snapshot.hasPrefix(accumulated) else {
            throw ProviderExecutionError.invalidResponse
          }
          let delta = String(snapshot.dropFirst(accumulated.count))
          accumulated = snapshot
          if !delta.isEmpty {
            continuation.yield(.delta(delta))
          }
        }

        try Task.checkCancellation()
        continuation.yield(.completed)
        continuation.finish()
      } catch is CancellationError {
        continuation.finish()
      } catch {
        continuation.finish(throwing: Self.normalized(error))
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

  private static func normalizedContent(
    _ request: ProviderExecutionRequest
  ) throws -> (instructions: String, prompt: String) {
    guard
      !request.messages.isEmpty,
      request.messages.count <= maximumMessages
    else {
      throw ProviderExecutionError.requestTooLarge
    }

    var instructions: [String] = []
    var transcript: [String] = []
    var totalBytes = 0

    for message in request.messages {
      let content = message.content.trimmingCharacters(
        in: .whitespacesAndNewlines
      )
      let contentBytes = content.utf8.count
      guard
        !content.isEmpty,
        contentBytes <= maximumMessageBytes,
        totalBytes + contentBytes <= maximumRequestBytes
      else {
        throw ProviderExecutionError.requestTooLarge
      }
      totalBytes += contentBytes

      switch message.role {
      case .system:
        instructions.append(content)
      case .user:
        transcript.append("User: \(content)")
      case .assistant:
        transcript.append("Assistant: \(content)")
      }
    }

    let instructionText =
      instructions.isEmpty
      ? "Answer the user directly and accurately."
      : instructions.joined(separator: "\n\n")
    guard instructionText.utf8.count <= maximumInstructionsBytes else {
      throw ProviderExecutionError.requestTooLarge
    }
    let prompt = transcript.joined(separator: "\n\n")
    guard !prompt.isEmpty else {
      throw ProviderExecutionError.requestTooLarge
    }
    return (instructionText, prompt)
  }

  private static func normalized(_ error: Error) -> Error {
    switch error {
    case let providerError as ProviderExecutionError:
      providerError
    case is CancellationError:
      CancellationError()
    default:
      ProviderExecutionError.rejected(
        "Apple On-Device could not finish this reply."
      )
    }
  }
}

actor UnavailableAppleFoundationModelsProvider: AppleFoundationModelsProvider {
  private let state: AppleFoundationModelsAvailability

  init(state: AppleFoundationModelsAvailability = .unsupported) {
    self.state = state
  }

  func availability() -> AppleFoundationModelsAvailability {
    state
  }

  func stream(
    _ request: ProviderExecutionRequest
  ) -> AsyncThrowingStream<ProviderExecutionEvent, Error> {
    AsyncThrowingStream { continuation in
      continuation.finish(
        throwing: ProviderExecutionError.appleModelUnavailable(state)
      )
    }
  }

  func cancel(requestID: UUID) {}
}

struct RoutedProviderExecutor: ProviderExecutor {
  private let appleProvider: any AppleFoundationModelsProvider
  private let fallback: any ProviderExecutor

  init(
    appleProvider: any AppleFoundationModelsProvider,
    fallback: any ProviderExecutor
  ) {
    self.appleProvider = appleProvider
    self.fallback = fallback
  }

  func stream(
    _ request: ProviderExecutionRequest
  ) async -> AsyncThrowingStream<ProviderExecutionEvent, Error> {
    if request.destinationID == NativeAppleFoundationModelsProvider.destinationID {
      return await appleProvider.stream(request)
    }
    return await fallback.stream(request)
  }

  func cancel(requestID: UUID) async {
    await appleProvider.cancel(requestID: requestID)
    await fallback.cancel(requestID: requestID)
  }
}
