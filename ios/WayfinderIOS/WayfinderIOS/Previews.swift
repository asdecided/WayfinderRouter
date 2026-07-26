#if DEBUG

  import SwiftUI
  import WayfinderRoutingBridge

  /// Xcode preview fixtures for the states that decide whether this app reads
  /// as finished.
  ///
  /// The polish pass that produced these screens ran without a Mac, so every
  /// visual judgement in `docs/mobile-fidelity.md` is still unobserved. These
  /// exist so a reviewer opening the project can see each state immediately —
  /// including the ones that are awkward to reach by driving the app, like a
  /// reply that failed halfway through, or the transcript mid-stream.
  ///
  /// Deliberately plain: literal fixtures, no generic helpers, no async setup.
  enum PreviewFixtures {
    static let now = Date(timeIntervalSince1970: 1_780_000_000)

    static let richReply = """
      Here's the plan, in three parts.

      ## Steps

      1. Read the existing file
      2. Change the **one** thing that matters
      3. Leave the rest alone

      Inline `code` stays monospaced, and [links](https://example.com) stay \
      tappable.

      ```swift
      let value = 1
      print(value)
      ```

      | Stage | Cost |
      |:------|-----:|
      | local | 0.01 |
      | cloud | 0.42 |

      > One closing note, quoted.
      """

    static func user(_ text: String) -> ConversationMessageSnapshot {
      ConversationMessageSnapshot(
        id: UUID(),
        role: .user,
        content: text,
        createdAt: now,
        status: .completed,
        routeReceipt: nil
      )
    }

    static func assistant(
      _ text: String,
      status: ConversationMessageStatus,
      receipt: StoredRouteReceipt? = PreviewFixtures.onDeviceReceipt
    ) -> ConversationMessageSnapshot {
      ConversationMessageSnapshot(
        id: UUID(),
        role: .assistant,
        content: text,
        createdAt: now,
        status: status,
        routeReceipt: receipt
      )
    }

    static let onDeviceReceipt = StoredRouteReceipt(
      destinationID: "device-preview",
      destinationName: "On-device preview",
      score: 0.08,
      recommendation: "local",
      executionSummary: "On this device",
      boundaryID: ExecutionBoundary.onDevice.storageID,
      excluded: [
        StoredRouteExclusion(
          destinationName: "Hosted preview",
          reasons: ["blocked by the current privacy posture"]
        )
      ],
      fallbackDestinationNames: ["Hosted preview"]
    )

    static func thread(
      _ messages: [ConversationMessageSnapshot],
      title: String = "Planning a focused workday",
      pinned: Bool = false
    ) -> ConversationThreadSnapshot {
      ConversationThreadSnapshot(
        id: UUID(),
        title: title,
        createdAt: now,
        updatedAt: now,
        messages: messages,
        draft: "",
        pinnedAt: pinned ? now : nil
      )
    }

    /// A model wired to in-memory storage, with state assigned directly so a
    /// preview never has to await a restore.
    @MainActor
    static func model(
      threads: [ConversationThreadSnapshot] = [],
      isRestoring: Bool = false,
      phase: ChatExecutionPhase = .idle,
      notice: String? = nil,
      hasCompletedFirstRun: Bool = true
    ) -> AppModel {
      let model = AppModel(
        conversationStore: InMemoryConversationStore(),
        credentialStore: InMemoryCredentialStore(),
        destinations: .previewCandidates,
        hasCompletedFirstRun: hasCompletedFirstRun,
        now: { now }
      )
      model.threads = threads
      model.activeThreadID = threads.first?.id
      model.isRestoringConversations = isRestoring
      model.executionPhase = phase
      model.persistenceNotice = notice
      return model
    }
  }

  // MARK: - Chat

  #Preview("Chat · empty") {
    ChatTabView()
      .environment(PreviewFixtures.model())
  }

  #Preview("Chat · restoring") {
    ChatTabView()
      .environment(PreviewFixtures.model(isRestoring: true))
  }

  #Preview("Chat · rendered reply") {
    ChatTabView()
      .environment(
        PreviewFixtures.model(threads: [
          PreviewFixtures.thread([
            PreviewFixtures.user("Help me plan a focused workday"),
            PreviewFixtures.assistant(
              PreviewFixtures.richReply,
              status: .completed
            ),
          ])
        ])
      )
  }

  #Preview("Chat · streaming") {
    ChatTabView()
      .environment(
        PreviewFixtures.model(
          threads: [
            PreviewFixtures.thread([
              PreviewFixtures.user("Explain a difficult idea simply"),
              PreviewFixtures.assistant(
                "Here's the plan, in three parts.\n\n## Steps\n\n1. Read the ex",
                status: .streaming
              ),
            ])
          ],
          phase: .streaming(UUID())
        )
      )
  }

  #Preview("Chat · failed reply") {
    ChatTabView()
      .environment(
        PreviewFixtures.model(threads: [
          PreviewFixtures.thread([
            PreviewFixtures.user("Draft a thoughtful reply"),
            PreviewFixtures.assistant(
              "The provider stopped before completing this",
              status: .failed
            ),
          ])
        ])
      )
  }

  #Preview("Chat · storage failure") {
    RootView()
      .environment(
        PreviewFixtures.model(
          threads: [
            PreviewFixtures.thread([
              PreviewFixtures.user("Anything"),
              PreviewFixtures.assistant("A reply.", status: .completed),
            ])
          ],
          notice: "This turn is visible now, but Wayfinder could not save it."
        )
      )
  }

  #Preview("Chat · dark") {
    ChatTabView()
      .environment(
        PreviewFixtures.model(threads: [
          PreviewFixtures.thread([
            PreviewFixtures.user("Help me plan a focused workday"),
            PreviewFixtures.assistant(
              PreviewFixtures.richReply,
              status: .completed
            ),
          ])
        ])
      )
      .preferredColorScheme(.dark)
  }

  /// WF-DESIGN-0020 forbids Dynamic Type hiding send, privacy, or navigation.
  /// This is the size at which the composer stops fitting on one row.
  #Preview("Chat · AX5") {
    // With `openSidebar`, so the navigation control this preview exists to
    // vouch for is actually present.
    ChatTabView(openSidebar: {})
      .environment(
        PreviewFixtures.model(threads: [
          PreviewFixtures.thread([
            PreviewFixtures.user("Help me plan a focused workday"),
            PreviewFixtures.assistant(
              "A short reply, so the composer is what you are looking at.",
              status: .completed
            ),
          ])
        ])
      )
      .environment(\.dynamicTypeSize, .accessibility5)
  }

  // MARK: - Other screens

  #Preview("First launch") {
    FirstLaunchView()
      .environment(PreviewFixtures.model(hasCompletedFirstRun: false))
  }

  #Preview("Threads · populated") {
    NavigationStack {
      ThreadsView()
    }
    .environment(
      PreviewFixtures.model(threads: [
        PreviewFixtures.thread(
          [PreviewFixtures.user("Pinned conversation")],
          title: "Quarterly planning",
          pinned: true
        ),
        PreviewFixtures.thread(
          [PreviewFixtures.user("Another")],
          title: "Explain a difficult idea simply"
        ),
      ])
    )
  }

  #Preview("Threads · empty") {
    NavigationStack {
      ThreadsView()
    }
    .environment(PreviewFixtures.model())
  }

  #Preview("Destinations") {
    NavigationStack {
      DestinationsView()
    }
    .environment(PreviewFixtures.model())
  }

  #Preview("Settings") {
    NavigationStack {
      SettingsView()
    }
    .environment(PreviewFixtures.model())
  }

#endif
