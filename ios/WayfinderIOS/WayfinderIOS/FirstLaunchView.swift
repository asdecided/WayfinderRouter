import SwiftUI
import WayfinderRoutingBridge

/// The first-launch chooser specified by WF-ROADMAP-0016 (UX-013).
///
/// Standalone choices lead, because the product's thesis is that a Mac is
/// optional. Every option is skippable: the roadmap requires that skipping
/// everything lands in a useful no-destination state, so this never gates the
/// app behind setup.
///
/// Nothing here fabricates capability. Apple On-Device appears only when the
/// live device reports it available, and pairing a Mac — which is v0.2.1 — is
/// shown as an explicit unavailable state rather than a button that lies.
struct FirstLaunchView: View {
  @Environment(AppModel.self) private var appModel
  @Environment(\.dismiss) private var dismiss
  @ScaledMetric(relativeTo: .largeTitle) private var markSize: CGFloat = 34

  var body: some View {
    NavigationStack {
      ScrollView {
        VStack(alignment: .leading, spacing: WayfinderSpacing.large) {
          header

          VStack(alignment: .leading, spacing: WayfinderSpacing.small) {
            Text("Use AI from this iPhone or iPad")
              .font(.headline)

            if appModel.isAppleOnDeviceAvailable {
              FirstLaunchOption(
                title: "Use Apple On-Device",
                detail: "Runs entirely on this device. No account, no network.",
                systemImage: "iphone",
                boundary: .onDevice
              ) {
                // Selects the destination rather than merely navigating to a
                // list; the verb promised use.
                appModel.selectDestination(
                  NativeAppleFoundationModelsProvider.destinationID
                )
                finish()
              }
            }

            NavigationLink {
              AccountsView()
            } label: {
              FirstLaunchOptionLabel(
                title: "Connect an Account",
                detail: "Sign in to a provider you already pay for.",
                systemImage: "person.badge.key",
                boundary: .hosted
              )
            }
            .buttonStyle(.plain)

            NavigationLink {
              APIKeysView()
            } label: {
              FirstLaunchOptionLabel(
                title: "Add an API Key",
                detail: "Kept in this device's Keychain, never synced.",
                systemImage: "key",
                boundary: .hosted
              )
            }
            .buttonStyle(.plain)
          }

          VStack(alignment: .leading, spacing: WayfinderSpacing.small) {
            Text("More ways to use Wayfinder")
              .font(.headline)

            FirstLaunchOptionLabel(
              title: "Connect a Mac",
              detail: "Not available in this release.",
              systemImage: "desktopcomputer",
              boundary: .localNetwork,
              isAvailable: false
            )
            .accessibilityElement(children: .combine)
            .accessibilityLabel("Connect a Mac")
            .accessibilityValue("Not available in this release")
          }

          Text(
            "Connecting a provider does not change Automatic routing on its own. You choose when to use it."
          )
          .font(.footnote)
          .foregroundStyle(.secondary)
        }
        .padding(WayfinderSpacing.large)
        .frame(maxWidth: WayfinderMetrics.readableWidth)
        .frame(maxWidth: .infinity)
      }
      .background(WayfinderTheme.canvas)
      .navigationBarTitleDisplayMode(.inline)
      .toolbar {
        ToolbarItem(placement: .topBarTrailing) {
          Button("Skip", action: finish)
            .accessibilityHint("Starts Wayfinder without a destination")
        }
      }
      .safeAreaInset(edge: .bottom) {
        VStack(spacing: WayfinderSpacing.xSmall) {
          Button(action: finish) {
            Text("Start Chatting")
              .font(.body.weight(.semibold))
              .foregroundStyle(WayfinderTheme.onAccent)
              .frame(maxWidth: .infinity)
              .frame(minHeight: WayfinderMetrics.minimumHitTarget)
              .background(
                WayfinderTheme.accent,
                in: RoundedRectangle(cornerRadius: WayfinderRadius.medium)
              )
          }
          .buttonStyle(.plain)

          Text(appModel.destinationReadinessSummary)
            .font(.footnote)
            .foregroundStyle(.secondary)
        }
        .padding(.horizontal, WayfinderSpacing.large)
        .padding(.vertical, WayfinderSpacing.small)
        .frame(maxWidth: WayfinderMetrics.readableWidth)
        .frame(maxWidth: .infinity)
        .background(.bar)
      }
    }
    .task {
      await appModel.restoreCredentialStatuses()
    }
  }

  private var header: some View {
    VStack(alignment: .leading, spacing: WayfinderSpacing.xSmall) {
      WayfinderMark()
        .font(.system(size: markSize, weight: .semibold))

      Text("Welcome to Wayfinder")
        .font(.largeTitle.weight(.bold))

      // Claimed the message was "then sent to the cheapest destination that
      // can handle it" while nothing was enrolled in Automatic at all, so it
      // was the first sentence a new user read and it was false. It reads
      // the enrolment state now instead of asserting the outcome.
      Text(
        appModel.hasAutomaticDestination
          ? "Every message is scored on this device, then sent to the cheapest enrolled destination that can handle it. You always see where it ran."
          : "Every message is scored on this device, and you always see where it ran. Nothing is enrolled in Automatic yet, so choosing where a message goes is up to you."
      )
      .font(.subheadline)
      .foregroundStyle(.secondary)
    }
    .accessibilityElement(children: .combine)
  }

  private func finish() {
    // Recorded before dismissing: dismissing first left the presentation
    // binding still reading "not completed" at the instant SwiftUI wrote to
    // it, making whether the cover stayed down a race.
    Task {
      await appModel.completeFirstRun()
      dismiss()
    }
  }
}

private struct FirstLaunchOption: View {
  let title: String
  let detail: String
  let systemImage: String
  let boundary: ExecutionBoundary
  let action: () -> Void

  var body: some View {
    Button(action: action) {
      FirstLaunchOptionLabel(
        title: title,
        detail: detail,
        systemImage: systemImage,
        boundary: boundary
      )
    }
    .buttonStyle(.plain)
  }
}

private struct FirstLaunchOptionLabel: View {
  let title: String
  let detail: String
  let systemImage: String
  let boundary: ExecutionBoundary
  var isAvailable = true

  var body: some View {
    HStack(spacing: WayfinderSpacing.small) {
      Image(systemName: systemImage)
        .font(.title3)
        .foregroundStyle(isAvailable ? boundary.routeColor : Color.secondary)
        .frame(width: WayfinderMetrics.minimumHitTarget)
        .accessibilityHidden(true)

      VStack(alignment: .leading, spacing: 2) {
        Text(title)
          .font(.body.weight(.medium))
          .foregroundStyle(isAvailable ? .primary : .secondary)
        Text(detail)
          .font(.footnote)
          .foregroundStyle(.secondary)
          .fixedSize(horizontal: false, vertical: true)
      }
      .frame(maxWidth: .infinity, alignment: .leading)

      if isAvailable {
        Image(systemName: "chevron.right")
          .font(.footnote.weight(.semibold))
          .foregroundStyle(.tertiary)
          .accessibilityHidden(true)
      }
    }
    .padding(WayfinderSpacing.small)
    .frame(minHeight: WayfinderMetrics.minimumHitTarget)
    .background(
      WayfinderTheme.raisedSurface,
      in: RoundedRectangle(cornerRadius: WayfinderRadius.medium)
    )
    .contentShape(Rectangle())
  }
}
