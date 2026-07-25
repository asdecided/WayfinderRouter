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
    // One place maps model state changes to haptics, and the same vocabulary
    // supplies the VoiceOver announcements below.
    .wayfinderFeedback(appModel.feedbackEvent)
    .onChange(of: appModel.feedbackEvent) { _, event in
      guard let announcement = event?.kind.announcement else { return }
      AccessibilityNotification.Announcement(announcement).post()
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
        if messages.isEmpty {
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
      return bottomEdge >= contentBottom - TranscriptScrollState.followThreshold
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

  @ViewBuilder
  private func composerArea(scrollProxy: ScrollViewProxy) -> some View {
    @Bindable var appModel = appModel

    VStack(spacing: WayfinderSpacing.xSmall) {
      // The control lives directly above the composer so it never overlaps
      // the transcript's newest content, and takes no space when following.
      if scroll.showsScrollToBottomControl {
        ScrollToLatestButton {
          returnToLatest(using: scrollProxy)
        }
        .transition(
          reduceMotion
            ? .opacity
            : .opacity.combined(with: .scale(scale: 0.86, anchor: .bottom))
        )
      }

      if let notice = appModel.persistenceNotice {
        InlineNotice(
          message: notice,
          canRetry: appModel.persistenceRetry != nil,
          isRetrying: appModel.isRetryingPersistence,
          retry: { Task { await appModel.retryPersistence() } },
          dismiss: appModel.dismissPersistenceNotice
        )
        .transition(.opacity)
      }

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
        stop: { Task { await appModel.stopGenerating() } }
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
      value: appModel.persistenceNotice,
      reduceMotion: reduceMotion
    )
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
      RoutingModeMenu(posture: appModel.privacyPosture)
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

  var body: some View {
    Menu {
      Section("Automatic routing") {
        Text(
          "Wayfinder scores every message on this device and sends it to the cheapest destination that can handle it."
        )
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
    .accessibilityValue("Automatic")
    .accessibilityHint("Explains how Wayfinder chooses a destination")
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
        .foregroundStyle(hasDestination ? Color.secondary : WayfinderTheme.routeCloud)
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
          Button(suggestion) {
            use(suggestion)
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
private struct InlineNotice: View {
  let message: String
  let canRetry: Bool
  let isRetrying: Bool
  let retry: () -> Void
  let dismiss: () -> Void

  var body: some View {
    HStack(alignment: .top, spacing: WayfinderSpacing.xSmall) {
      Image(systemName: "exclamationmark.triangle.fill")
        .foregroundStyle(WayfinderTheme.routeCloud)
        .accessibilityHidden(true)

      Text(message)
        .font(.footnote)
        .frame(maxWidth: .infinity, alignment: .leading)

      if canRetry {
        Button(action: retry) {
          if isRetrying {
            ProgressView()
          } else {
            Text("Try Again")
          }
        }
        .font(.footnote.weight(.semibold))
        .disabled(isRetrying)
      }

      Button(action: dismiss) {
        Image(systemName: "xmark")
          .font(.caption.weight(.bold))
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
          message.status == .failed ? WayfinderTheme.routeCloud : Color.secondary
        )
    }

    HStack(spacing: WayfinderSpacing.medium) {
      if let receipt = message.routeReceipt {
        RouteReceiptChip(receipt: receipt) {
          showReceipt(receipt)
        }
      }

      if canRetry {
        Button("Retry") {
          retry(message.id)
        }
        .font(.footnote.weight(.semibold))
      }
    }
    .accessibilityHidden(true)
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
  var rotorLabel: String {
    let speaker = role == .user ? "You" : "Wayfinder"
    let preview = content
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
          LabeledContent("Routing tier", value: receipt.recommendation)
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
          Label(
            receipt.destinationID.hasSuffix("-preview")
              ? "This response used the deterministic preview provider. No network request was made."
              : "This response was sent directly from this device to the selected provider.",
            systemImage: "checkmark.shield"
          )
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
}

// MARK: - Composer

private struct ComposerView: View {
  @Environment(\.accessibilityReduceMotion) private var reduceMotion
  @ScaledMetric(relativeTo: .body) private var controlSize: CGFloat =
    WayfinderMetrics.minimumHitTarget

  @Binding var draft: String
  @Binding var privacyPosture: PrivacyPostureOption
  @Binding var selectedDestinationID: String?
  let destinations: [RoutingDestination]
  let canSubmit: Bool
  let isGenerating: Bool
  let submit: () -> Void
  let stop: () -> Void

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

      HStack(spacing: WayfinderSpacing.xSmall) {
        AttachmentAffordance(size: controlSize)
        DestinationLabel(
          selectedDestinationID: $selectedDestinationID,
          destinations: destinations,
          minimumHeight: controlSize
        )

        Spacer(minLength: WayfinderSpacing.xSmall)

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
          .disabled(!canSubmit)
        Button("Stop response", action: stop)
          .keyboardShortcut(.escape, modifiers: [])
          .disabled(!isGenerating)
      }
      .opacity(0)
      .accessibilityHidden(true)
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

private struct DestinationLabel: View {
  @Binding var selectedDestinationID: String?
  let destinations: [RoutingDestination]
  let minimumHeight: CGFloat

  var body: some View {
    Menu {
      Picker("Destination", selection: $selectedDestinationID) {
        Text("Automatic — Wayfinder chooses")
          .tag(nil as String?)
        ForEach(destinations) { destination in
          Text(destination.displayName)
            .tag(Optional(destination.id))
        }
      }
    } label: {
      Label(
        selectedDestinationName,
        systemImage: "point.3.connected.trianglepath.dotted"
      )
      .font(.subheadline.weight(.medium))
      .foregroundStyle(.secondary)
      .frame(minHeight: minimumHeight)
      .contentShape(Rectangle())
    }
    .accessibilityLabel("Destination")
    .accessibilityValue(selectedDestinationName)
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
