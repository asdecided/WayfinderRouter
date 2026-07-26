import SwiftUI

struct RootView: View {
  @Environment(AppModel.self) private var appModel
  @Environment(\.horizontalSizeClass) private var horizontalSizeClass
  @Environment(\.scenePhase) private var scenePhase
  @Environment(\.accessibilityReduceMotion) private var reduceMotion
  @State private var showsSidebar = false
  /// Live drag offset, in points, while the drawer is being dragged.
  @State private var dragTranslation: CGFloat = 0

  // This file is written deliberately plainly — explicit types, no generic
  // view helpers, no key-path-as-function. Several rounds of CI were spent on
  // "failed to produce diagnostic" errors here, which name only the outermost
  // expression and give nothing to bisect against without a local compiler.

  var body: some View {
    layout
      .safeAreaInset(edge: .top, spacing: 0) { persistenceNoticeBar }
      .modifier(RootFeedback(event: appModel.feedbackEvent))
      .fullScreenCover(isPresented: firstRunBinding) { FirstLaunchView() }
      .onChange(of: scenePhase) { _, phase in
        guard phase != .active else { return }
        Task { await appModel.saveDraft() }
      }
  }

  @ViewBuilder
  private var layout: some View {
    if horizontalSizeClass == .regular {
      regularWidthLayout
    } else {
      compactWidthLayout
    }
  }

  /// Storage failures are raised from Threads and Settings too, so the notice
  /// cannot live inside Chat or those failures are silent.
  @ViewBuilder
  private var persistenceNoticeBar: some View {
    if let notice = appModel.persistenceNotice {
      InlineNotice(
        message: notice,
        canRetry: appModel.persistenceRetry != nil,
        isRetrying: appModel.isRetryingPersistence,
        retry: { Task { await appModel.retryPersistence() } },
        dismiss: { appModel.dismissPersistenceNotice() }
      )
      .padding(.horizontal, WayfinderSpacing.small)
      .padding(.bottom, WayfinderSpacing.xSmall)
      .background(.bar)
      .transition(.opacity)
    }
  }

  private var compactWidthLayout: some View {
    GeometryReader { proxy in
      CompactShell(
        width: min(340, proxy.size.width * 0.86),
        showsSidebar: $showsSidebar,
        dragTranslation: $dragTranslation,
        reduceMotion: reduceMotion,
        sections: sectionStack(canOpenDrawer: true),
        select: select
      )
    }
  }

  private var regularWidthLayout: some View {
    NavigationSplitView {
      AppSidebarView(select: select, close: nil)
        .navigationSplitViewColumnWidth(min: 260, ideal: 300, max: 340)
    } detail: {
      sectionStack(canOpenDrawer: false)
    }
    .navigationSplitViewStyle(.balanced)
  }

  /// Every section keeps its own `NavigationStack`, all of them alive, so
  /// pushed detail survives a round trip through the drawer. WF-DESIGN-0020
  /// permits this hidden-container mechanism; WF-ROADMAP-0016 and
  /// WF-ADR-0047 require the state to be preserved (UX-020).
  ///
  /// A ZStack rather than a `TabView`: every `TabView` style adds visible
  /// navigation — `.page` lets the user swipe between sections, which would
  /// also fight the drawer's drag, and the default draws a tab bar.
  private func sectionStack(canOpenDrawer: Bool) -> some View {
    let drawerAction: (() -> Void)? = canOpenDrawer ? { openSidebar() } : nil
    let selected: AppTab = appModel.selectedTab

    return ZStack {
      ChatTabView(openSidebar: drawerAction)
        .modifier(SectionVisibility(isSelected: selected == .chat))

      NavigationStack {
        ThreadsView(openSidebar: drawerAction)
      }
      .modifier(SectionVisibility(isSelected: selected == .threads))

      NavigationStack {
        DestinationsView(openSidebar: drawerAction)
      }
      .modifier(SectionVisibility(isSelected: selected == .destinations))

      NavigationStack {
        SettingsView(openSidebar: drawerAction)
      }
      .modifier(SectionVisibility(isSelected: selected == .settings))
    }
  }

  private var firstRunBinding: Binding<Bool> {
    Binding(
      get: { !appModel.hasCompletedFirstRun },
      set: { _ in }
    )
  }

  private func openSidebar() {
    showsSidebar = true
  }

  private func select(_ tab: AppTab) {
    appModel.selectedTab = tab
    dragTranslation = 0
    showsSidebar = false
  }
}

/// The iPhone drawer, extracted so `RootView.body` stays small enough to
/// type-check quickly and so the drag state has one owner.
private struct CompactShell<Sections: View>: View {
  let width: CGFloat
  @Binding var showsSidebar: Bool
  @Binding var dragTranslation: CGFloat
  let reduceMotion: Bool
  let sections: Sections
  let select: (AppTab) -> Void

  var body: some View {
    ZStack(alignment: .leading) {
      sections
        .allowsHitTesting(!showsSidebar)
        .accessibilityHidden(showsSidebar)

      if showsSidebar || dragTranslation != 0 {
        WayfinderTheme.scrim
          .opacity(scrimOpacity)
          .ignoresSafeArea()
          .onTapGesture { close() }
          .accessibilityHidden(true)

        AppSidebarView(select: select, close: { close() })
          .frame(width: width)
          .offset(x: drawerOffset)
          .accessibilityAddTraits(.isModal)
      }
    }
    // Dismissible by swipe, not only by hitting the scrim, and interruptible:
    // releasing below the threshold springs back rather than snapping.
    .gesture(dragGesture)
    .wayfinderAnimation(
      WayfinderMotion.drawer,
      value: showsSidebar,
      reduceMotion: reduceMotion
    )
  }

  private var dragGesture: some Gesture {
    DragGesture(minimumDistance: 12, coordinateSpace: .local)
      .onChanged { value in
        guard showsSidebar else { return }
        // Only closing drags move the drawer; pulling right does nothing.
        dragTranslation = min(0, value.translation.width)
      }
      .onEnded { value in
        guard showsSidebar else { return }
        let travelled = -value.translation.width
        let velocity = -value.predictedEndTranslation.width
        let shouldClose = travelled > width / 3 || velocity > width
        withAnimation(
          WayfinderMotion.resolved(
            WayfinderMotion.drawer,
            reduceMotion: reduceMotion
          )
        ) {
          dragTranslation = 0
          if shouldClose {
            showsSidebar = false
          }
        }
      }
  }

  private var drawerOffset: CGFloat {
    let base: CGFloat = showsSidebar ? 0 : -width
    return max(-width, base + dragTranslation)
  }

  private var scrimOpacity: Double {
    guard width > 0 else { return showsSidebar ? 1 : 0 }
    let revealed = (width + drawerOffset) / width
    return Double(max(0, min(1, revealed)))
  }

  private func close() {
    dragTranslation = 0
    showsSidebar = false
  }
}

/// Plays the app's feedback vocabulary and speaks the matching announcement.
/// A concrete modifier so neither the trigger closure nor its result type has
/// to be inferred at the call site.
private struct RootFeedback: ViewModifier {
  let event: WayfinderFeedbackEvent?

  func body(content: Content) -> some View {
    content
      .sensoryFeedback(trigger: event) { _, current in
        guard let current else { return nil }
        return current.kind.sensoryFeedback
      }
      .onChange(of: event) { _, latest in
        guard let announcement = latest?.kind.announcement else { return }
        AccessibilityNotification.Announcement(announcement).post()
      }
  }
}

/// Hides a section without removing it, so its `NavigationStack` keeps the
/// state the roadmap requires preserved across drawer switches.
///
/// A concrete `ViewModifier` rather than a generic helper returning
/// `some View`: the generic form left the solver unifying four opaque types
/// inside one `ZStack` and it gave up without a usable diagnostic.
private struct SectionVisibility: ViewModifier {
  let isSelected: Bool

  func body(content: Content) -> some View {
    content
      .opacity(isSelected ? 1 : 0)
      .allowsHitTesting(isSelected)
      .accessibilityHidden(!isSelected)
      .zIndex(isSelected ? 1 : 0)
  }
}

private struct AppSidebarView: View {
  @Environment(AppModel.self) private var appModel
  @ScaledMetric(relativeTo: .headline) private var headerHeight: CGFloat = 56

  let select: (AppTab) -> Void
  /// Present on iPhone, where the drawer is modal. Assistive technology had
  /// no way to dismiss it: the scrim is hidden from it by design, leaving
  /// selecting a destination as the only exit (UX-027).
  let close: (() -> Void)?

  private var recentLimit: Int { 30 }

  var body: some View {
    VStack(spacing: 0) {
      sidebarHeader

      ScrollView {
        VStack(alignment: .leading, spacing: WayfinderSpacing.medium) {
          Button {
            Task {
              await appModel.startNewChat()
              select(.chat)
            }
          } label: {
            Label("New chat", systemImage: "square.and.pencil")
              .frame(maxWidth: .infinity, alignment: .leading)
              .frame(minHeight: WayfinderMetrics.minimumHitTarget)
              .contentShape(Rectangle())
          }
          .buttonStyle(.plain)
          .font(.body.weight(.medium))
          .accessibilityHint("Starts a new conversation")

          if !appModel.pinnedThreads.isEmpty {
            threadSection("Pinned", threads: appModel.pinnedThreads)
          }

          threadSection(
            "Chats",
            threads: Array(appModel.unpinnedThreads.prefix(recentLimit)),
            showsEmptyState: appModel.threads.isEmpty
          )

          SidebarDestinationButton(
            title: "All chats",
            systemImage: AppTab.threads.systemImage,
            isSelected: appModel.selectedTab == .threads
          ) {
            select(.threads)
          }

          Divider()

          SidebarDestinationButton(
            title: "Destinations",
            systemImage: AppTab.destinations.systemImage,
            isSelected: appModel.selectedTab == .destinations
          ) {
            select(.destinations)
          }

          SidebarDestinationButton(
            title: "Settings",
            systemImage: AppTab.settings.systemImage,
            isSelected: appModel.selectedTab == .settings
          ) {
            select(.settings)
          }
        }
        .padding(.horizontal, WayfinderSpacing.medium)
        .padding(.top, WayfinderSpacing.small)
        .padding(.bottom, WayfinderSpacing.large)
      }

      Divider()

      Button {
        select(.settings)
      } label: {
        Label {
          VStack(alignment: .leading, spacing: 2) {
            Text(appModel.privacyPosture.title)
              .font(.subheadline.weight(.medium))
            Text(appModel.privacyPosture.boundarySummary)
              .font(.caption)
              .foregroundStyle(.secondary)
              .lineLimit(2)
          }
        } icon: {
          Image(systemName: appModel.privacyPosture.symbolName)
            .foregroundStyle(WayfinderTheme.accent)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .frame(minHeight: WayfinderMetrics.minimumHitTarget)
        .contentShape(Rectangle())
      }
      .buttonStyle(.plain)
      .padding(WayfinderSpacing.medium)
      .accessibilityHint("Opens privacy settings")
    }
    .background(WayfinderTheme.raisedSurface)
  }

  @ViewBuilder
  private func threadSection(
    _ title: String,
    threads: [ConversationThreadSnapshot],
    showsEmptyState: Bool = false
  ) -> some View {
    VStack(alignment: .leading, spacing: WayfinderSpacing.xSmall) {
      Text(title)
        .font(.caption.weight(.semibold))
        .foregroundStyle(.secondary)
        .textCase(.uppercase)
        .accessibilityAddTraits(.isHeader)

      if showsEmptyState, threads.isEmpty {
        Text("Your conversations will appear here.")
          .font(.subheadline)
          .foregroundStyle(.secondary)
          .padding(.vertical, WayfinderSpacing.xSmall)
      }

      ForEach(threads) { thread in
        Button {
          Task {
            await appModel.selectThread(id: thread.id)
            select(.chat)
          }
        } label: {
          HStack(spacing: WayfinderSpacing.xSmall) {
            VStack(alignment: .leading, spacing: 3) {
              Text(thread.title)
                .lineLimit(1)
              Text(subtitle(for: thread))
                .font(.caption)
                .foregroundStyle(.secondary)
            }
            .frame(maxWidth: .infinity, alignment: .leading)

            if thread.isPinned {
              Image(systemName: "pin.fill")
                .font(.caption2)
                .foregroundStyle(.secondary)
                .accessibilityHidden(true)
            }
          }
          .padding(.horizontal, WayfinderSpacing.small)
          .padding(.vertical, WayfinderSpacing.xSmall)
          .frame(minHeight: WayfinderMetrics.minimumHitTarget)
          .background(
            thread.id == appModel.activeThreadID
              && appModel.selectedTab == .chat
              ? Color.primary.opacity(0.07)
              : Color.clear,
            in: RoundedRectangle(cornerRadius: WayfinderRadius.small)
          )
          .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(
          thread.isPinned ? "\(thread.title), pinned" : thread.title
        )
        .accessibilityValue(subtitle(for: thread))
        .accessibilityAction(named: thread.isPinned ? "Unpin" : "Pin") {
          Task { await appModel.setPinned(!thread.isPinned, threadID: thread.id) }
        }
      }
    }
  }

  private func subtitle(for thread: ConversationThreadSnapshot) -> String {
    if thread.id == appModel.activeThreadID {
      return "Current chat"
    }
    return thread.updatedAt.relativeDescription
  }

  private var sidebarHeader: some View {
    HStack(spacing: WayfinderSpacing.xSmall) {
      WayfinderMark()

      Text("Wayfinder")
        .font(.headline)

      Spacer()

      if let close {
        Button(action: close) {
          Image(systemName: "xmark")
            .font(.subheadline.weight(.semibold))
            .frame(
              width: WayfinderMetrics.minimumHitTarget,
              height: WayfinderMetrics.minimumHitTarget
            )
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .foregroundStyle(.secondary)
        .accessibilityLabel("Close navigation")
      }
    }
    .padding(.leading, WayfinderSpacing.medium)
    .padding(.trailing, close == nil ? WayfinderSpacing.medium : WayfinderSpacing.hairline)
    .frame(minHeight: headerHeight)
  }
}

extension Date {
  /// Relative time, as both reference apps use in conversation lists
  /// (UX-022). Falls back to an absolute date once relative wording stops
  /// being useful.
  var relativeDescription: String {
    let elapsed = Date().timeIntervalSince(self)
    guard elapsed < 60 else {
      guard elapsed < 6 * 24 * 60 * 60 else {
        return formatted(date: .abbreviated, time: .omitted)
      }
      return formatted(.relative(presentation: .named))
    }
    return "Just now"
  }
}

private struct SidebarDestinationButton: View {
  let title: String
  let systemImage: String
  let isSelected: Bool
  let action: () -> Void

  var body: some View {
    Button(action: action) {
      Label(title, systemImage: systemImage)
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.horizontal, WayfinderSpacing.small)
        .padding(.vertical, WayfinderSpacing.xSmall)
        .frame(minHeight: WayfinderMetrics.minimumHitTarget)
        .background(
          isSelected ? Color.primary.opacity(0.07) : Color.clear,
          in: RoundedRectangle(cornerRadius: WayfinderRadius.small)
        )
        .contentShape(Rectangle())
    }
    .buttonStyle(.plain)
  }
}

struct SidebarToolbarButton: ToolbarContent {
  let action: () -> Void

  var body: some ToolbarContent {
    ToolbarItem(placement: .topBarLeading) {
      Button(action: action) {
        Image(systemName: "line.3.horizontal")
      }
      .accessibilityLabel("Open navigation")
    }
  }
}

struct WayfinderMark: View {
  var body: some View {
    Image(systemName: "point.3.connected.trianglepath.dotted")
      .font(.body.weight(.semibold))
      .foregroundStyle(WayfinderTheme.accent)
      .accessibilityHidden(true)
  }
}
