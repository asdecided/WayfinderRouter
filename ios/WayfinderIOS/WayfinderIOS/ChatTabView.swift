import SwiftUI
import UIKit
import WayfinderRoutingBridge

struct ChatTabView: View {
  var openSidebar: (() -> Void)?

  var body: some View {
    NavigationStack {
      ChatView(openSidebar: openSidebar)
    }
  }
}

private struct PresentedReceipt: Identifiable {
  let id = UUID()
  let receipt: StoredRouteReceipt
}

struct ChatView: View {
  @Environment(AppModel.self) private var appModel
  @Environment(\.accessibilityReduceMotion) private var reduceMotion
  @FocusState private var composerFocused: Bool
  @State private var presentedReceipt: PresentedReceipt?
  @State private var scroll = TranscriptScrollState()
  @ScaledMetric(relativeTo: .body) private var followThreshold: CGFloat =
    TranscriptScrollState.followThreshold

  var openSidebar: (() -> Void)?

  private var messages: [ConversationMessageSnapshot] {
    appModel.activeThread?.messages ?? []
  }

  var body: some View {
    @Bindable var appModel = appModel

    ScrollViewReader { scrollProxy in
      transcript
        .scrollDismissesKeyboard(.interactively)
        .background(WayfinderTheme.canvas)
        .safeAreaInset(edge: .bottom, spacing: 0) {
          composerArea(scrollProxy: scrollProxy)
            // The pill floats above the composer instead of sitting in its
            // stack. Inside the stack it was part of the bottom inset, so
            // the moment it appeared — which is the moment you scroll up
            // mid-stream — the transcript shifted under your finger.
            .overlay(alignment: .top) {
              scrollToLatestControl(scrollProxy: scrollProxy)
            }
        }
        .onChange(of: messages.last?.content) {
          followTailIfNeeded(using: scrollProxy)
        }
        .onChange(of: messages.count) {
          followTailIfNeeded(using: scrollProxy)
        }
        .onChange(of: appModel.activeThreadID) {
          scroll.conversationChanged()
          scrollToLatest(using: scrollProxy, animated: false)
        }
    }
    .navigationBarTitleDisplayMode(.inline)
    .toolbar { toolbarContent }
    .sheet(item: $presentedReceipt) { presented in
      RouteReceiptSheet(receipt: presented.receipt)
        .presentationDetents([.medium, .large])
        .presentationDragIndicator(.visible)
    }
    .onChange(of: appModel.draft) {
      appModel.scheduleDraftSave()
    }
    .task {
      await appModel.restoreConversations()
    }
  }

  // MARK: - Transcript

  private var transcript: some View {
    ScrollView {
      // Messages are direct children of the LazyVStack: wrapping them in a
      // plain VStack, as the audited build did, built every turn eagerly and
      // defeated the laziness entirely (UX-021).
      LazyVStack(alignment: .leading, spacing: WayfinderSpacing.large) {
        if messages.isEmpty, appModel.isRestoringConversations {
          TranscriptRestoringState()
            .containerRelativeFrame(.vertical) { length, _ in
              max(240, length * 0.55)
            }
        } else if messages.isEmpty {
          ChatEmptyState(
            readiness: appModel.destinationReadinessSummary,
            hasDestination: !appModel.readyDestinations.isEmpty,
            openDestinations: { appModel.selectedTab = .destinations }
          )
          .containerRelativeFrame(.vertical) { length, _ in
            max(240, length * 0.55)
          }
        } else {
          ForEach(messages) { message in
            MessageView(
              message: message,
              isStreaming: message.status == .streaming
                || message.status == .pending,
              showReceipt: { presentedReceipt = PresentedReceipt(receipt: $0) },
              retry: { id in
                Task { await appModel.retry(messageID: id) }
              }
            )
            .id(message.id)
          }
          Color.clear
            .frame(height: 1)
            .id(Self.tailAnchor)
            .accessibilityHidden(true)
        }
      }
      .frame(maxWidth: WayfinderMetrics.readableWidth)
      .frame(maxWidth: .infinity)
      .padding(.horizontal, WayfinderSpacing.medium)
      .padding(.top, WayfinderSpacing.large)
      .padding(.bottom, WayfinderSpacing.medium)
    }
    // Following the tail is conditional on the reader being near it, so
    // scrolling up to re-read is never undone by the next token (UX-002).
    .onScrollGeometryChange(for: Bool.self) { geometry in
      let bottomEdge = geometry.contentOffset.y + geometry.containerSize.height
      let contentBottom =
        geometry.contentSize.height + geometry.contentInsets.bottom
      return bottomEdge >= contentBottom - followThreshold
    } action: { _, isNearBottom in
      scroll.viewportChanged(isNearBottom: isNearBottom)
    }
    .accessibilityRotor("Turns") {
      ForEach(messages) { message in
        AccessibilityRotorEntry(message.rotorLabel, id: message.id)
      }
    }
  }

  private static let tailAnchor = "wayfinder.transcript.tail"

  /// Floats the return-to-latest pill just above the composer without taking
  /// part in its layout. The alignment guide moves the pill's own bottom edge
  /// to the composer's top, so nothing reflows when it appears or leaves.
  @ViewBuilder
  private func scrollToLatestControl(
    scrollProxy: ScrollViewProxy
  ) -> some View {
    if scroll.showsScrollToBottomControl {
      ScrollToLatestButton {
        returnToLatest(using: scrollProxy)
      }
      .alignmentGuide(.top) { dimensions in
        dimensions[.bottom] + WayfinderSpacing.xSmall
      }
      .transition(
        reduceMotion
          ? .opacity
          : .opacity.combined(with: .scale(scale: 0.86, anchor: .bottom))
      )
    }
  }

  @ViewBuilder
  private func composerArea(scrollProxy: ScrollViewProxy) -> some View {
    @Bindable var appModel = appModel

    VStack(spacing: WayfinderSpacing.xSmall) {
      // Suggestions sit directly above the composer, where task entry
      // happens, rather than stranded mid-screen (UX-026).
      if messages.isEmpty, appModel.draft.isEmpty {
        SuggestionRow(use: useSuggestion)
      }

      ComposerView(
        draft: $appModel.draft,
        privacyPosture: $appModel.privacyPosture,
        selectedDestinationID: $appModel.selectedDestinationID,
        destinations: appModel.destinations,
        canSubmit: appModel.canSendMessage,
        isGenerating: appModel.executionPhase.isActive,
        submit: { submit(using: scrollProxy) },
        stop: { Task { await appModel.stopGenerating() } },
        openDestinations: { appModel.selectedTab = .destinations },
        isForeground: appModel.selectedTab == .chat
      )
      .focused($composerFocused)
    }
    .frame(maxWidth: WayfinderMetrics.readableWidth)
    .padding(.horizontal, WayfinderSpacing.small)
    .padding(.vertical, WayfinderSpacing.xSmall)
    .frame(maxWidth: .infinity)
    .background(.bar)
    .wayfinderAnimation(
      WayfinderMotion.reveal,
      value: scroll.showsScrollToBottomControl,
      reduceMotion: reduceMotion
    )
  }

  // MARK: - Toolbar

  @ToolbarContentBuilder
  private var toolbarContent: some ToolbarContent {
    if let openSidebar {
      SidebarToolbarButton(action: openSidebar)
    }

    ToolbarItem(placement: .principal) {
      RoutingModeMenu(
        posture: appModel.privacyPosture,
        hasAutomaticDestination: appModel.hasAutomaticDestination
      )
    }

    ToolbarItemGroup(placement: .topBarTrailing) {
      Button {
        Task {
          await appModel.startNewChat()
          composerFocused = true
        }
      } label: {
        Image(systemName: "square.and.pencil")
      }
      .accessibilityLabel("New chat")
      .keyboardShortcut("n", modifiers: .command)
      .disabled(appModel.selectedTab != .chat)
    }
  }

  // MARK: - Actions

  private func submit(using scrollProxy: ScrollViewProxy) {
    // Submitting is an explicit request to watch the reply arrive, even if
    // the reader had scrolled away.
    scroll.didSubmitTurn()
    Task {
      await appModel.sendMessage()
    }
    scrollToLatest(using: scrollProxy, animated: true)
  }

  private func useSuggestion(_ prompt: String) {
    appModel.draft = prompt
    composerFocused = true
  }

  private func followTailIfNeeded(using scrollProxy: ScrollViewProxy) {
    guard scroll.contentDidGrow() else { return }
    scrollToLatest(using: scrollProxy, animated: true)
  }

  private func returnToLatest(using scrollProxy: ScrollViewProxy) {
    scroll.returnToBottom()
    scrollToLatest(using: scrollProxy, animated: true)
  }

  private func scrollToLatest(
    using scrollProxy: ScrollViewProxy,
    animated: Bool
  ) {
    guard !messages.isEmpty else { return }
    guard animated else {
      scrollProxy.scrollTo(Self.tailAnchor, anchor: .bottom)
      return
    }
    withAnimation(
      WayfinderMotion.resolved(
        WayfinderMotion.transcript,
        reduceMotion: reduceMotion
      )
    ) {
      scrollProxy.scrollTo(Self.tailAnchor, anchor: .bottom)
    }
  }
}

// MARK: - Routing mode

/// The title surface. WF-DESIGN-0020 wants it to expose the Automatic mode;
/// the audited build showed a chevron over a permanently disabled button,
/// which read as a broken model picker (UX-009). It now explains what
/// Automatic means in one sentence — there is still nothing to choose,
/// because the routing decision is the product.
private struct RoutingModeMenu: View {
  let posture: PrivacyPostureOption
  /// Whether any destination is actually enrolled in Automatic. Read from the
  /// destinations rather than asserted: in this release nothing is enrolled,
  /// and describing Automatic as if it were routing was the single largest
  /// untruth in the app.
  let hasAutomaticDestination: Bool

  var body: some View {
    Menu {
      Section("Automatic routing") {
        Text(automaticExplanation)
      }
      Section("Privacy") {
        Text("\(posture.title) — \(posture.boundarySummary).")
      }
    } label: {
      HStack(spacing: WayfinderSpacing.hairline) {
        Text("Wayfinder")
          .font(.headline)
        Image(systemName: "chevron.down")
          .font(.caption2.weight(.bold))
          .foregroundStyle(.secondary)
      }
      .contentShape(Rectangle())
    }
    .accessibilityLabel("Routing mode")
    .accessibilityValue(hasAutomaticDestination ? "Automatic" : "No Automatic destination")
    .accessibilityHint("Explains how Wayfinder chooses a destination")
  }

  private var automaticExplanation: String {
    guard hasAutomaticDestination else {
      return """
        Wayfinder scores every message on this device. No destination is \
        enrolled in Automatic in this release, so choose one in Destinations \
        — the score is still recorded on every receipt.
        """
    }
    return """
      Wayfinder scores every message on this device and sends it to the \
      cheapest destination that can handle it.
      """
  }
}

// MARK: - Empty state

private struct ChatEmptyState: View {
  @ScaledMetric(relativeTo: .largeTitle) private var markSize: CGFloat = 30

  let readiness: String
  let hasDestination: Bool
  let openDestinations: () -> Void

  var body: some View {
    VStack(spacing: WayfinderSpacing.medium) {
      WayfinderMark()
        .font(.system(size: markSize, weight: .semibold))

      Text("What can I help with?")
        .font(.title2.weight(.semibold))
        .multilineTextAlignment(.center)

      Text("Every message is scored on this device before it goes anywhere.")
        .font(.subheadline)
        .foregroundStyle(.secondary)
        .multilineTextAlignment(.center)

      // Saving a key used to change nothing outside the key screens (UX-013).
      // Readiness is stated here, where it decides whether sending will work.
      Button(action: openDestinations) {
        HStack(spacing: WayfinderSpacing.hairline + 2) {
          Image(systemName: hasDestination ? "checkmark.circle" : "exclamationmark.circle")
          Text(readiness)
          if !hasDestination {
            Text("Connect one")
              .fontWeight(.semibold)
          }
        }
        .font(.footnote)
        .foregroundStyle(hasDestination ? Color.secondary : WayfinderTheme.warning)
        .frame(minHeight: WayfinderMetrics.minimumHitTarget)
        .contentShape(Rectangle())
      }
      .buttonStyle(.plain)
      .accessibilityLabel(readiness)
      .accessibilityHint("Opens Destinations")
    }
    .frame(maxWidth: .infinity)
    .padding(.horizontal, WayfinderSpacing.large)
  }
}

/// Progressive reveal while the conversation is restored, rather than an
/// empty state that makes claims about a transcript that has not loaded.
private struct TranscriptRestoringState: View {
  var body: some View {
    VStack(alignment: .leading, spacing: WayfinderSpacing.large) {
      ForEach(0..<3, id: \.self) { index in
        VStack(alignment: index.isMultiple(of: 2) ? .leading : .trailing, spacing: WayfinderSpacing.xSmall) {
          RoundedRectangle(cornerRadius: WayfinderRadius.small)
            .fill(.quaternary)
            .frame(height: 14)
            .frame(maxWidth: index.isMultiple(of: 2) ? .infinity : 200)
          RoundedRectangle(cornerRadius: WayfinderRadius.small)
            .fill(.quaternary)
            .frame(width: index.isMultiple(of: 2) ? 220 : 140, height: 14)
        }
        .frame(maxWidth: .infinity, alignment: index.isMultiple(of: 2) ? .leading : .trailing)
      }
    }
    .padding(.horizontal, WayfinderSpacing.xSmall)
    .accessibilityElement()
    .accessibilityLabel("Restoring conversation")
    .accessibilityAddTraits(.updatesFrequently)
  }
}

private struct SuggestionRow: View {
  let use: (String) -> Void

  // Suggestions stay inside what the deterministic router can actually do:
  // no image generation, no web search, no tools (WF-DESIGN-0020).
  private let suggestions = [
    "Help me plan a focused workday",
    "Explain a difficult idea simply",
    "Draft a thoughtful reply",
  ]

  var body: some View {
    ScrollView(.horizontal, showsIndicators: false) {
      HStack(spacing: WayfinderSpacing.xSmall) {
        ForEach(suggestions, id: \.self) { suggestion in
          Button {
            use(suggestion)
          } label: {
            Text(suggestion)
              .frame(minHeight: WayfinderMetrics.minimumHitTarget)
              .contentShape(Rectangle())
          }
          .buttonStyle(.bordered)
          .buttonBorderShape(.capsule)
          .tint(.primary)
          .font(.subheadline)
        }
      }
      .padding(.horizontal, WayfinderSpacing.hairline)
      .padding(.vertical, 2)
    }
    .scrollClipDisabled()
    .accessibilityLabel("Suggested prompts")
  }
}

// MARK: - Scroll to latest

private struct ScrollToLatestButton: View {
  @ScaledMetric(relativeTo: .footnote) private var diameter: CGFloat =
    WayfinderMetrics.minimumHitTarget

  let action: () -> Void

  var body: some View {
    Button(action: action) {
      Image(systemName: "arrow.down")
        .font(.footnote.weight(.semibold))
        .frame(width: diameter, height: diameter)
        .background(.regularMaterial, in: Circle())
        .overlay {
          Circle().stroke(WayfinderTheme.hairline, lineWidth: 1)
        }
    }
    .buttonStyle(.plain)
    .accessibilityLabel("Scroll to latest")
    .accessibilityHint("Resumes following the reply")
  }
}

// MARK: - Inline notices

/// A recoverable failure, shown in place with a way forward instead of a
/// blocking alert (UX-015).
/// Internal rather than file-private: `RootView` presents it above every
/// section so failures raised from Threads and Settings are not silent.
struct InlineNotice: View {
  let message: String
  let canRetry: Bool
  let isRetrying: Bool
  let retry: () -> Void
  let dismiss: () -> Void

  var body: some View {
    HStack(alignment: .top, spacing: WayfinderSpacing.xSmall) {
      Image(systemName: "exclamationmark.triangle.fill")
        .foregroundStyle(WayfinderTheme.warning)
        .accessibilityHidden(true)

      Text(message)
        .font(.footnote)
        .frame(maxWidth: .infinity, alignment: .leading)

      if canRetry {
        Button(action: retry) {
          Group {
            if isRetrying {
              ProgressView()
            } else {
              Text("Try Again")
            }
          }
          .frame(minHeight: WayfinderMetrics.minimumHitTarget)
          .contentShape(Rectangle())
        }
        .font(.footnote.weight(.semibold))
        .disabled(isRetrying)
        // The label must survive the swap to a spinner.
        .accessibilityLabel("Try again")
        .accessibilityValue(isRetrying ? "Retrying" : "")
      }

      Button(action: dismiss) {
        Image(systemName: "xmark")
          .font(.caption.weight(.bold))
          .frame(
            width: WayfinderMetrics.minimumHitTarget,
            height: WayfinderMetrics.minimumHitTarget
          )
          .contentShape(Rectangle())
      }
      .buttonStyle(.plain)
      .foregroundStyle(.secondary)
      .accessibilityLabel("Dismiss")
    }
    .padding(WayfinderSpacing.small)
    .background(
      WayfinderTheme.raisedSurface,
      in: RoundedRectangle(cornerRadius: WayfinderRadius.small)
    )
    .accessibilityElement(children: .contain)
  }
}

// MARK: - Messages

private struct MessageView: View {
  let message: ConversationMessageSnapshot
  let isStreaming: Bool
  let showReceipt: (StoredRouteReceipt) -> Void
  let retry: (UUID) -> Void

  var body: some View {
    switch message.role {
    case .user:
      UserMessage(message: message)
    case .assistant:
      AssistantMessage(
        message: message,
        isStreaming: isStreaming,
        showReceipt: showReceipt,
        retry: retry
      )
    case .system:
      EmptyView()
    }
  }
}

private struct UserMessage: View {
  let message: ConversationMessageSnapshot

  var body: some View {
    HStack {
      Spacer(minLength: WayfinderSpacing.xxLarge)
      Text(message.content)
        .textSelection(.enabled)
        .padding(.horizontal, WayfinderSpacing.medium)
        .padding(.vertical, WayfinderSpacing.small)
        .background(
          WayfinderTheme.raisedSurface,
          in: RoundedRectangle(cornerRadius: WayfinderRadius.large)
        )
    }
    .frame(maxWidth: .infinity, alignment: .trailing)
    .messageActions(content: message.content, speaker: "You")
    .accessibilityElement(children: .combine)
    .accessibilityLabel("You")
    .accessibilityValue(message.content)
  }
}

private struct AssistantMessage: View {
  @Environment(\.accessibilityReduceMotion) private var reduceMotion
  @State private var document = MarkdownDocument.empty
  @State private var didCopy = false

  let message: ConversationMessageSnapshot
  let isStreaming: Bool
  let showReceipt: (StoredRouteReceipt) -> Void
  let retry: (UUID) -> Void

  var body: some View {
    VStack(alignment: .leading, spacing: WayfinderSpacing.small) {
      content
      footer
    }
    .frame(maxWidth: .infinity, alignment: .leading)
    .onChange(of: message.content, initial: true) { _, content in
      // Reusing the previous parse keeps closed blocks — and therefore their
      // layout — untouched as text arrives.
      document = MarkdownDocument.parse(content, reusing: document)
    }
    .messageActions(content: message.content, speaker: "Wayfinder")
    .accessibilityElement(children: .contain)
    .accessibilityRespondsToUserInteraction(true)
    // The audited build left assistant turns unattributed while user turns
    // announced "You", so VoiceOver read replies as free-floating text.
    .accessibilityLabel("Wayfinder")
    .accessibilityAddTraits(isStreaming ? .updatesFrequently : [])
    .accessibilityAction(named: "Copy reply") {
      UIPasteboard.general.string = message.content
    }
    .accessibilityActions {
      if let receipt = message.routeReceipt {
        Button("Routing details") { showReceipt(receipt) }
      }
      if canRetry {
        Button("Retry") { retry(message.id) }
      }
    }
  }

  @ViewBuilder
  private var content: some View {
    if message.content.isEmpty, isStreaming {
      HStack(spacing: WayfinderSpacing.xSmall) {
        ProgressView()
        Text("Wayfinder is responding…")
          .foregroundStyle(.secondary)
      }
      .accessibilityElement(children: .combine)
    } else if message.content.isEmpty {
      Text(statusDescription)
        .foregroundStyle(.secondary)
    } else if document.isEmpty {
      // The parse lands in `onChange`, one body pass behind the content.
      // Without this the spinner disappears into nothing for a frame.
      Text(message.content)
        .textSelection(.enabled)
    } else {
      MarkdownTextView(document: document, isStreaming: isStreaming)
    }
  }

  @ViewBuilder
  private var footer: some View {
    if message.status != .completed, !isStreaming {
      Label(statusDescription, systemImage: statusImage)
        .font(.footnote.weight(.medium))
        .foregroundStyle(
          message.status == .failed ? WayfinderTheme.warning : Color.secondary
        )
        .accessibilityElement(children: .combine)
        .accessibilityLabel(statusDescription)
    }

    HStack(spacing: WayfinderSpacing.medium) {
      if let receipt = message.routeReceipt {
        RouteReceiptChip(receipt: receipt) {
          showReceipt(receipt)
        }
      }

      if canRetry {
        Button {
          retry(message.id)
        } label: {
          // The frame and shape belong inside the label: applied outside the
          // Button they enlarge the layout slot and centre the control in it,
          // leaving the tap target the size of the text.
          Text("Retry")
            .font(.footnote.weight(.semibold))
            .frame(minHeight: WayfinderMetrics.minimumHitTarget)
            .contentShape(Rectangle())
        }
        .accessibilityHint("Regenerates this reply in place")
      }

      // Copy needs a real control, not only a context menu on a container
      // VoiceOver does not stop on.
      Button {
        UIPasteboard.general.string = message.content
        didCopy = true
      } label: {
        Label(didCopy ? "Copied" : "Copy", systemImage: didCopy ? "checkmark" : "doc.on.doc")
          .font(.footnote.weight(.medium))
          .labelStyle(.titleAndIcon)
          .frame(minHeight: WayfinderMetrics.minimumHitTarget)
          .contentShape(Rectangle())
      }
      .buttonStyle(.plain)
      .foregroundStyle(didCopy ? WayfinderTheme.accent : Color.secondary)
      .wayfinderFeedback(.copied, trigger: didCopy) { _, copied in copied }
      .accessibilityLabel(didCopy ? "Reply copied" : "Copy reply")
    }
  }

  private var canRetry: Bool {
    [.failed, .interrupted, .stopped].contains(message.status)
  }

  private var statusDescription: String {
    message.status.transcriptDescription
  }

  private var statusImage: String {
    switch message.status {
    case .failed: "exclamationmark.triangle.fill"
    case .stopped: "stop.circle"
    case .interrupted: "arrow.clockwise.circle"
    case .pending, .streaming, .completed: "checkmark.circle"
    }
  }
}

extension ConversationMessageStatus {
  var transcriptDescription: String {
    switch self {
    case .pending, .streaming: "Responding"
    case .completed: "Completed"
    case .stopped: "You stopped this response"
    case .interrupted: "This response was interrupted"
    case .failed: "Reply failed"
    }
  }
}

extension ConversationMessageSnapshot {
  /// Label used by the transcript's turn rotor (UX-027).
  ///
  /// The rotor builder is eager over every message, and the transcript's body
  /// re-evaluates on each delta, so this must not walk the whole reply.
  /// Bounding the slice first keeps it O(1) per turn instead of
  /// O(transcript length) per token.
  var rotorLabel: String {
    let speaker = role == .user ? "You" : "Wayfinder"
    let preview = content.prefix(64)
      .split(whereSeparator: \.isWhitespace)
      .prefix(8)
      .joined(separator: " ")
    return preview.isEmpty
      ? "\(speaker), \(status.transcriptDescription)"
      : "\(speaker): \(preview)"
  }
}

// MARK: - Message actions

extension View {
  /// Copy, share, and selection on every turn, for both roles (UX-007).
  fileprivate func messageActions(
    content: String,
    speaker: String
  ) -> some View {
    modifier(MessageActions(content: content, speaker: speaker))
  }
}

private struct MessageActions: ViewModifier {
  let content: String
  let speaker: String

  @State private var didCopy = false

  func body(content view: Content) -> some View {
    view
      .contextMenu {
        Button {
          UIPasteboard.general.string = content
          didCopy = true
        } label: {
          Label("Copy", systemImage: "doc.on.doc")
        }

        ShareLink(item: content) {
          Label("Share", systemImage: "square.and.arrow.up")
        }
      }
      .wayfinderFeedback(.copied, trigger: didCopy) { _, copied in copied }
      .onChange(of: didCopy) { _, copied in
        guard copied else { return }
        AccessibilityNotification.Announcement("Copied").post()
        didCopy = false
      }
  }
}

// MARK: - Receipts

private struct RouteReceiptChip: View {
  let receipt: StoredRouteReceipt
  let action: () -> Void

  private var boundary: ExecutionBoundary {
    receipt.boundary ?? .hosted
  }

  var body: some View {
    Button(action: action) {
      HStack(spacing: WayfinderSpacing.hairline + 2) {
        Image(systemName: boundary.routeSymbolName)
          .foregroundStyle(boundary.routeColor)
        Text(receipt.receiptSummary)
          .fontWeight(.medium)
        Image(systemName: "info.circle")
          .foregroundStyle(.secondary)
      }
      .font(.footnote)
      .frame(minHeight: WayfinderMetrics.minimumHitTarget)
      .contentShape(Rectangle())
    }
    .buttonStyle(.plain)
    .accessibilityLabel(receipt.receiptSummary)
    .accessibilityHint("Shows routing details")
  }
}

private struct RouteReceiptSheet: View {
  @Environment(\.dismiss) private var dismiss
  let receipt: StoredRouteReceipt

  var body: some View {
    NavigationStack {
      List {
        Section {
          // One reading unit, per the contract's receipt-row rule.
          VStack(alignment: .leading, spacing: WayfinderSpacing.hairline) {
            Label {
              Text(receipt.receiptSummary)
                .font(.headline)
            } icon: {
              Image(systemName: boundary.routeSymbolName)
                .foregroundStyle(boundary.routeColor)
            }
            Text(receipt.scoreExplanation)
              .font(.subheadline)
              .foregroundStyle(.secondary)
          }
          .padding(.vertical, WayfinderSpacing.hairline)
          .accessibilityElement(children: .combine)
        }

        Section("Details") {
          LabeledContent("Destination", value: receipt.destinationName)
          LabeledContent("Execution", value: receipt.executionSummary)
          LabeledContent("Routing tier", value: receipt.tierDescription)
          LabeledContent(
            "Score",
            value: receipt.score.formatted(.number.precision(.fractionLength(2)))
          )
        }

        if let excluded = receipt.excluded, !excluded.isEmpty {
          Section {
            ForEach(excluded, id: \.destinationName) { exclusion in
              VStack(alignment: .leading, spacing: 2) {
                Text(exclusion.destinationName)
                  .font(.subheadline.weight(.medium))
                Text(exclusion.reasons.joined(separator: ", "))
                  .font(.footnote)
                  .foregroundStyle(.secondary)
              }
              .accessibilityElement(children: .combine)
            }
          } header: {
            Text("Not eligible")
          } footer: {
            Text("Eligibility is decided before scoring.")
          }
        }

        if let fallbacks = receipt.fallbackDestinationNames, !fallbacks.isEmpty {
          Section {
            ForEach(fallbacks, id: \.self) { name in
              Text(name)
            }
          } header: {
            Text("Would have fallen back to")
          } footer: {
            Text(
              "Automatic only falls back inside the configured route and privacy posture, and only before reply content begins."
            )
          }
        }

        Section {
          // Branched on a "-preview" ID suffix until the gauntlet found that
          // no shipping destination carries one, so every real receipt —
          // including an Apple On-Device run under On-Device Only — claimed
          // a send that never happened. The execution boundary is the only
          // thing that actually knows, so it decides.
          Label(egressStatement, systemImage: "checkmark.shield")
            .font(.footnote)
            .foregroundStyle(.secondary)
        } header: {
          Text("This build slice")
        }
      }
      .navigationTitle("Routing details")
      .navigationBarTitleDisplayMode(.inline)
      .toolbar {
        ToolbarItem(placement: .confirmationAction) {
          Button("Done") {
            dismiss()
          }
        }
      }
    }
  }

  private var boundary: ExecutionBoundary {
    receipt.boundary ?? .hosted
  }

  /// What actually crossed the device edge, stated per boundary. The
  /// on-device case must never assert a send: under On-Device Only that
  /// sentence would be the one falsehood the posture exists to rule out.
  private var egressStatement: String {
    switch boundary {
    case .onDevice:
      "This response was produced on this device. No network request was made."
    case .localNetwork:
      "This response was sent to a device on your local network, not to a hosted provider."
    case .hosted:
      "This response was sent directly from this device to the provider named above, with no Wayfinder server in between."
    }
  }
}

// MARK: - Composer

private struct ComposerView: View {
  @Environment(\.accessibilityReduceMotion) private var reduceMotion
  /// 44 pt is a *minimum*, not a metric to scale: multiplying it by the
  /// Dynamic Type factor produced a 137 pt control at AX5, which is what
  /// forced the row to wrap in the first place. Glyph controls stay at the
  /// floor and grow only modestly; the text label beside them scales freely.
  @ScaledMetric(relativeTo: .body) private var scaledControl: CGFloat =
    WayfinderMetrics.minimumHitTarget
  private var controlSize: CGFloat {
    min(scaledControl, WayfinderMetrics.maximumControlSize)
  }

  @Environment(\.dynamicTypeSize) private var dynamicTypeSize

  @Binding var draft: String
  @Binding var privacyPosture: PrivacyPostureOption
  @Binding var selectedDestinationID: String?
  let destinations: [RoutingDestination]
  let canSubmit: Bool
  let isGenerating: Bool
  let submit: () -> Void
  let stop: () -> Void
  let openDestinations: () -> Void
  /// Every section stays mounted to preserve its navigation stack, so the
  /// composer's shortcuts must not fire from Settings or Threads.
  var isForeground = true

  var body: some View {
    VStack(alignment: .leading, spacing: WayfinderSpacing.xSmall) {
      // Never disabled. The audited build blocked typing for the whole reply,
      // which is worse than either reference app (UX-008).
      TextField("Message Wayfinder", text: $draft, axis: .vertical)
        .lineLimit(1...8)
        .textFieldStyle(.plain)
        .font(.body)
        .accessibilityLabel("Message Wayfinder")
        .submitLabel(.send)
        .onSubmit(submitIfPossible)

      // At accessibility sizes each control scales from the 44 pt floor, so
      // five of them plus a label cannot share one row on any iPhone width.
      // WF-DESIGN-0020 forbids hiding send, privacy, or navigation, so the
      // row wraps rather than truncating.
      // `.xxxLarge` is not an accessibility size but already overflows on a
      // narrow device, so the wrap is keyed on width need, not on the
      // accessibility flag.
      if dynamicTypeSize >= .xxLarge {
        VStack(alignment: .leading, spacing: WayfinderSpacing.xSmall) {
          routingControls
          actionControls
            .frame(maxWidth: .infinity, alignment: .trailing)
        }
      } else {
        HStack(spacing: WayfinderSpacing.xSmall) {
          routingControls
          Spacer(minLength: WayfinderSpacing.xSmall)
          actionControls
        }
      }
    }
    .padding(.horizontal, WayfinderSpacing.small)
    .padding(.vertical, WayfinderSpacing.small)
    .background(
      WayfinderTheme.raisedSurface,
      in: RoundedRectangle(cornerRadius: WayfinderRadius.composer)
    )
    .overlay {
      RoundedRectangle(cornerRadius: WayfinderRadius.composer)
        .stroke(WayfinderTheme.hairline, lineWidth: 1)
    }
    .shadow(color: WayfinderTheme.shadow, radius: 12, y: 4)
    .wayfinderAnimation(
      WayfinderMotion.control,
      value: isGenerating,
      reduceMotion: reduceMotion
    )
    .background {
      // Hardware-keyboard shortcuts for iPad, kept off-screen so they do not
      // add visible controls (UX-004).
      Group {
        Button("Send message", action: submitIfPossible)
          .keyboardShortcut(.return, modifiers: .command)
          .disabled(!canSubmit || !isForeground)
        Button("Stop response", action: stop)
          .keyboardShortcut(.escape, modifiers: [])
          .disabled(!isGenerating || !isForeground)
      }
      .opacity(0)
      .accessibilityHidden(true)
    }
  }

  private var routingControls: some View {
    HStack(spacing: WayfinderSpacing.xSmall) {
      AttachmentAffordance(size: controlSize)
      RouteLabel(
        selectedDestinationID: $selectedDestinationID,
        destinations: destinations,
        minimumHeight: controlSize,
        openDestinations: openDestinations
      )
    }
  }

  private var actionControls: some View {
    HStack(spacing: WayfinderSpacing.xSmall) {
      PrivacyMenu(posture: $privacyPosture, size: controlSize)
      SendButton(
        isGenerating: isGenerating,
        canSubmit: canSubmit,
        size: controlSize,
        submit: submit,
        stop: stop
      )
    }
  }

  private func submitIfPossible() {
    guard canSubmit else { return }
    submit()
  }
}

/// The attachment affordance the contract permits only when its unavailable
/// state is explicit. A menu whose single item was disabled read as broken, so
/// this is an inert, labelled control instead (UX-024).
private struct AttachmentAffordance: View {
  let size: CGFloat

  var body: some View {
    Image(systemName: "paperclip")
      .font(.subheadline)
      .foregroundStyle(.tertiary)
      .frame(width: size, height: size)
      .contentShape(Rectangle())
      .accessibilityElement()
      .accessibilityLabel("Attachments")
      .accessibilityValue("Unavailable")
      .accessibilityHint("Wayfinder cannot send attachments in this build")
      .accessibilityAddTraits(.isStaticText)
  }
}

/// The composer's route label.
///
/// This was a `Picker` over destination display names until the gauntlet
/// caught it: a list of model names in the composer is a model picker, in the
/// same screen position the reference apps put theirs, and the product's
/// first guardrail forbids one outright. WF-DESIGN-0020 asks for "a compact
/// Automatic route label" here — a readout of where the next message will go.
///
/// The two actions it offers are not model choices. Returning to Automatic
/// removes a pin rather than picking anything, and the second is navigation
/// to Destinations, which owns explicit selection and applies the readiness
/// rules — disabling what has no key — that a bare name list cannot.
private struct RouteLabel: View {
  @Binding var selectedDestinationID: String?
  let destinations: [RoutingDestination]
  let minimumHeight: CGFloat
  let openDestinations: () -> Void

  var body: some View {
    Menu {
      if selectedDestinationID != nil {
        Button("Use Automatic Routing") {
          selectedDestinationID = nil
        }
      }
      Button("Choose in Destinations…", action: openDestinations)
    } label: {
      Label(
        selectedDestinationName,
        systemImage: selectedDestinationID == nil
          ? "point.3.connected.trianglepath.dotted"
          : "pin.fill"
      )
      .font(.subheadline.weight(.medium))
      .foregroundStyle(.secondary)
      .frame(minHeight: minimumHeight)
      .contentShape(Rectangle())
    }
    .accessibilityLabel("Route")
    .accessibilityValue(
      selectedDestinationID == nil
        ? "Automatic"
        : "Pinned to \(selectedDestinationName)"
    )
  }

  private var selectedDestinationName: String {
    guard let selectedDestinationID else {
      return "Automatic"
    }
    return destinations.first(where: { $0.id == selectedDestinationID })?
      .displayName ?? "Destination"
  }
}

/// Privacy posture, with the consequence of each choice stated where the
/// choice is made rather than one line deep in Settings (UX-018).
private struct PrivacyMenu: View {
  @Binding var posture: PrivacyPostureOption
  let size: CGFloat

  var body: some View {
    Menu {
      Picker("Privacy", selection: $posture) {
        ForEach(PrivacyPostureOption.allCases) { option in
          Text(option.title).tag(option)
        }
      }
      Section {
        Text(posture.consequence)
      }
    } label: {
      Image(systemName: posture.symbolName)
        .font(.subheadline)
        .frame(width: size, height: size)
        .contentShape(Rectangle())
    }
    .accessibilityLabel("Privacy")
    .accessibilityValue("\(posture.title). \(posture.consequence)")
  }
}

extension PrivacyPostureOption {
  var symbolName: String {
    switch self {
    case .onDeviceOnly: "hand.raised.fill"
    case .localDevices: "hand.raised"
    case .hostedAllowed: "hand.raised.slash"
    }
  }

  /// What this posture actually does, in plain language.
  var consequence: String {
    switch self {
    case .onDeviceOnly:
      "Messages never leave this device. Hosted destinations are excluded from routing, so replies may be unavailable."
    case .localDevices:
      "Messages may reach trusted devices on your local network, but never a hosted provider."
    case .hostedAllowed:
      "Messages may be sent to hosted providers when Wayfinder judges them the best destination."
    }
  }
}

private struct SendButton: View {
  @Environment(\.accessibilityReduceMotion) private var reduceMotion

  let isGenerating: Bool
  let canSubmit: Bool
  let size: CGFloat
  let submit: () -> Void
  let stop: () -> Void

  var body: some View {
    Button(action: isGenerating ? stop : submit) {
      Image(systemName: isGenerating ? "stop.fill" : "arrow.up")
        .font(.subheadline.weight(.bold))
        .foregroundStyle(
          canSubmit || isGenerating ? WayfinderTheme.onAccent : Color.secondary
        )
        .frame(width: size, height: size)
        .background(
          canSubmit || isGenerating
            ? WayfinderTheme.accent
            : WayfinderTheme.controlSurface,
          in: Circle()
        )
        // The glyph swap is a state change and reads as one.
        .contentTransition(
          reduceMotion ? .opacity : .symbolEffect(.replace)
        )
    }
    .buttonStyle(.plain)
    .disabled(!canSubmit && !isGenerating)
    .accessibilityLabel(isGenerating ? "Stop response" : "Send message")
  }
}
