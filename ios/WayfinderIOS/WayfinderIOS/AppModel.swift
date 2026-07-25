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
    now: @escaping () -> Date = Date.init
  ) {
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
    defer { isRestoringConversations = false }

    do {
      threads = try await conversationStore.listThreads()
      let workspace = try await conversationStore.loadWorkspace()
      retentionPolicy = ConversationRetentionPolicy(
        days: workspace.retentionDays
      )
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
      persistenceNotice =
        "Wayfinder could not restore saved conversations. New chats remain available."
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

    let requestID = UUID()
    executionPhase = .routing(requestID)
    submittedPrompt = prompt

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
      let receipt = StoredRouteReceipt(
        destinationID: destination.id,
        destinationName: destination.displayName,
        score: plan.score,
        recommendation: plan.recommendation,
        executionSummary: destination.boundaryLabel
      )
      let providerMessages = providerHistory(appending: prompt)
      let assistantMessageID = await beginTurn(
        prompt: prompt,
        receipt: receipt
      )

      if executionPhase == .stopping(requestID) {
        await finishMessage(id: assistantMessageID, status: .stopped)
        executionPhase = .idle
        return
      }

      executionPhase = .streaming(requestID)
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
            await appendDelta(delta, to: assistantMessageID)
          case .completed:
            reachedTerminalEvent = true
            await finishMessage(
              id: assistantMessageID,
              status: .completed
            )
          }
        }

        if !reachedTerminalEvent {
          let status: ConversationMessageStatus =
            executionPhase == .stopping(requestID) ? .stopped : .interrupted
          await finishMessage(id: assistantMessageID, status: status)
        }
      } catch is CancellationError {
        let status: ConversationMessageStatus =
          executionPhase == .stopping(requestID) ? .stopped : .interrupted
        await finishMessage(id: assistantMessageID, status: status)
      } catch {
        await failMessage(
          id: assistantMessageID,
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
    guard let requestID = executionPhase.requestID else {
      return
    }

    executionPhase = .stopping(requestID)
    await providerExecutor.cancel(requestID: requestID)
  }

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

    draft = prompt
    await sendMessage()
  }

  func clearPreview() {
    routePreviewState = .idle
  }

  func selectDestination(_ id: String?) {
    selectedDestinationID = id
  }

  func startNewChat() async {
    guard !executionPhase.isActive else {
      return
    }

    await persistActiveDraft()
    draft = ""
    submittedPrompt = nil
    routePreviewState = .idle
    activeThreadID = nil
    selectedTab = .chat
    await persistWorkspace()
  }

  func selectThread(id: UUID) async {
    guard !executionPhase.isActive else {
      return
    }

    guard id != activeThreadID else {
      selectedTab = .chat
      return
    }

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
      persistenceNotice = "Wayfinder could not open that conversation."
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
      persistenceNotice = "Wayfinder could not prepare the conversation export."
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
      persistenceNotice = "Wayfinder could not delete that conversation."
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
      persistenceNotice = "Wayfinder could not clear saved conversations."
    }
  }

  private func beginTurn(
    prompt: String,
    receipt: StoredRouteReceipt
  ) async -> UUID {
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
      persistenceNotice =
        "This turn is visible now, but Wayfinder could not save it."
      upsertInMemory(thread)
    }

    return assistantMessage.id
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

  private func appendDelta(
    _ delta: String,
    to messageID: UUID
  ) async {
    guard !delta.isEmpty, var thread = activeThread,
      let index = thread.messages.firstIndex(where: { $0.id == messageID })
    else {
      return
    }

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
    await saveUpdatedThread(thread)
  }

  private func finishMessage(
    id messageID: UUID,
    status: ConversationMessageStatus
  ) async {
    guard var thread = activeThread,
      let index = thread.messages.firstIndex(where: { $0.id == messageID })
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
    await saveUpdatedThread(thread)
  }

  private func failMessage(
    id messageID: UUID,
    message failureMessage: String
  ) async {
    guard var thread = activeThread,
      let index = thread.messages.firstIndex(where: { $0.id == messageID })
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
    await saveUpdatedThread(thread)
  }

  private func saveUpdatedThread(
    _ thread: ConversationThreadSnapshot
  ) async {
    upsertInMemory(thread)

    do {
      try await conversationStore.save(thread: thread)
      await persistWorkspace()
    } catch {
      persistenceNotice =
        "This turn is visible now, but Wayfinder could not save it."
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
      persistenceNotice = "Wayfinder could not save the current draft."
    }
  }

  private func persistWorkspace() async {
    let workspace = ConversationWorkspaceSnapshot(
      activeThreadID: activeThreadID,
      draft: activeThreadID == nil ? draft : "",
      retentionDays: retentionPolicy.days,
      updatedAt: now()
    )

    do {
      try await conversationStore.save(workspace: workspace)
    } catch {
      persistenceNotice = "Wayfinder could not save the current draft."
    }
  }

  private func refreshThreads() async {
    do {
      threads = try await conversationStore.listThreads()
    } catch {
      persistenceNotice = "Wayfinder could not refresh saved conversations."
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
      persistenceNotice = "Wayfinder could not apply conversation retention."
    }
  }

  private func upsertInMemory(_ thread: ConversationThreadSnapshot) {
    threads.removeAll { $0.id == thread.id }
    threads.append(thread)
    threads.sort {
      if $0.updatedAt == $1.updatedAt {
        return $0.id.uuidString < $1.id.uuidString
      }
      return $0.updatedAt > $1.updatedAt
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
