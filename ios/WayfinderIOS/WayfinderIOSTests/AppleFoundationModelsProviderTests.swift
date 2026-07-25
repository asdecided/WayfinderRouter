import XCTest

@testable import WayfinderIOS

final class AppleFoundationModelsProviderTests: XCTestCase {
  func testAvailabilityMappingPreservesDistinctNativeStates() {
    XCTAssertEqual(
      AppleFoundationModelsAvailabilityQuery.map(
        frameworkSupported: false,
        native: nil
      ),
      .unsupported
    )
    XCTAssertEqual(
      AppleFoundationModelsAvailabilityQuery.map(
        frameworkSupported: true,
        native: .available
      ),
      .available
    )
    XCTAssertEqual(
      AppleFoundationModelsAvailabilityQuery.map(
        frameworkSupported: true,
        native: .deviceNotEligible
      ),
      .deviceNotEligible
    )
    XCTAssertEqual(
      AppleFoundationModelsAvailabilityQuery.map(
        frameworkSupported: true,
        native: .appleIntelligenceNotEnabled
      ),
      .appleIntelligenceNotEnabled
    )
    XCTAssertEqual(
      AppleFoundationModelsAvailabilityQuery.map(
        frameworkSupported: true,
        native: .modelNotReady
      ),
      .modelNotReady
    )
    XCTAssertEqual(
      AppleFoundationModelsAvailabilityQuery.map(
        frameworkSupported: true,
        native: .unknownUnavailable
      ),
      .unavailable
    )
  }

  func testAggregatedSnapshotsBecomeOrderedDeltasAndOneTerminalEvent() async
    throws
  {
    let runtime = FakeAppleFoundationModelsRuntime(
      availability: .available,
      snapshots: ["Hello", "Hello world"]
    )
    let provider = NativeAppleFoundationModelsProvider(runtime: runtime)

    let events = try await collect(
      await provider.stream(makeRequest(prompt: "Say hello"))
    )

    XCTAssertEqual(
      events,
      [.delta("Hello"), .delta(" world"), .completed]
    )
    let calls = await runtime.capturedCalls()
    XCTAssertEqual(calls.count, 1)
    XCTAssertEqual(calls[0].prompt, "User: Say hello")
    XCTAssertEqual(
      calls[0].instructions,
      "Answer the user directly and accurately."
    )
  }

  func testConversationHistoryIsNormalizedWithoutPersistingASession() async
    throws
  {
    let runtime = FakeAppleFoundationModelsRuntime(
      availability: .available,
      snapshots: ["Done"]
    )
    let provider = NativeAppleFoundationModelsProvider(runtime: runtime)
    let request = ProviderExecutionRequest(
      id: UUID(),
      prompt: "Continue",
      destinationID: NativeAppleFoundationModelsProvider.destinationID,
      messages: [
        .init(role: .system, content: "Be concise."),
        .init(role: .user, content: "First"),
        .init(role: .assistant, content: "Second"),
        .init(role: .user, content: "Continue"),
      ]
    )

    _ = try await collect(await provider.stream(request))

    let calls = await runtime.capturedCalls()
    let call = try XCTUnwrap(calls.first)
    XCTAssertEqual(call.instructions, "Be concise.")
    XCTAssertEqual(
      call.prompt,
      "User: First\n\nAssistant: Second\n\nUser: Continue"
    )
  }

  func testUnavailableModelFailsBeforeCreatingNativeSession() async {
    let runtime = FakeAppleFoundationModelsRuntime(
      availability: .appleIntelligenceNotEnabled,
      snapshots: ["Never sent"]
    )
    let provider = NativeAppleFoundationModelsProvider(runtime: runtime)

    await XCTAssertThrowsErrorAsync(
      try await collect(await provider.stream(makeRequest()))
    ) { error in
      XCTAssertEqual(
        error as? ProviderExecutionError,
        .appleModelUnavailable(.appleIntelligenceNotEnabled)
      )
    }
    let calls = await runtime.capturedCalls()
    XCTAssertTrue(calls.isEmpty)
  }

  func testNonPrefixSnapshotFailsClosed() async {
    let runtime = FakeAppleFoundationModelsRuntime(
      availability: .available,
      snapshots: ["Hello", "Goodbye"]
    )
    let provider = NativeAppleFoundationModelsProvider(runtime: runtime)

    await XCTAssertThrowsErrorAsync(
      try await collect(await provider.stream(makeRequest()))
    ) { error in
      XCTAssertEqual(error as? ProviderExecutionError, .invalidResponse)
    }
  }

  func testOversizedResponseFailsWithBoundedError() async {
    let runtime = FakeAppleFoundationModelsRuntime(
      availability: .available,
      snapshots: [
        String(
          repeating: "a",
          count: NativeAppleFoundationModelsProvider.maximumResponseBytes + 1
        )
      ]
    )
    let provider = NativeAppleFoundationModelsProvider(runtime: runtime)

    await XCTAssertThrowsErrorAsync(
      try await collect(await provider.stream(makeRequest()))
    ) { error in
      XCTAssertEqual(error as? ProviderExecutionError, .responseTooLarge)
    }
  }

  func testOversizedRequestFailsBeforeCreatingNativeSession() async {
    let runtime = FakeAppleFoundationModelsRuntime(
      availability: .available,
      snapshots: []
    )
    let provider = NativeAppleFoundationModelsProvider(runtime: runtime)
    let request = makeRequest(
      prompt: String(
        repeating: "a",
        count: NativeAppleFoundationModelsProvider.maximumMessageBytes + 1
      )
    )

    await XCTAssertThrowsErrorAsync(
      try await collect(await provider.stream(request))
    ) { error in
      XCTAssertEqual(error as? ProviderExecutionError, .requestTooLarge)
    }
    let calls = await runtime.capturedCalls()
    XCTAssertTrue(calls.isEmpty)
  }

  func testCancellationProducesNoTerminalCompletion() async throws {
    let runtime = FakeAppleFoundationModelsRuntime(
      availability: .available,
      snapshots: ["Too late"],
      delay: .seconds(5)
    )
    let provider = NativeAppleFoundationModelsProvider(runtime: runtime)
    let request = makeRequest()
    let stream = await provider.stream(request)
    let collection = Task<[ProviderExecutionEvent], Error> {
      var events: [ProviderExecutionEvent] = []
      for try await event in stream {
        events.append(event)
      }
      return events
    }

    await provider.cancel(requestID: request.id)
    let events = try await collection.value

    XCTAssertTrue(events.isEmpty)
  }

  @MainActor
  func testAvailabilityRefreshPublishesTruthfulPinnedDestination() async {
    let runtime = FakeAppleFoundationModelsRuntime(
      availability: .available,
      snapshots: ["Offline reply"]
    )
    let appleProvider = NativeAppleFoundationModelsProvider(runtime: runtime)
    let model = AppModel(
      appleFoundationModelsProvider: appleProvider,
      providerExecutor: RoutedProviderExecutor(
        appleProvider: appleProvider,
        fallback: DeterministicMockProvider()
      ),
      destinations: .liveDirectProviders
    )

    await model.refreshModelInventory()

    let destination = model.destinations.first {
      $0.id == NativeAppleFoundationModelsProvider.destinationID
    }
    XCTAssertEqual(destination?.readiness, .ready)
    XCTAssertEqual(
      destination?.detail,
      AppleFoundationModelsAvailability.available.detail
    )
    XCTAssertEqual(destination?.boundary, .onDevice)
    XCTAssertEqual(destination?.billingClass, .onDevice)
    XCTAssertEqual(destination?.automaticEligible, false)
    XCTAssertNil(destination?.contextWindow)
    XCTAssertNil(model.selectedDestinationID)
  }

  @MainActor
  func testPinnedOnDeviceChatWorksUnderOnDeviceOnlyAndReceiptsBoundary() async {
    let runtime = FakeAppleFoundationModelsRuntime(
      availability: .available,
      snapshots: ["Private reply"]
    )
    let appleProvider = NativeAppleFoundationModelsProvider(runtime: runtime)
    let cloudProvider = RecordingProvider(response: "Cloud reply")
    let model = AppModel(
      appleFoundationModelsProvider: appleProvider,
      providerExecutor: RoutedProviderExecutor(
        appleProvider: appleProvider,
        fallback: cloudProvider
      ),
      destinations: .liveDirectProviders
    )
    await model.refreshModelInventory()
    model.privacyPosture = .onDeviceOnly
    model.selectedDestinationID =
      NativeAppleFoundationModelsProvider.destinationID
    model.draft = "Keep this local"

    await model.sendMessage()

    let assistant = model.activeThread?.messages.last
    XCTAssertEqual(assistant?.content, "Private reply")
    XCTAssertEqual(assistant?.status, .completed)
    XCTAssertEqual(assistant?.routeReceipt?.executionSummary, "On this device")
    let cloudRequestCount = await cloudProvider.requestCount()
    XCTAssertEqual(cloudRequestCount, 0)
  }

  private func makeRequest(
    prompt: String = "Hello"
  ) -> ProviderExecutionRequest {
    ProviderExecutionRequest(
      id: UUID(),
      prompt: prompt,
      destinationID: NativeAppleFoundationModelsProvider.destinationID
    )
  }

  private func collect(
    _ stream: AsyncThrowingStream<ProviderExecutionEvent, Error>
  ) async throws -> [ProviderExecutionEvent] {
    var events: [ProviderExecutionEvent] = []
    for try await event in stream {
      events.append(event)
    }
    return events
  }
}

private actor FakeAppleFoundationModelsRuntime:
  AppleFoundationModelsRuntime
{
  struct Call: Equatable, Sendable {
    let prompt: String
    let instructions: String
  }

  private let currentAvailability: AppleFoundationModelsAvailability
  private let snapshots: [String]
  private let delay: Duration
  private var calls: [Call] = []

  init(
    availability: AppleFoundationModelsAvailability,
    snapshots: [String],
    delay: Duration = .zero
  ) {
    currentAvailability = availability
    self.snapshots = snapshots
    self.delay = delay
  }

  func availability() -> AppleFoundationModelsAvailability {
    currentAvailability
  }

  func responseSnapshots(
    prompt: String,
    instructions: String
  ) -> AsyncThrowingStream<String, Error> {
    calls.append(Call(prompt: prompt, instructions: instructions))
    let snapshots = snapshots
    let delay = delay
    return AsyncThrowingStream { continuation in
      let task = Task {
        do {
          for snapshot in snapshots {
            try await Task.sleep(for: delay)
            try Task.checkCancellation()
            continuation.yield(snapshot)
          }
          continuation.finish()
        } catch is CancellationError {
          continuation.finish()
        } catch {
          continuation.finish(throwing: error)
        }
      }
      continuation.onTermination = { @Sendable _ in
        task.cancel()
      }
    }
  }

  func capturedCalls() -> [Call] {
    calls
  }
}

private actor RecordingProvider: ProviderExecutor {
  private let response: String
  private var requests: [ProviderExecutionRequest] = []

  init(response: String) {
    self.response = response
  }

  func stream(
    _ request: ProviderExecutionRequest
  ) -> AsyncThrowingStream<ProviderExecutionEvent, Error> {
    requests.append(request)
    return AsyncThrowingStream { continuation in
      continuation.yield(.delta(response))
      continuation.yield(.completed)
      continuation.finish()
    }
  }

  func cancel(requestID: UUID) {}

  func requestCount() -> Int {
    requests.count
  }
}

private func XCTAssertThrowsErrorAsync<T>(
  _ expression: @autoclosure () async throws -> T,
  _ errorHandler: (Error) -> Void,
  file: StaticString = #filePath,
  line: UInt = #line
) async {
  do {
    _ = try await expression()
    XCTFail("Expected expression to throw", file: file, line: line)
  } catch {
    errorHandler(error)
  }
}
