import XCTest

@testable import WayfinderIOS

@MainActor
final class AppModelTests: XCTestCase {
  func testCredentialSnapshotNeverContainsSavedSecret() async {
    let store = InMemoryCredentialStore()
    let model = AppModel(credentialStore: store)
    let provider = APIKeyProviderDescriptor.supported[0]
    let secret = "do-not-expose-this-key"

    let saved = await model.saveAPIKey(secret, for: provider)

    XCTAssertTrue(saved)
    XCTAssertTrue(model.isCredentialConfigured(provider.id))
    XCTAssertEqual(model.configuredCredentialCount, 1)
    XCTAssertFalse(
      String(reflecting: model.credentialStatuses).contains(secret)
    )
    let export = await model.exportConversations()
    XCTAssertFalse(
      export.map { String(decoding: $0, as: UTF8.self).contains(secret) }
        ?? true
    )
  }

  func testRemovingCredentialUpdatesOnlyReadinessSnapshot() async {
    let store = InMemoryCredentialStore()
    let model = AppModel(credentialStore: store)
    let provider = APIKeyProviderDescriptor.supported[0]
    model.privacyPosture = .onDeviceOnly
    _ = await model.saveAPIKey("temporary-key", for: provider)

    await model.removeAPIKey(for: provider)

    XCTAssertFalse(model.isCredentialConfigured(provider.id))
    XCTAssertEqual(model.configuredCredentialCount, 0)
    XCTAssertEqual(model.privacyPosture, .onDeviceOnly)
  }

  func testDirectCredentialChangesReadinessWithoutChangingAutomatic() async {
    let store = InMemoryCredentialStore()
    let model = AppModel(
      credentialStore: store,
      destinations: .liveDirectProviders
    )

    _ = await model.saveAPIKey(
      "temporary-key",
      for: APIKeyProviderDescriptor.supported[0]
    )

    let destination = model.destinations.first {
      $0.id == OpenAICompatibleConfiguration.openAIPlatform.destinationID
    }
    XCTAssertEqual(destination?.readiness, .ready)
    XCTAssertEqual(destination?.automaticEligible, false)
    XCTAssertNil(model.selectedDestinationID)
  }

  func testEveryCompiledDirectPresetStartsPinnedAndUsesItsOwnCredential() {
    let hostedDestinations =
      [RoutingDestination].liveDirectProviders.filter {
        $0.boundary == .hosted
      }
    XCTAssertEqual(
      Set(
        hostedDestinations.map(\.id)
      ),
      Set(OpenAICompatibleConfiguration.supported.map(\.destinationID))
    )
    XCTAssertTrue(
      hostedDestinations.allSatisfy {
        !$0.automaticEligible && $0.readiness == .signedOut
      }
    )
    XCTAssertEqual(
      Set(
        hostedDestinations.compactMap(\.credentialID)
      ),
      Set(OpenAICompatibleConfiguration.supported.map(\.credentialID))
    )
  }

  func testMoonshotKeyReadiesOnlyMoonshotPreset() async {
    let store = InMemoryCredentialStore()
    let model = AppModel(
      credentialStore: store,
      destinations: .liveDirectProviders
    )

    _ = await model.saveAPIKey(
      "moonshot-key",
      for: APIKeyProviderDescriptor.supported[1]
    )

    XCTAssertEqual(
      model.destinations.first {
        $0.id == OpenAICompatibleConfiguration.moonshotPlatform.destinationID
      }?.readiness,
      .ready
    )
    XCTAssertTrue(
      model.destinations.filter {
        $0.boundary == .hosted
          && $0.id
            != OpenAICompatibleConfiguration.moonshotPlatform.destinationID
      }.allSatisfy { $0.readiness == .signedOut }
    )
    XCTAssertNil(model.selectedDestinationID)
  }

  func testDiscoveredModelPublishesReadyWithoutEnteringAutomatic() async {
    let store = InMemoryCredentialStore()
    let model = AppModel(
      credentialStore: store,
      providerModelCatalog: StubProviderModelCatalog(
        results: [
          ProviderModelInventoryResult(
            providerID: "moonshot-platform",
            state: .loaded([
              DiscoveredProviderModel(
                providerID: "moonshot-platform",
                modelID: "kimi-k2.5",
                displayName: "Kimi K2.5",
                contextWindow: 262_144
              )
            ])
          )
        ]
      ),
      destinations: .liveDirectProviders
    )

    _ = await model.saveAPIKey(
      "moonshot-key",
      for: APIKeyProviderDescriptor.supported[1]
    )

    let discovered = model.destinations.first {
      $0.id == "moonshot-platform:kimi-k2.5"
    }
    XCTAssertEqual(discovered?.readiness, .ready)
    XCTAssertEqual(discovered?.contextWindow, 262_144)
    XCTAssertEqual(discovered?.automaticEligible, false)
    XCTAssertNil(model.selectedDestinationID)
  }

  func testRemovingKeyRemovesItsDiscoveredModels() async {
    let store = InMemoryCredentialStore()
    let catalog = MutableProviderModelCatalog(
      results: [
        ProviderModelInventoryResult(
          providerID: "moonshot-platform",
          state: .loaded([
            DiscoveredProviderModel(
              providerID: "moonshot-platform",
              modelID: "kimi-k2.5",
              displayName: "Kimi K2.5",
              contextWindow: 262_144
            )
          ])
        )
      ]
    )
    let model = AppModel(
      credentialStore: store,
      providerModelCatalog: catalog,
      destinations: .liveDirectProviders
    )
    _ = await model.saveAPIKey(
      "moonshot-key",
      for: APIKeyProviderDescriptor.supported[1]
    )
    model.selectDestination("moonshot-platform:kimi-k2.5")
    await catalog.setResults([
      ProviderModelInventoryResult(
        providerID: "moonshot-platform",
        state: .notConfigured
      )
    ])

    await model.removeAPIKey(for: APIKeyProviderDescriptor.supported[1])

    XCTAssertFalse(
      model.destinations.contains {
        $0.id == "moonshot-platform:kimi-k2.5"
      }
    )
    XCTAssertNil(model.selectedDestinationID)
  }

  func testPinnedDirectDestinationRoutesOnlyAfterExplicitSelection() async {
    let store = InMemoryCredentialStore()
    let provider = DeterministicMockProvider(
      configuration: .init(
        outcome: .response(chunks: ["Direct reply"]),
        delay: .zero
      )
    )
    let model = AppModel(
      credentialStore: store,
      providerExecutor: provider,
      destinations: .liveDirectProviders
    )
    _ = await model.saveAPIKey(
      "temporary-key",
      for: APIKeyProviderDescriptor.supported[0]
    )
    model.selectDestination(
      OpenAICompatibleConfiguration.openAIPlatform.destinationID
    )
    model.draft = "Simple prompt"

    await model.sendMessage()

    XCTAssertEqual(
      model.activeThread?.messages.last?.content,
      "Direct reply"
    )
    XCTAssertEqual(
      model.activeThread?.messages.last?.routeReceipt?.destinationID,
      OpenAICompatibleConfiguration.openAIPlatform.destinationID
    )
    XCTAssertEqual(
      model.activeThread?.messages.last?.routeReceipt?.recommendation,
      "pinned"
    )
  }

  func testAutomaticDoesNotUseNewlyConfiguredDirectDestination() async {
    let store = InMemoryCredentialStore()
    let model = AppModel(
      credentialStore: store,
      destinations: .liveDirectProviders
    )
    _ = await model.saveAPIKey(
      "temporary-key",
      for: APIKeyProviderDescriptor.supported[0]
    )
    model.draft = "Simple prompt"

    await model.sendMessage()

    XCTAssertNil(model.selectedDestinationID)
    XCTAssertEqual(model.activeThread?.messages.last?.status, .failed)
    XCTAssertTrue(
      model.activeThread?.messages.last?.content.contains(
        "No Automatic destination"
      ) ?? false
    )
  }

  func testSupportedAPIKeyProvidersKeepPlatformAndAccountAccessDistinct() {
    XCTAssertEqual(
      APIKeyProviderDescriptor.supported.map(\.displayName),
      ["OpenAI Platform", "Moonshot / Kimi Platform", "OpenRouter"]
    )
    XCTAssertFalse(
      APIKeyProviderDescriptor.supported.map(\.displayName).contains("ChatGPT")
    )
  }

  func testSimplePromptRoutesToOnDeviceCandidate() async {
    let model = AppModel()
    model.draft = "Hello"

    await model.previewRoute()

    guard case .routed(let preview) = model.routePreviewState else {
      return XCTFail("Expected a routed preview")
    }
    XCTAssertEqual(preview.destinationID, "device-preview")
    XCTAssertEqual(preview.executionSummary, "On this device")
    XCTAssertEqual(preview.score, 0.0)
    XCTAssertEqual(model.submittedPrompt, "Hello")
  }

  func testStructuredPromptRoutesToHostedCandidateWhenAllowed() async {
    let model = AppModel()
    model.draft = "# Plan\n\n## Steps\n- one\n- two\n- three\n1. first\n2. second"

    await model.previewRoute()

    guard case .routed(let preview) = model.routePreviewState else {
      return XCTFail("Expected a routed preview")
    }
    XCTAssertEqual(preview.destinationID, "hosted-preview")
    XCTAssertEqual(preview.executionSummary, "Hosted cloud")
    XCTAssertEqual(preview.score, 0.15)
  }

  func testOnDeviceOnlyExcludesHostedRecommendation() async {
    let model = AppModel()
    model.privacyPosture = .onDeviceOnly
    model.draft = "# Plan\n\n## Steps\n- one\n- two\n- three\n1. first\n2. second"

    await model.previewRoute()

    guard case .unavailable(let message) = model.routePreviewState else {
      return XCTFail("Expected no eligible route")
    }
    XCTAssertTrue(message.contains("On-Device Only"))
  }

  func testEmptyPromptDoesNotRoute() async {
    let model = AppModel()
    model.draft = " \n "

    await model.previewRoute()

    XCTAssertEqual(
      model.routePreviewState,
      .unavailable("Enter a message to send.")
    )
  }

  func testRootTabsRemainCompleteAndStable() {
    XCTAssertEqual(
      AppTab.allCases.map(\.title),
      ["Chat", "Threads", "Destinations", "Settings"]
    )
  }

  func testNewChatClearsTransientConversationState() async {
    let model = AppModel()
    model.selectedTab = .settings
    model.draft = "Hello"
    await model.previewRoute()

    await model.startNewChat()

    XCTAssertEqual(model.selectedTab, .chat)
    XCTAssertEqual(model.draft, "")
    XCTAssertNil(model.submittedPrompt)
    XCTAssertEqual(model.routePreviewState, .idle)
  }

  func testSavedConversationRestoresIntoNewModel() async {
    let store = InMemoryConversationStore()
    let timestamp = Date(timeIntervalSince1970: 1_700_000_000)
    let firstModel = AppModel(
      conversationStore: store,
      now: { timestamp }
    )
    firstModel.draft = "Restore this conversation"

    await firstModel.previewRoute()

    let restoredModel = AppModel(conversationStore: store)
    await restoredModel.restoreConversations()

    XCTAssertEqual(restoredModel.threads.count, 1)
    XCTAssertEqual(
      restoredModel.activeThread?.messages.first?.content,
      "Restore this conversation"
    )
    XCTAssertEqual(restoredModel.submittedPrompt, "Restore this conversation")
  }

  func testSecondTurnAppendsToActiveConversation() async {
    let store = InMemoryConversationStore()
    let model = AppModel(conversationStore: store)
    model.draft = "First turn"
    await model.previewRoute()
    model.draft = "Second turn"

    await model.previewRoute()

    XCTAssertEqual(model.threads.count, 1)
    XCTAssertEqual(
      model.activeThread?.messages
        .filter { $0.role == .user }
        .map(\.content),
      ["First turn", "Second turn"]
    )
  }

  func testProviderReceivesOnlyCompletedHistoryBeforeNewPrompt() async {
    let provider = CapturingProvider(
      outcomes: [
        .response("First reply"),
        .failure(partial: "Partial reply"),
        .response("Third reply"),
      ]
    )
    let model = AppModel(providerExecutor: provider)
    model.draft = "First question"
    await model.sendMessage()
    model.draft = "Failed question"
    await model.sendMessage()
    model.draft = "Third question"
    await model.sendMessage()

    let requests = await provider.capturedRequests()

    XCTAssertEqual(requests.count, 3)
    XCTAssertEqual(
      requests[2].messages,
      [
        ProviderExecutionMessage(role: .user, content: "First question"),
        ProviderExecutionMessage(role: .assistant, content: "First reply"),
        ProviderExecutionMessage(role: .user, content: "Third question"),
      ]
    )
  }

  func testDeterministicProviderStreamsOrderedAssistantReply() async {
    let provider = DeterministicMockProvider(
      configuration: .init(
        outcome: .response(chunks: ["First ", "second ", "third."]),
        delay: .zero
      )
    )
    let model = AppModel(providerExecutor: provider)
    model.draft = "Stream this"

    await model.sendMessage()

    let messages = model.activeThread?.messages ?? []
    XCTAssertEqual(messages.map(\.role), [.user, .assistant])
    XCTAssertEqual(messages[1].content, "First second third.")
    XCTAssertEqual(messages[1].status, .completed)
    XCTAssertNotNil(messages[1].routeReceipt)
    XCTAssertEqual(model.executionPhase, .idle)
  }

  func testProviderFailurePreservesPartialReplyAndOffersRetryState() async {
    let provider = DeterministicMockProvider(
      configuration: .init(
        outcome: .failure(
          afterChunks: ["Partial reply"],
          message: "The deterministic provider rejected this request."
        ),
        delay: .zero
      )
    )
    let model = AppModel(providerExecutor: provider)
    model.draft = "Fail after output"

    await model.sendMessage()

    let assistant = model.activeThread?.messages.last
    XCTAssertEqual(assistant?.role, .assistant)
    XCTAssertEqual(assistant?.content, "Partial reply")
    XCTAssertEqual(assistant?.status, .failed)
  }

  func testStoppingGenerationProducesOneStoppedAssistantMessage() async {
    let provider = DeterministicMockProvider(
      configuration: .init(
        outcome: .response(chunks: ["Too ", "slow"]),
        delay: .seconds(5)
      )
    )
    let model = AppModel(providerExecutor: provider)
    model.draft = "Stop this"
    let sendTask = Task {
      await model.sendMessage()
    }

    await waitUntil {
      if case .streaming = model.executionPhase {
        return true
      }
      return false
    }
    await model.stopGenerating()
    await sendTask.value

    let assistantMessages =
      model.activeThread?.messages.filter { $0.role == .assistant } ?? []
    XCTAssertEqual(assistantMessages.count, 1)
    XCTAssertEqual(assistantMessages[0].status, .stopped)
    XCTAssertEqual(model.executionPhase, .idle)
  }

  /// UX-001. This previously asserted that retry appended a second copy of the
  /// user's prompt. That behaviour misrepresented the user's actions in the
  /// persisted history, so the expectation is inverted: a retry regenerates
  /// the failed reply in place.
  func testRetryRegeneratesTheFailedReplyInPlace() async {
    let provider = DeterministicMockProvider(
      configuration: .init(
        outcome: .failure(afterChunks: [], message: "Preview failed."),
        delay: .zero
      )
    )
    let model = AppModel(providerExecutor: provider)
    model.draft = "Try this"
    await model.sendMessage()
    let failedID = try! XCTUnwrap(model.activeThread?.messages.last?.id)

    await model.retry(messageID: failedID)

    let messages = model.activeThread?.messages ?? []
    XCTAssertEqual(messages.map(\.role), [.user, .assistant])
    XCTAssertEqual(
      messages.filter { $0.role == .user }.map(\.content),
      ["Try this"],
      "retry duplicated the user turn"
    )
    XCTAssertEqual(messages.last?.id, failedID, "the reply moved to a new slot")
    XCTAssertEqual(messages.last?.status, .failed)
  }

  func testSuccessfulRetryReplacesTheFailedReplyWithItsAnswer() async {
    let provider = SwitchableMockProvider(
      first: .failure(afterChunks: ["half "], message: "Preview failed."),
      second: .response(chunks: ["A ", "complete ", "answer."])
    )
    let model = AppModel(providerExecutor: provider)
    model.draft = "Try this"
    await model.sendMessage()
    let failedID = try! XCTUnwrap(model.activeThread?.messages.last?.id)
    XCTAssertEqual(model.activeThread?.messages.last?.status, .failed)

    await model.retry(messageID: failedID)

    let messages = model.activeThread?.messages ?? []
    XCTAssertEqual(messages.count, 2)
    XCTAssertEqual(messages.last?.id, failedID)
    XCTAssertEqual(messages.last?.content, "A complete answer.")
    XCTAssertEqual(messages.last?.status, .completed)
  }

  func testRetryLeavesAnUnsentDraftAlone() async {
    // The audited build assigned the prompt to `draft` to re-send it, which
    // silently destroyed whatever the user was typing.
    let provider = DeterministicMockProvider(
      configuration: .init(
        outcome: .failure(afterChunks: [], message: "Preview failed."),
        delay: .zero
      )
    )
    let model = AppModel(providerExecutor: provider)
    model.draft = "Try this"
    await model.sendMessage()
    let failedID = try! XCTUnwrap(model.activeThread?.messages.last?.id)
    model.draft = "a different thought"

    await model.retry(messageID: failedID)

    XCTAssertEqual(model.draft, "a different thought")
  }

  func testRetryDoesNotResendThePromptToTheProvider() async {
    // Regenerating must ask the provider the original question exactly once.
    let provider = SwitchableMockProvider(
      first: .failure(afterChunks: [], message: "Preview failed."),
      second: .response(chunks: ["ok"])
    )
    let model = AppModel(providerExecutor: provider)
    model.draft = "Only once"
    await model.sendMessage()
    let failedID = try! XCTUnwrap(model.activeThread?.messages.last?.id)

    await model.retry(messageID: failedID)

    let lastRequest = await provider.lastRequestMessages
    XCTAssertEqual(
      lastRequest.filter { $0.content == "Only once" }.count,
      1,
      "the prompt was sent twice in one request"
    )
  }

  func testRestoreMarksPendingAssistantMessageInterrupted() async {
    let store = InMemoryConversationStore()
    let threadID = UUID()
    let pendingID = UUID()
    await store.save(
      thread: ConversationThreadSnapshot(
        id: threadID,
        title: "Interrupted",
        createdAt: .distantPast,
        updatedAt: .distantPast,
        messages: [
          ConversationMessageSnapshot(
            id: UUID(),
            role: .user,
            content: "Continue",
            createdAt: .distantPast,
            status: .completed,
            routeReceipt: nil
          ),
          ConversationMessageSnapshot(
            id: pendingID,
            role: .assistant,
            content: "Partial",
            createdAt: .distantPast,
            status: .pending,
            routeReceipt: nil
          ),
        ],
        draft: ""
      )
    )
    await store.save(
      workspace: ConversationWorkspaceSnapshot(
        activeThreadID: threadID,
        draft: "",
        retentionDays: nil,
        updatedAt: .distantPast
      )
    )
    let model = AppModel(conversationStore: store)

    await model.restoreConversations()

    XCTAssertEqual(
      model.activeThread?.messages.first { $0.id == pendingID }?.status,
      .interrupted
    )
  }

  func testNewChatDraftRestoresWithoutCreatingThread() async {
    let store = InMemoryConversationStore()
    let firstModel = AppModel(conversationStore: store)
    firstModel.draft = "Unsent draft"
    await firstModel.saveDraft()

    let restoredModel = AppModel(conversationStore: store)
    await restoredModel.restoreConversations()

    XCTAssertEqual(restoredModel.draft, "Unsent draft")
    XCTAssertTrue(restoredModel.threads.isEmpty)
    XCTAssertNil(restoredModel.activeThreadID)
  }

  func testRetentionPolicyPrunesOldConversationOnRestore() async {
    let store = InMemoryConversationStore()
    let now = Date(timeIntervalSince1970: 10_000_000)
    let oldThread = ConversationThreadSnapshot(
      id: UUID(),
      title: "Old",
      createdAt: now.addingTimeInterval(-100 * 86_400),
      updatedAt: now.addingTimeInterval(-100 * 86_400),
      messages: [],
      draft: ""
    )
    await store.save(thread: oldThread)
    await store.save(
      workspace: ConversationWorkspaceSnapshot(
        activeThreadID: nil,
        draft: "",
        retentionDays: 30,
        updatedAt: now
      )
    )
    let model = AppModel(
      conversationStore: store,
      now: { now }
    )

    await model.restoreConversations()

    XCTAssertEqual(model.retentionPolicy, .thirtyDays)
    XCTAssertTrue(model.threads.isEmpty)
  }

  private func waitUntil(
    timeout: Duration = .seconds(1),
    condition: @escaping @MainActor () -> Bool
  ) async {
    let clock = ContinuousClock()
    let deadline = clock.now.advanced(by: timeout)

    while !condition(), clock.now < deadline {
      await Task.yield()
    }

    XCTAssertTrue(condition(), "Timed out waiting for state transition")
  }
}

private actor CapturingProvider: ProviderExecutor {
  enum Outcome {
    case response(String)
    case failure(partial: String)
  }

  private var outcomes: [Outcome]
  private var requests: [ProviderExecutionRequest] = []

  init(outcomes: [Outcome]) {
    self.outcomes = outcomes
  }

  func stream(
    _ request: ProviderExecutionRequest
  ) -> AsyncThrowingStream<ProviderExecutionEvent, Error> {
    requests.append(request)
    let outcome = outcomes.removeFirst()
    let (stream, continuation) =
      AsyncThrowingStream<ProviderExecutionEvent, Error>.makeStream()

    switch outcome {
    case .response(let content):
      continuation.yield(.delta(content))
      continuation.yield(.completed)
      continuation.finish()
    case .failure(let partial):
      continuation.yield(.delta(partial))
      continuation.finish(
        throwing: ProviderExecutionError.rejected("Provider failed.")
      )
    }
    return stream
  }

  func cancel(requestID: UUID) {}

  func capturedRequests() -> [ProviderExecutionRequest] {
    requests
  }
}

/// Answers the first request one way and every later request another, so a
/// retry can be observed producing a different outcome from the attempt it
/// replaces.
private actor SwitchableMockProvider: ProviderExecutor {
  private let first: DeterministicMockProvider.Outcome
  private let second: DeterministicMockProvider.Outcome
  private var requestCount = 0
  private(set) var lastRequestMessages: [ProviderExecutionMessage] = []

  init(
    first: DeterministicMockProvider.Outcome,
    second: DeterministicMockProvider.Outcome
  ) {
    self.first = first
    self.second = second
  }

  func stream(
    _ request: ProviderExecutionRequest
  ) -> AsyncThrowingStream<ProviderExecutionEvent, Error> {
    lastRequestMessages = request.messages
    let outcome = requestCount == 0 ? first : second
    requestCount += 1

    let (stream, continuation) =
      AsyncThrowingStream<ProviderExecutionEvent, Error>.makeStream()

    switch outcome {
    case .response(let chunks):
      for chunk in chunks {
        continuation.yield(.delta(chunk))
      }
      continuation.yield(.completed)
      continuation.finish()
    case .failure(let chunks, let message):
      for chunk in chunks {
        continuation.yield(.delta(chunk))
      }
      continuation.finish(throwing: ProviderExecutionError.rejected(message))
    }
    return stream
  }

  func cancel(requestID: UUID) {}
}

private struct StubProviderModelCatalog: ProviderModelCatalog {
  let results: [ProviderModelInventoryResult]

  func refresh() async -> [ProviderModelInventoryResult] {
    results
  }
}

private actor MutableProviderModelCatalog: ProviderModelCatalog {
  private var results: [ProviderModelInventoryResult]

  init(results: [ProviderModelInventoryResult]) {
    self.results = results
  }

  func refresh() -> [ProviderModelInventoryResult] {
    results
  }

  func setResults(_ results: [ProviderModelInventoryResult]) {
    self.results = results
  }
}
