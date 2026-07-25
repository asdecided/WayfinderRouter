import SwiftUI

struct DestinationsView: View {
  @Environment(AppModel.self) private var appModel
  @State private var searchText = ""
  var openSidebar: (() -> Void)?

  var body: some View {
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
            destinationRow(destination, systemImage: "iphone")
          }
        }
      }

      if !hostedDestinations.isEmpty {
        Section("Direct cloud") {
          ForEach(hostedDestinations) { destination in
            destinationRow(destination, systemImage: "cloud")
          }
        }
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

  private func destinationRow(
    _ destination: RoutingDestination,
    systemImage: String
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
        Image(systemName: systemImage)
          .foregroundStyle(WayfinderTheme.accent)
      }
    }
    .buttonStyle(.plain)
    .badge(readinessLabel(for: destination))
    .disabled(destination.readiness != .ready)
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
