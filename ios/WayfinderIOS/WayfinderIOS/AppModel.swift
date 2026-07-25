import Foundation
import Observation
import WayfinderRoutingBridge

enum AppTab: Hashable, CaseIterable {
  case chat
  case threads
  case destinations
  case settings

  var title: String {
    switch self {
    case .chat: "Chat"
    case .threads: "Threads"
    case .destinations: "Destinations"
    case .settings: "Settings"
    }
  }

  var systemImage: String {
    switch self {
    case .chat: "bubble.left.and.bubble.right"
    case .threads: "clock"
    case .destinations: "point.3.connected.trianglepath.dotted"
    case .settings: "gearshape"
    }
  }
}

enum PrivacyPostureOption: String, CaseIterable, Identifiable {
  case onDeviceOnly
  case localDevices
  case hostedAllowed

  var id: Self { self }

  var title: String {
    switch self {
    case .onDeviceOnly: "On-Device Only"
    case .localDevices: "Local Devices"
    case .hostedAllowed: "Hosted Allowed"
    }
  }

  var boundarySummary: String {
    switch self {
    case .onDeviceOnly: "This iPhone or iPad only"
    case .localDevices: "This device and trusted local devices"
    case .hostedAllowed: "On-device, local-network, and hosted destinations"
    }
  }

  var bridgeValue: PrivacyPosture {
    switch self {
    case .onDeviceOnly: .onDeviceOnly
    case .localDevices: .localDevices
    case .hostedAllowed: .hostedAllowed
    }
  }
}

/// A storage operation that failed and can be tried again (UX-015).
enum PersistenceRetryAction: Equatable {
  case saveThread(ConversationThreadSnapshot)
  case saveDraft
  case restoreConversations
  case openThread(UUID)
  case refreshThreads
  case deleteThread(UUID)
  case deleteAllThreads
  case applyRetention
  case exportConversations
}

struct RoutePreview: Equatable, Identifiable {
  let destinationID: String
  let destinationName: String
  let score: Double
  let recommendation: String
  let executionSummary: String

  var id: String { destinationID }
}

enum RoutePreviewState: Equatable {
  case idle
  case routed(RoutePreview)
  case unavailable(String)
}

enum ChatExecutionPhase: Equatable {
  case idle
  case routing(UUID)
  case streaming(UUID)
  case stopping(UUID)

  var requestID: UUID? {
    switch self {
    case .idle:
      nil
    case .routing(let requestID),
      .streaming(let requestID),
      .stopping(let requestID):
      requestID
    }
  }

  var isActive: Bool {
    requestID != nil
  }
}

enum ConversationRetentionPolicy: String, CaseIterable, Identifiable {
  case thirtyDays
  case ninetyDays
  case forever

  var id: Self { self }

  var title: String {
    switch self {
    case .thirtyDays: "30 days"
    case .ninetyDays: "90 days"
    case .forever: "Forever"
    }
  }

  var days: Int? {
    switch self {
    case .thirtyDays: 30
    case .ninetyDays: 90
    case .forever: nil
    }
  }

  init(days: Int?) {
    switch days {
    case 30: self = .thirtyDays
    case 90: self = .ninetyDays
    default: self = .forever
    }
  }
}

@MainActor
@Observable
final class AppModel {
  var selectedTab: AppTab = .chat
  var draft = ""
  var submittedPrompt: String?
  var privacyPosture: PrivacyPostureOption = .hostedAllowed
  var routePreviewState: RoutePreviewState = .idle
  var threads: [ConversationThreadSnapshot] = []
  var activeThreadID: UUID?
  var persistenceNotice: String?
  /// The operation a failed persistence path can re-attempt.
  ///
  /// The audited build surfaced nine distinct storage-failure strings through
  /// blocking alerts with no way forward (UX-015). Each failure now carries
  /// the work that failed, so the notice can offer Try Again inline.
  private(set) var persistenceRetry: PersistenceRetryAction?
  private(set) var isRetryingPersistence = false
  /// The most recent meaningful state change, for haptics and VoiceOver.
  private(set) var feedbackEvent: WayfinderFeedbackEvent?
  /// Whether the first-launch chooser has been answered or skipped (UX-013).
  private(set) var hasCompletedFirstRun = true
  var credentialNotice: String?
  var accountNotice: String?
  var openRouterAccountState = ProviderAccountState(
    providerID: "openrouter",
    readiness: .checking
  )
  var appleFoundationModelsAvailability: AppleFoundationModelsAvailability =
    .unsupported
  var modelInventoryNotice: String?
  var isRefreshingModelInventory = false
  var isRestoringConversations = false
  var retentionPolicy: ConversationRetentionPolicy = .forever
  var executionPhase: ChatExecutionPhase = .idle
  var credentialStatuses: [ProviderCredentialStatus] = []
  var destinations: [RoutingDestination]
  var selectedDestinationID: String?

  private let routingEngine: RoutingEngine
  private let conversationStore: any ConversationStore
  private let credentialStore: any CredentialStore
  private let openRouterAccountController: any ProviderAccountController
  private let appleFoundationModelsProvider: any AppleFoundationModelsProvider
  private let providerExecutor: any ProviderExecutor
  private let providerModelCatalog: any ProviderModelCatalog
  private let compiledDestinations: [RoutingDestination]
  private let now: () -> Date
  private var discoveredModelsByProvider: [String: [DiscoveredProviderModel]] = [:]
  private var hasRestoredConversations = false
  private var hasRestoredCredentialStatuses = false
  private var draftSaveTask: Task<Void, Never>?
  private let checkpointPolicy: StreamingCheckpointPolicy
  private let clock = ContinuousClock()
  private var deltasSinceCheckpoint = 0
  private var lastCheckpoint: ContinuousClock.Instant?
  /// Which terminal state an in-flight turn should settle into once its
  /// cancellation lands: the user pressing Stop, or navigating away.
  private var stopReason: ConversationMessageStatus = .stopped
  private var feedbackSequence = 0

  init(
    conversationStore: any ConversationStore = InMemoryConversationStore(),
    credentialStore: any CredentialStore = KeychainCredentialStore(),
    openRouterAccountController: (any ProviderAccountController)? = nil,
    appleFoundationModelsProvider: any AppleFoundationModelsProvider =
      UnavailableAppleFoundationModelsProvider(),
    providerExecutor: any ProviderExecutor = DeterministicMockProvider(),
    providerModelCatalog: any ProviderModelCatalog = EmptyProviderModelCatalog(),
    destinations: [RoutingDestination] = .previewCandidates,
    initialPersistenceNotice: String? = nil,
    checkpointPolicy: StreamingCheckpointPolicy = .standard,
    now: @escaping () -> Date = Date.init
  ) {
    self.checkpointPolicy = checkpointPolicy
    self.conversationStore = conversationStore
    self.credentialStore = credentialStore
    self.openRouterAccountController =
      openRouterAccountController
      ?? AuthorizationCodePKCEController(
        configuration: .openRouter,
        credentialStore: credentialStore
      )
    self.appleFoundationModelsProvider = appleFoundationModelsProvider
    self.providerExecutor = providerExecutor
    self.providerModelCatalog = providerModelCatalog
    compiledDestinations = destinations
    self.destinations = destinations
    self.persistenceNotice = initialPersistenceNotice
    self.now = now

    do {
      routingEngine = try RoutingEngine(
        configuration: RoutingConfiguration(
          tiers: [
            RoutingTier(minScore: 0.0, model: "local"),
            RoutingTier(minScore: 0.1, model: "cloud"),
          ]
        )
      )
    } catch {
      fatalError("The bundled routing configuration is invalid: \(error)")
    }
  }

  var canSendMessage: Bool {
    !executionPhase.isActive
      && !draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
  }

  var activeThread: ConversationThreadSnapshot? {
    guard let activeThreadID else {
      return nil
    }
    return threads.first { $0.id == activeThreadID }
  }

  var configuredCredentialCount: Int {
    credentialStatuses.count(where: \.isConfigured)
  }

  var selectedDestinationName: String {
    guard let selectedDestinationID else {
      return "Automatic"
    }
    return destinations.first(where: { $0.id == selectedDestinationID })?
      .displayName ?? "Destination"
  }

  func isCredentialConfigured(_ id: CredentialID) -> Bool {
    credentialStatuses.first(where: { $0.id == id })?.isConfigured ?? false
  }

  func restoreCredentialStatuses() async {
    guard !hasRestoredCredentialStatuses else {
      return
    }
    hasRestoredCredentialStatuses = await refreshCredentialStatuses()
    if hasRestoredCredentialStatuses {
      openRouterAccountState = await openRouterAccountController.refresh()
    }
    await refreshModelInventory()
  }

  func beginOpenRouterAuthorization() async -> AuthorizationChallenge? {
    do {
      let challenge = try await openRouterAccountController.beginAuthorization()
      openRouterAccountState = await openRouterAccountController.state()
      accountNotice = nil
      return challenge
    } catch {
      accountNotice = userFacingAccountError(error)
      openRouterAccountState = await openRouterAccountController.state()
      return nil
    }
  }

  func completeOpenRouterAuthorization(
    _ authorizationID: UUID,
    callbackURL: URL
  ) async -> Bool {
    do {
      openRouterAccountState = try await openRouterAccountController
        .completeAuthorization(
          authorizationID,
          callbackURL: callbackURL
        )
      accountNotice = nil
      _ = await refreshCredentialStatuses()
      await refreshModelInventory()
      return true
    } catch {
      accountNotice = userFacingAccountError(error)
      openRouterAccountState = await openRouterAccountController.state()
      return false
    }
  }

  func cancelOpenRouterAuthorization(_ authorizationID: UUID) async {
    await openRouterAccountController.cancelAuthorization(authorizationID)
    openRouterAccountState = await openRouterAccountController.state()
  }

  func disconnectOpenRouterAccount() async {
    do {
      try await openRouterAccountController.signOut()
      openRouterAccountState = await openRouterAccountController.state()
      accountNotice = nil
      _ = await refreshCredentialStatuses()
      await refreshModelInventory()
    } catch {
      accountNotice = userFacingAccountError(error)
    }
  }

  func saveAPIKey(
    _ key: String,
    for provider: APIKeyProviderDescriptor
  ) async -> Bool {
    let normalized = key.trimmingCharacters(in: .whitespacesAndNewlines)
    do {
      try await credentialStore.save(secret: normalized, for: provider.id)
      credentialNotice = nil
      _ = await refreshCredentialStatuses()
      if provider.id == OpenAICompatibleConfiguration.openRouter.credentialID {
        openRouterAccountState = await openRouterAccountController.refresh()
      }
      await refreshModelInventory()
      return true
    } catch {
      credentialNotice = userFacingCredentialError(error)
      return false
    }
  }

  func removeAPIKey(for provider: APIKeyProviderDescriptor) async {
    do {
      try await credentialStore.delete(provider.id)
      let remainsConfigured = try await credentialStore.contains(provider.id)
      guard !remainsConfigured else {
        credentialNotice = "Wayfinder could not remove that API key."
        return
      }
      credentialNotice = nil
      _ = await refreshCredentialStatuses()
      if provider.id == OpenAICompatibleConfiguration.openRouter.credentialID {
        openRouterAccountState = await openRouterAccountController.refresh()
      }
      await refreshModelInventory()
    } catch {
      credentialNotice = userFacingCredentialError(error)
    }
  }

  func refreshModelInventory() async {
    guard !isRefreshingModelInventory else {
      return
    }

    isRefreshingModelInventory = true
    defer { isRefreshingModelInventory = false }

    async let appleAvailability = appleFoundationModelsProvider.availability()
    let results = await providerModelCatalog.refresh()
    var failedProviderNames: [String] = []

    for result in results {
      switch result.state {
      case .loaded(let models):
        discoveredModelsByProvider[result.providerID] = models
      case .notConfigured:
        discoveredModelsByProvider[result.providerID] = nil
      case .failed:
        let providerName =
          OpenAICompatibleConfiguration.supported.first {
            $0.providerID == result.providerID
          }?.providerName ?? "A provider"
        failedProviderNames.append(providerName)
      }
    }

    appleFoundationModelsAvailability = await appleAvailability
    rebuildDestinations()
    if failedProviderNames.isEmpty {
      modelInventoryNotice = nil
    } else {
      modelInventoryNotice =
        "Wayfinder could not refresh models for \(failedProviderNames.joined(separator: ", ")). Existing destinations remain available."
    }
  }

  func restoreConversations() async {
    guard !hasRestoredConversations else {
      return
    }

    hasRestoredConversations = true
    isRestoringConversations = true

    let signpost = WayfinderSignposts.conversation.beginInterval(
      "restoreConversations"
    )
    defer {
      WayfinderSignposts.conversation.endInterval(
        "restoreConversations",
        signpost
      )
      isRestoringConversations = false
    }

    do {
      threads = try await conversationStore.listThreads()
      let workspace = try await conversationStore.loadWorkspace()
      retentionPolicy = ConversationRetentionPolicy(
        days: workspace.retentionDays
      )
      hasCompletedFirstRun = workspace.hasCompletedFirstRun ?? false
      activeThreadID = workspace.activeThreadID

      if let activeThread {
        draft = activeThread.draft
        restorePreview(from: activeThread)
      } else {
        activeThreadID = nil
        draft = workspace.draft
      }

      await applyRetentionPolicy()
      await interruptPendingMessages()
    } catch {
      recordPersistenceFailure(
        "Wayfinder could not restore saved conversations. New chats remain available.",
        retry: .restoreConversations
      )
    }
  }

  func sendMessage() async {
    let prompt = draft.trimmingCharacters(in: .whitespacesAndNewlines)
    guard !executionPhase.isActive else {
      return
    }
    guard !prompt.isEmpty else {
      routePreviewState = .unavailable("Enter a message to send.")
      return
    }

    await runTurn(prompt: prompt, regenerating: nil)
  }

  /// Runs one routed turn.
  ///
  /// `regenerating` names an existing assistant message to answer into. Retry
  /// uses it so a second attempt replaces the failed turn where it stands
  /// rather than appending a duplicate exchange (UX-001).
  private func runTurn(
    prompt: String,
    regenerating assistantMessageID: UUID?
  ) async {
    let requestID = UUID()
    executionPhase = .routing(requestID)
    submittedPrompt = prompt
    stopReason = .stopped

    do {
      let routingRequest = RoutingRequest(
        schemaVersion: 1,
        requestId: requestID.uuidString,
        prompt: prompt,
        privacyPosture: privacyPosture.bridgeValue,
        requirements: RoutingRequirements(
          contextTokens: nil,
          imageInput: false,
          tools: false,
          streaming: true
        )
      )
      let plan: RoutePlan
      if let selectedDestinationID,
        let pinnedDestination = destinations.first(where: {
          $0.id == selectedDestinationID
        })
      {
        let pinnedEngine = try RoutingEngine(
          configuration: RoutingConfiguration(
            tiers: [RoutingTier(minScore: 0, model: "pinned")]
          )
        )
        plan = try pinnedEngine.plan(
          request: routingRequest,
          candidates: [pinnedDestination.pinnedBridgeSnapshot]
        )
      } else {
        plan = try routingEngine.plan(
          request: routingRequest,
          candidates: destinations.map(\.bridgeSnapshot)
        )
      }

      guard
        let selectedID = plan.selectedDestinationId,
        let destination = destinations.first(where: { $0.id == selectedID })
      else {
        let message = unavailableDestinationMessage()
        routePreviewState = .unavailable(message)
        await persistFailedTurn(
          prompt: prompt,
          message: message
        )
        executionPhase = .idle
        return
      }

      let preview = RoutePreview(
        destinationID: destination.id,
        destinationName: destination.displayName,
        score: plan.score,
        recommendation: plan.recommendation,
        executionSummary: destination.boundaryLabel
      )
      routePreviewState = .routed(preview)
      let destinationsByID = Dictionary(
        destinations.map { ($0.id, $0.displayName) },
        uniquingKeysWith: { first, _ in first }
      )
      let receipt = StoredRouteReceipt(
        destinationID: destination.id,
        destinationName: destination.displayName,
        score: plan.score,
        recommendation: plan.recommendation,
        executionSummary: destination.boundaryLabel,
        boundaryID: destination.boundary.storageID,
        excluded: plan.exclusions { destinationsByID[$0] },
        fallbackDestinationNames: plan.fallbackDestinationIds.map {
          destinationsByID[$0] ?? $0
        }
      )
      let providerMessages = providerHistory(appending: prompt)
      let turn: TurnSlot
      if let assistantMessageID {
        guard
          let reset = await resetAssistantMessage(
            id: assistantMessageID,
            receipt: receipt
          )
        else {
          executionPhase = .idle
          return
        }
        turn = reset
      } else {
        turn = await beginTurn(prompt: prompt, receipt: receipt)
      }

      emitFeedback(.messageSent)

      if executionPhase == .stopping(requestID) {
        await finishMessage(turn, status: stopReason)
        executionPhase = .idle
        return
      }

      executionPhase = .streaming(requestID)
      beginStreamingCheckpointWindow()
      let stream = await providerExecutor.stream(
        ProviderExecutionRequest(
          id: requestID,
          prompt: prompt,
          destinationID: destination.id,
          messages: providerMessages
        )
      )
      var reachedTerminalEvent = false

      do {
        for try await event in stream {
          guard executionPhase == .streaming(requestID) else {
            break
          }

          switch event {
          case .delta(let delta):
            await appendDelta(delta, to: turn)
          case .completed:
            reachedTerminalEvent = true
            await finishMessage(turn, status: .completed)
          }
        }

        if !reachedTerminalEvent {
          let status: ConversationMessageStatus =
            executionPhase == .stopping(requestID) ? stopReason : .interrupted
          await finishMessage(turn, status: status)
        }
      } catch is CancellationError {
        let status: ConversationMessageStatus =
          executionPhase == .stopping(requestID) ? stopReason : .interrupted
        await finishMessage(turn, status: status)
      } catch {
        await failMessage(
          turn,
          message: userFacingExecutionError(error)
        )
      }
    } catch {
      routePreviewState = .unavailable(
        "Wayfinder could not calculate this route. Try a shorter message."
      )
      await persistFailedTurn(
        prompt: prompt,
        message: "Wayfinder could not calculate a route for this message."
      )
    }

    if executionPhase.requestID == requestID {
      executionPhase = .idle
    }
  }

  func previewRoute() async {
    await sendMessage()
  }

  func stopGenerating() async {
    await interruptActiveTurn(as: .stopped)
  }

  /// Cancels an in-flight turn and states how it should settle.
  ///
  /// Stopping is the user's decision; navigating away is not, so the two are
  /// recorded differently rather than both reading "You stopped this response".
  private func interruptActiveTurn(
    as reason: ConversationMessageStatus
  ) async {
    guard let requestID = executionPhase.requestID else {
      return
    }

    stopReason = reason
    executionPhase = .stopping(requestID)
    await providerExecutor.cancel(requestID: requestID)
  }

  /// Regenerates a failed, stopped, or interrupted reply in place.
  ///
  /// The audited build re-injected the prompt as a brand-new user message, so
  /// the transcript — and the persisted history — showed the user asking twice
  /// (UX-001). The second attempt now replaces the first where it stands, and
  /// the composer draft is left untouched.
  func retry(messageID: UUID) async {
    guard
      !executionPhase.isActive,
      let thread = activeThread,
      let failedIndex = thread.messages.firstIndex(where: {
        $0.id == messageID
          && $0.role == .assistant
          && [.failed, .interrupted, .stopped].contains($0.status)
      }),
      let prompt = thread.messages[..<failedIndex].last(where: {
        $0.role == .user
      })?.content
    else {
      return
    }

    await runTurn(prompt: prompt, regenerating: messageID)
  }

  func clearPreview() {
    routePreviewState = .idle
  }

  func selectDestination(_ id: String?) {
    selectedDestinationID = id
  }

  /// Navigation stays available while a reply streams (UX-008). The in-flight
  /// turn is cancelled and settles as interrupted against its own thread.
  func startNewChat() async {
    await interruptActiveTurn(as: .interrupted)
    await persistActiveDraft()
    draft = ""
    submittedPrompt = nil
    routePreviewState = .idle
    activeThreadID = nil
    selectedTab = .chat
    await persistWorkspace()
  }

  func selectThread(id: UUID) async {
    guard id != activeThreadID else {
      selectedTab = .chat
      return
    }

    await interruptActiveTurn(as: .interrupted)
    await persistActiveDraft()

    do {
      guard let thread = try await conversationStore.thread(id: id) else {
        await refreshThreads()
        return
      }

      activeThreadID = id
      draft = thread.draft
      restorePreview(from: thread)
      selectedTab = .chat
      await persistWorkspace()
    } catch {
      recordPersistenceFailure(
        "Wayfinder could not open that conversation.",
        retry: .openThread(id)
      )
    }
  }

  func saveDraft() async {
    if activeThreadID == nil {
      await persistWorkspace()
    } else {
      await persistActiveDraft()
    }
  }

  func scheduleDraftSave() {
    draftSaveTask?.cancel()
    draftSaveTask = Task { [weak self] in
      try? await Task.sleep(for: .milliseconds(350))
      guard !Task.isCancelled else {
        return
      }
      await self?.saveDraft()
    }
  }

  func setRetentionPolicy(_ policy: ConversationRetentionPolicy) async {
    retentionPolicy = policy
    await persistWorkspace()
    await applyRetentionPolicy()
  }

  func exportConversations() async -> Data? {
    do {
      return try await conversationStore.exportData()
    } catch {
      recordPersistenceFailure(
        "Wayfinder could not prepare the conversation export.",
        retry: .exportConversations
      )
      return nil
    }
  }

  func deleteThread(id: UUID) async {
    do {
      try await conversationStore.deleteThread(id: id)

      if activeThreadID == id {
        activeThreadID = nil
        draft = ""
        submittedPrompt = nil
        routePreviewState = .idle
      }

      await refreshThreads()
      await persistWorkspace()
    } catch {
      recordPersistenceFailure(
        "Wayfinder could not delete that conversation.",
        retry: .deleteThread(id)
      )
    }
  }

  func deleteAllThreads() async {
    do {
      try await conversationStore.deleteAll()
      threads = []
      activeThreadID = nil
      draft = ""
      submittedPrompt = nil
      routePreviewState = .idle
      await persistWorkspace()
    } catch {
      recordPersistenceFailure(
        "Wayfinder could not clear saved conversations.",
        retry: .deleteAllThreads
      )
    }
  }

  /// Identifies the assistant message a running turn writes into.
  ///
  /// Turns address their thread explicitly rather than through `activeThread`,
  /// so a turn still finalises correctly if the user navigates away from the
  /// conversation while it is in flight (UX-008).
  private struct TurnSlot: Equatable {
    let threadID: UUID
    let messageID: UUID
  }

  private func beginTurn(
    prompt: String,
    receipt: StoredRouteReceipt
  ) async -> TurnSlot {
    let timestamp = now()
    let userMessage = ConversationMessageSnapshot(
      id: UUID(),
      role: .user,
      content: prompt,
      createdAt: timestamp,
      status: .completed,
      routeReceipt: nil
    )
    let assistantMessage = ConversationMessageSnapshot(
      id: UUID(),
      role: .assistant,
      content: "",
      createdAt: timestamp,
      status: .pending,
      routeReceipt: receipt
    )

    var thread: ConversationThreadSnapshot
    if let activeThread {
      thread = activeThread
      thread.updatedAt = timestamp
      thread.messages.append(contentsOf: [userMessage, assistantMessage])
      thread.draft = ""
    } else {
      thread = ConversationThreadSnapshot(
        id: UUID(),
        title: ConversationThreadSnapshot.title(for: prompt),
        createdAt: timestamp,
        updatedAt: timestamp,
        messages: [userMessage, assistantMessage],
        draft: ""
      )
      activeThreadID = thread.id
    }

    draft = ""

    do {
      try await conversationStore.save(thread: thread)
      await refreshThreads()
      await persistWorkspace()
    } catch {
      recordPersistenceFailure(
        "This turn is visible now, but Wayfinder could not save it.",
        retry: .saveThread(thread)
      )
      upsertInMemory(thread)
    }

    return TurnSlot(threadID: thread.id, messageID: assistantMessage.id)
  }

  /// Clears an existing assistant message so a retry can answer into its slot.
  private func resetAssistantMessage(
    id messageID: UUID,
    receipt: StoredRouteReceipt
  ) async -> TurnSlot? {
    guard var thread = activeThread,
      let index = thread.messages.firstIndex(where: { $0.id == messageID })
    else {
      return nil
    }

    let message = thread.messages[index]
    thread.messages[index] = ConversationMessageSnapshot(
      id: message.id,
      role: message.role,
      content: "",
      createdAt: message.createdAt,
      status: .pending,
      routeReceipt: receipt
    )
    thread.updatedAt = now()

    let slot = TurnSlot(threadID: thread.id, messageID: messageID)
    await saveUpdatedThread(thread)
    return slot
  }

  private func persistFailedTurn(
    prompt: String,
    message: String
  ) async {
    let timestamp = now()
    let userMessage = ConversationMessageSnapshot(
      id: UUID(),
      role: .user,
      content: prompt,
      createdAt: timestamp,
      status: .completed,
      routeReceipt: nil
    )
    let assistantMessage = ConversationMessageSnapshot(
      id: UUID(),
      role: .assistant,
      content: message,
      createdAt: timestamp,
      status: .failed,
      routeReceipt: nil
    )

    var thread: ConversationThreadSnapshot
    if let activeThread {
      thread = activeThread
      thread.updatedAt = timestamp
      thread.messages.append(contentsOf: [userMessage, assistantMessage])
      thread.draft = ""
    } else {
      thread = ConversationThreadSnapshot(
        id: UUID(),
        title: ConversationThreadSnapshot.title(for: prompt),
        createdAt: timestamp,
        updatedAt: timestamp,
        messages: [userMessage, assistantMessage],
        draft: ""
      )
      activeThreadID = thread.id
    }

    draft = ""
    await saveUpdatedThread(thread)
  }

  /// Applies one streamed delta.
  ///
  /// This is the hot path: it touches memory only. Encoding the thread and
  /// writing it to SwiftData here — as the audited build did — made every
  /// token cost a full-thread re-encode plus two saves (UX-021).
  private func appendDelta(
    _ delta: String,
    to turn: TurnSlot
  ) async {
    guard !delta.isEmpty,
      var thread = thread(id: turn.threadID),
      let index = thread.messages.firstIndex(where: { $0.id == turn.messageID })
    else {
      return
    }

    WayfinderSignposts.streaming.emitEvent("applyDelta")

    let message = thread.messages[index]
    thread.messages[index] = ConversationMessageSnapshot(
      id: message.id,
      role: message.role,
      content: message.content + delta,
      createdAt: message.createdAt,
      status: .streaming,
      routeReceipt: message.routeReceipt
    )
    thread.updatedAt = now()
    upsertInMemory(thread)

    deltasSinceCheckpoint += 1
    if shouldCheckpoint() {
      await checkpoint(thread)
    }
  }

  private func finishMessage(
    _ turn: TurnSlot,
    status: ConversationMessageStatus
  ) async {
    guard var thread = thread(id: turn.threadID),
      let index = thread.messages.firstIndex(where: { $0.id == turn.messageID })
    else {
      return
    }

    let message = thread.messages[index]
    guard message.status == .pending || message.status == .streaming else {
      return
    }

    thread.messages[index] = ConversationMessageSnapshot(
      id: message.id,
      role: message.role,
      content: message.content,
      createdAt: message.createdAt,
      status: status,
      routeReceipt: message.routeReceipt
    )
    thread.updatedAt = now()
    emitFeedback(status == .completed ? .responseCompleted : .responseEnded)
    // Terminal states always reach storage, whatever the checkpoint cadence.
    await saveUpdatedThread(thread)
  }

  private func failMessage(
    _ turn: TurnSlot,
    message failureMessage: String
  ) async {
    guard var thread = thread(id: turn.threadID),
      let index = thread.messages.firstIndex(where: { $0.id == turn.messageID })
    else {
      return
    }

    let message = thread.messages[index]
    thread.messages[index] = ConversationMessageSnapshot(
      id: message.id,
      role: message.role,
      content: message.content.isEmpty ? failureMessage : message.content,
      createdAt: message.createdAt,
      status: .failed,
      routeReceipt: message.routeReceipt
    )
    thread.updatedAt = now()
    emitFeedback(.failure)
    await saveUpdatedThread(thread)
  }

  // MARK: - Streaming checkpoints

  private func beginStreamingCheckpointWindow() {
    deltasSinceCheckpoint = 0
    lastCheckpoint = clock.now
  }

  private func shouldCheckpoint() -> Bool {
    if deltasSinceCheckpoint >= checkpointPolicy.deltaInterval {
      return true
    }
    guard let lastCheckpoint else {
      return true
    }
    return clock.now - lastCheckpoint >= checkpointPolicy.minimumInterval
  }

  /// A mid-stream write. Unlike a terminal save it does not touch the
  /// workspace record, which cannot have changed while a reply is arriving.
  private func checkpoint(_ thread: ConversationThreadSnapshot) async {
    deltasSinceCheckpoint = 0
    lastCheckpoint = clock.now

    let state = WayfinderSignposts.streaming.beginInterval("checkpoint")
    defer { WayfinderSignposts.streaming.endInterval("checkpoint", state) }

    do {
      try await conversationStore.save(thread: thread)
    } catch {
      recordPersistenceFailure(
        "This reply is visible now, but Wayfinder could not save it.",
        retry: .saveThread(thread)
      )
    }
  }

  private func saveUpdatedThread(
    _ thread: ConversationThreadSnapshot
  ) async {
    upsertInMemory(thread)
    deltasSinceCheckpoint = 0
    lastCheckpoint = clock.now

    do {
      try await conversationStore.save(thread: thread)
      await persistWorkspace()
    } catch {
      recordPersistenceFailure(
        "This turn is visible now, but Wayfinder could not save it.",
        retry: .saveThread(thread)
      )
    }
  }

  private func thread(id: UUID) -> ConversationThreadSnapshot? {
    threads.first { $0.id == id }
  }

  func emitFeedback(_ kind: WayfinderFeedbackKind) {
    feedbackSequence += 1
    feedbackEvent = WayfinderFeedbackEvent(
      sequence: feedbackSequence,
      kind: kind
    )
  }

  // MARK: - Persistence recovery

  private func recordPersistenceFailure(
    _ message: String,
    retry: PersistenceRetryAction?
  ) {
    persistenceNotice = message
    persistenceRetry = retry
  }

  func dismissPersistenceNotice() {
    persistenceNotice = nil
    persistenceRetry = nil
  }

  /// Re-attempts the operation that produced the current notice. The notice
  /// clears only if the retry succeeds; otherwise it is replaced by whatever
  /// the second attempt reported.
  func retryPersistence() async {
    guard let action = persistenceRetry, !isRetryingPersistence else {
      return
    }

    isRetryingPersistence = true
    defer { isRetryingPersistence = false }

    persistenceNotice = nil
    persistenceRetry = nil

    switch action {
    case .saveThread(let thread):
      await saveUpdatedThread(thread)
    case .saveDraft:
      await saveDraft()
    case .restoreConversations:
      hasRestoredConversations = false
      await restoreConversations()
    case .openThread(let id):
      await selectThread(id: id)
    case .refreshThreads:
      await refreshThreads()
    case .deleteThread(let id):
      await deleteThread(id: id)
    case .deleteAllThreads:
      await deleteAllThreads()
    case .applyRetention:
      await applyRetentionPolicy()
    case .exportConversations:
      _ = await exportConversations()
    }
  }

  private func interruptPendingMessages() async {
    for var thread in threads {
      var changed = false
      thread.messages = thread.messages.map { message in
        guard message.status == .pending || message.status == .streaming else {
          return message
        }
        changed = true
        return ConversationMessageSnapshot(
          id: message.id,
          role: message.role,
          content: message.content,
          createdAt: message.createdAt,
          status: .interrupted,
          routeReceipt: message.routeReceipt
        )
      }

      if changed {
        thread.updatedAt = now()
        await saveUpdatedThread(thread)
      }
    }
  }

  private func userFacingExecutionError(_ error: Error) -> String {
    if let providerError = error as? ProviderExecutionError,
      let description = providerError.errorDescription
    {
      return description
    }
    return "The provider could not finish this reply."
  }

  private func refreshCredentialStatuses() async -> Bool {
    do {
      var statuses: [ProviderCredentialStatus] = []
      for provider in APIKeyProviderDescriptor.supported {
        statuses.append(
          ProviderCredentialStatus(
            id: provider.id,
            isConfigured: try await credentialStore.contains(provider.id)
          )
        )
      }
      credentialStatuses = statuses
      updateDestinationReadiness()
      credentialNotice = nil
      return true
    } catch {
      credentialNotice = userFacingCredentialError(error)
      return false
    }
  }

  private func userFacingCredentialError(_ error: Error) -> String {
    if let credentialError = error as? CredentialStoreError,
      let description = credentialError.errorDescription
    {
      return description
    }
    return "Wayfinder could not update the iOS Keychain."
  }

  private func userFacingAccountError(_ error: Error) -> String {
    if error is CancellationError {
      return "OpenRouter sign-in was cancelled."
    }
    if let authenticationError = error as? AuthorizationCodePKCEError,
      let description = authenticationError.errorDescription
    {
      return description
    }
    return "Wayfinder could not connect the OpenRouter account."
  }

  private func updateDestinationReadiness() {
    destinations = destinations.map { destination in
      if destination.id == NativeAppleFoundationModelsProvider.destinationID {
        return destination.withAppleAvailability(
          appleFoundationModelsAvailability
        )
      }
      guard let credentialID = destination.credentialID else {
        return destination
      }
      let isConfigured =
        credentialStatuses.first(where: { $0.id == credentialID })?
        .isConfigured ?? false
      return destination.withReadiness(isConfigured ? .ready : .signedOut)
    }
  }

  private func rebuildDestinations() {
    var rebuilt = compiledDestinations
    let compiledModels = Set(
      compiledDestinations.map { "\($0.providerID)\u{0}\($0.modelID)" }
    )

    for configuration in OpenAICompatibleConfiguration.discoverySupported {
      for model in discoveredModelsByProvider[configuration.providerID] ?? [] {
        guard
          !compiledModels.contains(
            "\(configuration.providerID)\u{0}\(model.modelID)"
          )
        else {
          continue
        }
        rebuilt.append(
          RoutingDestination(
            id: "\(configuration.providerID):\(model.modelID)",
            providerID: configuration.providerID,
            providerName: configuration.providerName,
            modelID: model.modelID,
            displayName: model.displayName,
            detail: "Discovered model · API key required",
            routeTier: "cloud",
            boundary: .hosted,
            boundaryLabel: "Hosted cloud",
            billingClass: .apiMetered,
            readiness: .signedOut,
            automaticEligible: false,
            contextWindow: model.contextWindow,
            credentialID: configuration.credentialID
          )
        )
      }
    }

    destinations = rebuilt
    updateDestinationReadiness()
    if let selectedDestinationID,
      !destinations.contains(where: { $0.id == selectedDestinationID })
    {
      self.selectedDestinationID = nil
    }
  }

  private func unavailableDestinationMessage() -> String {
    if let selectedDestinationID,
      let destination = destinations.first(where: {
        $0.id == selectedDestinationID
      })
    {
      if destination.readiness == .signedOut {
        return
          "Connect \(destination.providerName) in Settings before using \(destination.displayName)."
      }
      return "\(destination.displayName) is not currently available under \(privacyPosture.title)."
    }
    return
      "No Automatic destination is eligible under \(privacyPosture.title). Choose a destination or update routing."
  }

  private func providerHistory(
    appending prompt: String
  ) -> [ProviderExecutionMessage] {
    var history: [ProviderExecutionMessage] = []
    var pendingUser: ConversationMessageSnapshot?

    for message in activeThread?.messages ?? [] {
      switch message.role {
      case .system:
        if message.status == .completed {
          history.append(
            ProviderExecutionMessage(
              role: .system,
              content: message.content
            )
          )
        }
      case .user:
        pendingUser = message
      case .assistant:
        defer { pendingUser = nil }
        guard
          message.status == .completed,
          let pendingUser,
          pendingUser.status == .completed
        else {
          continue
        }
        history.append(
          ProviderExecutionMessage(
            role: .user,
            content: pendingUser.content
          )
        )
        history.append(
          ProviderExecutionMessage(
            role: .assistant,
            content: message.content
          )
        )
      }
    }

    history.append(
      ProviderExecutionMessage(role: .user, content: prompt)
    )
    return history
  }

  private func persistActiveDraft() async {
    guard var thread = activeThread else {
      return
    }

    thread.draft = draft
    thread.updatedAt = now()

    do {
      try await conversationStore.save(thread: thread)
      upsertInMemory(thread)
    } catch {
      recordPersistenceFailure(
        "Wayfinder could not save the current draft.",
        retry: .saveDraft
      )
    }
  }

  private func persistWorkspace() async {
    let workspace = ConversationWorkspaceSnapshot(
      activeThreadID: activeThreadID,
      draft: activeThreadID == nil ? draft : "",
      retentionDays: retentionPolicy.days,
      updatedAt: now(),
      hasCompletedFirstRun: hasCompletedFirstRun
    )

    do {
      try await conversationStore.save(workspace: workspace)
    } catch {
      recordPersistenceFailure(
        "Wayfinder could not save the current draft.",
        retry: .saveDraft
      )
    }
  }

  private func refreshThreads() async {
    do {
      threads = try await conversationStore.listThreads()
    } catch {
      recordPersistenceFailure(
        "Wayfinder could not refresh saved conversations.",
        retry: .refreshThreads
      )
    }
  }

  private func applyRetentionPolicy() async {
    guard let days = retentionPolicy.days else {
      return
    }

    let cutoff = now().addingTimeInterval(
      -TimeInterval(days * 24 * 60 * 60)
    )

    do {
      _ = try await conversationStore.pruneThreads(olderThan: cutoff)
      await refreshThreads()

      if let activeThreadID,
        !threads.contains(where: { $0.id == activeThreadID })
      {
        self.activeThreadID = nil
        draft = ""
        submittedPrompt = nil
        routePreviewState = .idle
        await persistWorkspace()
      }
    } catch {
      recordPersistenceFailure(
        "Wayfinder could not apply conversation retention.",
        retry: .applyRetention
      )
    }
  }

  /// Replaces a thread in the in-memory list.
  ///
  /// Sorting on every call was fine when this ran once per turn; it now runs
  /// once per streamed delta. The active thread is almost always already in
  /// its correct position, so the ordered case skips the sort entirely.
  private func upsertInMemory(_ thread: ConversationThreadSnapshot) {
    if let index = threads.firstIndex(where: { $0.id == thread.id }) {
      threads[index] = thread
      if isCorrectlyOrdered(at: index) {
        return
      }
    } else {
      threads.append(thread)
    }

    threads.sort(by: Self.isOrderedBefore)
  }

  private func isCorrectlyOrdered(at index: Int) -> Bool {
    if index > 0, !Self.isOrderedBefore(threads[index - 1], threads[index]) {
      return false
    }
    if index + 1 < threads.count,
      !Self.isOrderedBefore(threads[index], threads[index + 1])
    {
      return false
    }
    return true
  }

  /// Pinned conversations lead, then most-recently-updated. Ties break on id
  /// so ordering stays deterministic.
  private static func isOrderedBefore(
    _ lhs: ConversationThreadSnapshot,
    _ rhs: ConversationThreadSnapshot
  ) -> Bool {
    switch (lhs.pinnedAt, rhs.pinnedAt) {
    case (.some(let left), .some(let right)):
      if left != right {
        return left > right
      }
    case (.some, .none):
      return true
    case (.none, .some):
      return false
    case (.none, .none):
      break
    }

    if lhs.updatedAt == rhs.updatedAt {
      return lhs.id.uuidString < rhs.id.uuidString
    }
    return lhs.updatedAt > rhs.updatedAt
  }

  // MARK: - First launch

  /// Records that the chooser was answered or skipped. Skipping is a
  /// first-class outcome: the roadmap requires a useful no-destination state,
  /// so this never gates the app.
  func completeFirstRun() async {
    guard !hasCompletedFirstRun else {
      return
    }
    hasCompletedFirstRun = true
    await persistWorkspace()
  }

  /// Destinations that could actually run a turn right now. Adding a key had
  /// no visible effect outside the key screens (UX-013); this is what Chat and
  /// the chooser read to say something truthful about readiness.
  var readyDestinations: [RoutingDestination] {
    destinations.filter { $0.readiness == .ready }
  }

  var isAppleOnDeviceAvailable: Bool {
    destinations.contains {
      $0.id == NativeAppleFoundationModelsProvider.destinationID
        && $0.readiness == .ready
    }
  }

  /// One honest line about what the app can currently do.
  var destinationReadinessSummary: String {
    let ready = readyDestinations
    switch ready.count {
    case 0:
      return "No destination is connected yet."
    case 1:
      return "\(ready[0].displayName) is ready."
    default:
      return "\(ready.count) destinations are ready."
    }
  }

  // MARK: - Thread management

  /// Renames a conversation. Titles were derived from the first ~49 characters
  /// of the opening prompt and never revisited (UX-014).
  func renameThread(id: UUID, to title: String) async {
    let trimmed = title.trimmingCharacters(in: .whitespacesAndNewlines)
    guard var thread = thread(id: id), !trimmed.isEmpty else {
      return
    }

    thread.title = String(trimmed.prefix(120))
    thread.hasCustomTitle = true
    upsertInMemory(thread)

    do {
      try await conversationStore.save(thread: thread)
    } catch {
      recordPersistenceFailure(
        "Wayfinder could not rename that conversation.",
        retry: .saveThread(thread)
      )
    }
  }

  func setPinned(_ pinned: Bool, threadID: UUID) async {
    guard var thread = thread(id: threadID) else {
      return
    }

    thread.pinnedAt = pinned ? now() : nil
    upsertInMemory(thread)
    emitFeedback(.selectionBoundary)

    do {
      try await conversationStore.save(thread: thread)
    } catch {
      recordPersistenceFailure(
        "Wayfinder could not update that conversation.",
        retry: .saveThread(thread)
      )
    }
  }

  var pinnedThreads: [ConversationThreadSnapshot] {
    threads.filter(\.isPinned)
  }

  var unpinnedThreads: [ConversationThreadSnapshot] {
    threads.filter { !$0.isPinned }
  }

  /// Case- and diacritic-insensitive match over titles and message text.
  func threads(matching query: String) -> [ConversationThreadSnapshot] {
    let trimmed = query.trimmingCharacters(in: .whitespacesAndNewlines)
    guard !trimmed.isEmpty else {
      return threads
    }
    return threads.filter { thread in
      thread.title.localizedStandardContains(trimmed)
        || thread.messages.contains {
          $0.content.localizedStandardContains(trimmed)
        }
    }
  }

  private func restorePreview(from thread: ConversationThreadSnapshot) {
    guard let userMessage = thread.messages.last(where: { $0.role == .user })
    else {
      submittedPrompt = nil
      routePreviewState = .idle
      return
    }

    submittedPrompt = userMessage.content
    let assistantMessage = thread.messages.last(where: {
      $0.role == .assistant
    })

    if let receipt = assistantMessage?.routeReceipt {
      routePreviewState = .routed(
        RoutePreview(
          destinationID: receipt.destinationID,
          destinationName: receipt.destinationName,
          score: receipt.score,
          recommendation: receipt.recommendation,
          executionSummary: receipt.executionSummary
        )
      )
    } else if assistantMessage?.status == .failed {
      routePreviewState = .unavailable(
        "Wayfinder could not calculate a route for this message."
      )
    } else {
      routePreviewState = .idle
    }
  }
}

struct RoutingDestination: Identifiable, Hashable {
  let id: String
  let providerID: String
  let providerName: String
  let modelID: String
  let displayName: String
  let detail: String
  let routeTier: String
  let boundary: ExecutionBoundary
  let boundaryLabel: String
  let billingClass: BillingClass
  let readiness: ProviderReadiness
  let automaticEligible: Bool
  let contextWindow: UInt64?
  let credentialID: CredentialID?

  var bridgeSnapshot: DestinationSnapshot {
    DestinationSnapshot(
      id: id,
      providerId: providerID,
      modelId: modelID,
      displayName: displayName,
      routeTier: routeTier,
      executionBoundary: boundary,
      readiness: readiness,
      billingClass: billingClass,
      contextWindow: contextWindow,
      capabilities: DestinationCapabilities(
        text: true,
        streaming: true,
        imageInput: false,
        tools: false
      ),
      automaticEligible: automaticEligible
    )
  }

  var pinnedBridgeSnapshot: DestinationSnapshot {
    DestinationSnapshot(
      id: id,
      providerId: providerID,
      modelId: modelID,
      displayName: displayName,
      routeTier: "pinned",
      executionBoundary: boundary,
      readiness: readiness,
      billingClass: billingClass,
      contextWindow: contextWindow,
      capabilities: DestinationCapabilities(
        text: true,
        streaming: true,
        imageInput: false,
        tools: false
      ),
      automaticEligible: true
    )
  }

  func withReadiness(_ readiness: ProviderReadiness) -> Self {
    RoutingDestination(
      id: id,
      providerID: providerID,
      providerName: providerName,
      modelID: modelID,
      displayName: displayName,
      detail: detail,
      routeTier: routeTier,
      boundary: boundary,
      boundaryLabel: boundaryLabel,
      billingClass: billingClass,
      readiness: readiness,
      automaticEligible: automaticEligible,
      contextWindow: contextWindow,
      credentialID: credentialID
    )
  }

  func withAppleAvailability(
    _ availability: AppleFoundationModelsAvailability
  ) -> Self {
    RoutingDestination(
      id: id,
      providerID: providerID,
      providerName: providerName,
      modelID: modelID,
      displayName: displayName,
      detail: availability.detail,
      routeTier: routeTier,
      boundary: boundary,
      boundaryLabel: boundaryLabel,
      billingClass: billingClass,
      readiness: availability.readiness,
      automaticEligible: automaticEligible,
      contextWindow: contextWindow,
      credentialID: credentialID
    )
  }
}

extension [RoutingDestination] {
  static let previewCandidates: [RoutingDestination] = [
    RoutingDestination(
      id: "device-preview",
      providerID: "preview",
      providerName: "Preview",
      modelID: "device-preview",
      displayName: "On-device preview",
      detail: "Deterministic test provider",
      routeTier: "local",
      boundary: .onDevice,
      boundaryLabel: "On this device",
      billingClass: .onDevice,
      readiness: .ready,
      automaticEligible: true,
      contextWindow: 32_768,
      credentialID: nil
    ),
    RoutingDestination(
      id: "hosted-preview",
      providerID: "preview",
      providerName: "Preview",
      modelID: "hosted-preview",
      displayName: "Hosted preview",
      detail: "Deterministic test provider",
      routeTier: "cloud",
      boundary: .hosted,
      boundaryLabel: "Hosted cloud",
      billingClass: .unknown,
      readiness: .ready,
      automaticEligible: true,
      contextWindow: 32_768,
      credentialID: nil
    ),
  ]

  static let liveDirectProviders: [RoutingDestination] = [
    RoutingDestination(
      id: NativeAppleFoundationModelsProvider.destinationID,
      providerID: "apple-foundation-models",
      providerName: "Apple Foundation Models",
      modelID: "system-default",
      displayName: "Apple On-Device",
      detail: "Checking this device…",
      routeTier: "local",
      boundary: .onDevice,
      boundaryLabel: "On this device",
      billingClass: .onDevice,
      readiness: .checking,
      automaticEligible: false,
      contextWindow: nil,
      credentialID: nil
    ),
  ] + [
    OpenAICompatibleConfiguration.openAIPlatform,
    OpenAICompatibleConfiguration.moonshotPlatform,
    OpenAICompatibleConfiguration.openRouter,
    OpenAICompatibleConfiguration.openRouterFree,
  ].map { configuration in
    RoutingDestination(
      id: configuration.destinationID,
      providerID: configuration.providerID,
      providerName: configuration.providerName,
      modelID: configuration.modelID,
      displayName: configuration.displayName,
      detail: configuration.modelID == "openrouter/free"
        ? "No-cost models · rate limited"
        : "Direct API · account or API key",
      routeTier: "cloud",
      boundary: .hosted,
      boundaryLabel: "Hosted cloud",
      billingClass: .apiMetered,
      readiness: .signedOut,
      automaticEligible: false,
      contextWindow: configuration.contextWindow,
      credentialID: configuration.credentialID
    )
  }
}
