import SwiftUI

struct DestinationsView: View {
  @Environment(AppModel.self) private var appModel
  @State private var searchText = ""
  var openSidebar: (() -> Void)?

  var body: some View {
    Group {
      if appModel.destinations.isEmpty {
        // The no-destination state the roadmap requires to be useful, not
        // blank: skipping the first-launch chooser lands here.
        ContentUnavailableView {
          Label("No destinations yet", systemImage: "point.3.connected.trianglepath.dotted")
        } description: {
          Text(
            "Add an API key or connect an account, and it will appear here. Wayfinder never adds one to Automatic routing on its own."
          )
        } actions: {
          Button("Open Settings") {
            appModel.selectedTab = .settings
          }
        }
      } else if filteredDestinations.isEmpty {
        ContentUnavailableView.search(text: searchText)
      } else {
        list
      }
    }
    .navigationTitle("Destinations")
    .searchable(text: $searchText, prompt: "Search models")
    .refreshable {
      await appModel.refreshModelInventory()
    }
    .toolbar {
      if let openSidebar {
        SidebarToolbarButton(action: openSidebar)
      }
      ToolbarItem(placement: .topBarTrailing) {
        Button {
          Task {
            await appModel.refreshModelInventory()
          }
        } label: {
          if appModel.isRefreshingModelInventory {
            ProgressView()
          } else {
            Image(systemName: "arrow.clockwise")
          }
        }
        .disabled(appModel.isRefreshingModelInventory)
        .accessibilityLabel("Refresh models")
      }
    }
  }

  private var list: some View {
    List {
      // The shipping app always compiles destinations, so an empty list is
      // unreachable; "nothing is ready" is the state a user actually lands in
      // after skipping the first-launch chooser.
      if appModel.readyDestinations.isEmpty {
        Section {
          Label(
            "No destination is connected yet. Add an API key or connect an account to start a conversation.",
            systemImage: "exclamationmark.circle"
          )
          .font(.footnote)
          .foregroundStyle(WayfinderTheme.warning)

          Button("Open Settings") {
            appModel.selectedTab = .settings
          }
          .font(.footnote.weight(.semibold))
        }
      }

      Section {
        Text(
          appModel.hasAutomaticDestination
            ? "Automatic picks from the destinations enrolled below. Connecting a key never enrols one on its own — that is always your explicit choice."
            : "Nothing is enrolled in Automatic yet, so every message needs a destination you pick. Enrol one below and Wayfinder starts choosing for you."
        )
        .font(.footnote)
        .foregroundStyle(.secondary)
      }

      if let modelInventoryNotice = appModel.modelInventoryNotice {
        Section {
          Label(modelInventoryNotice, systemImage: "exclamationmark.triangle")
            .font(.footnote)
            .foregroundStyle(.secondary)
        }
      }

      automaticEnrolmentSection

      if !onDeviceDestinations.isEmpty {
        Section("On this device") {
          ForEach(onDeviceDestinations) { destination in
            destinationRow(destination)
          }
        }
      }

      if !hostedDestinations.isEmpty {
        Section("Direct cloud") {
          ForEach(hostedDestinations) { destination in
            destinationRow(destination)
          }
        }
      }
    }
  }

  /// The consent surface for Automatic routing.
  ///
  /// Deliberately a section of its own rather than a control inside each
  /// destination row: tapping a row means "use this for my next message",
  /// and enrolling means "you may choose this for me from now on". Those are
  /// different promises and they should not share a hit target.
  ///
  /// On-device is stated, not offered. It is enrolled by construction because
  /// it sends nothing anywhere, which makes it the one case where a default
  /// costs the user nothing to consent to — and saying so here is the whole
  /// point, since a toggle the user cannot move would imply otherwise.
  @ViewBuilder
  private var automaticEnrolmentSection: some View {
    Section {
      ForEach(enrollableDestinations) { destination in
        Toggle(
          isOn: Binding(
            get: { appModel.isEnrolledInAutomatic(destination) },
            set: { isEnrolled in
              Task {
                await appModel.setEnrolledInAutomatic(
                  isEnrolled,
                  destinationID: destination.id
                )
              }
            }
          )
        ) {
          VStack(alignment: .leading, spacing: 2) {
            Text(destination.displayName)
            Text(
              destination.readiness == .ready
                ? "Costs apply when Automatic picks it"
                : "Enrolled, but needs a key before it can be picked"
            )
            .font(.caption)
            .foregroundStyle(.secondary)
          }
        }
        .frame(minHeight: WayfinderMetrics.minimumHitTarget)
      }

      if let onDevice = onDeviceDestinations.first {
        LabeledContent("Always enrolled", value: onDevice.displayName)
          .font(.footnote)
          .accessibilityHint("On-device execution is always available to Automatic")
      }

      if enrollableDestinations.isEmpty {
        Text("Connect a hosted provider to offer Automatic more to choose from.")
          .font(.footnote)
          .foregroundStyle(.secondary)
      }
    } header: {
      Text("Automatic routing")
    } footer: {
      Text(
        "On-device execution is always available to Automatic — it sends nothing anywhere. A hosted destination joins only when you enrol it here, and you can withdraw it at any time."
      )
    }
  }

  /// Hosted destinations the user may enrol: those that could actually be
  /// picked, plus anything already enrolled so consent stays withdrawable
  /// after a key is removed.
  ///
  /// Drawn from the full destination list rather than the search results, so
  /// typing in the search field never hides something already consented to.
  /// Gating on readiness also keeps this from becoming a wall of toggles once
  /// model discovery has run — there is nothing to decide about a destination
  /// that has no credential and has never been enrolled.
  private var enrollableDestinations: [RoutingDestination] {
    appModel.destinations.filter { destination in
      guard appModel.canEnrolInAutomatic(destination) else { return false }
      return destination.readiness == .ready
        || appModel.isEnrolledInAutomatic(destination)
    }
  }

  private func destinationRow(
    _ destination: RoutingDestination
  ) -> some View {
    Button {
      appModel.selectDestination(destination.id)
      appModel.selectedTab = .chat
    } label: {
      Label {
        VStack(alignment: .leading, spacing: 3) {
          Text(destination.displayName)
          Text(destination.detail)
            .font(.caption)
            .foregroundStyle(.secondary)
        }
      } icon: {
        // Boundary identity, not ambient accent: the glyph and the colour
        // both come from where the destination actually executes.
        Image(systemName: destination.boundary.routeSymbolName)
          .foregroundStyle(destination.boundary.routeColor)
      }
    }
    .buttonStyle(.plain)
    .frame(minHeight: WayfinderMetrics.minimumHitTarget)
    .badge(readinessLabel(for: destination))
    .disabled(destination.readiness != .ready)
    .accessibilityHint(
      destination.readiness == .ready
        ? "Uses this destination for the next message"
        : "Unavailable: \(readinessLabel(for: destination).lowercased())"
    )
    .accessibilityLabel(destination.displayName)
    .accessibilityValue(
      "\(destination.boundary.receiptPhrase.replacingOccurrences(of: "Ran ", with: "Runs ")). \(readinessLabel(for: destination))"
    )
  }

  private var onDeviceDestinations: [RoutingDestination] {
    filteredDestinations.filter { $0.boundary == .onDevice }
  }

  private var hostedDestinations: [RoutingDestination] {
    filteredDestinations.filter { $0.boundary == .hosted }
  }

  private var filteredDestinations: [RoutingDestination] {
    let query = searchText.trimmingCharacters(in: .whitespacesAndNewlines)
    guard !query.isEmpty else {
      return appModel.destinations
    }
    return appModel.destinations.filter {
      $0.displayName.localizedStandardContains(query)
        || $0.providerName.localizedStandardContains(query)
        || $0.modelID.localizedStandardContains(query)
    }
  }

  private func readinessLabel(
    for destination: RoutingDestination
  ) -> String {
    switch destination.readiness {
    case .ready:
      "Ready"
    case .signedOut:
      "Key required"
    case .checking:
      "Checking"
    case .authorizing:
      "Connecting"
    case .reauthenticationRequired:
      "Reconnect"
    case .usageLimited:
      "Usage limited"
    case .modelUnavailable:
      "Model unavailable"
    case .networkUnavailable:
      "Offline"
    case .unsupportedPlatform:
      "Unsupported"
    case .unavailable, .failed:
      "Unavailable"
    }
  }
}
