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
      Section {
        Text(
          "Choose a destination explicitly in Chat. Connecting a key does not silently add it to Automatic."
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
