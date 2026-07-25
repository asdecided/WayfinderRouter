import SwiftUI

struct DestinationsView: View {
  @Environment(AppModel.self) private var appModel
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
    .toolbar {
      if let openSidebar {
        SidebarToolbarButton(action: openSidebar)
      }
    }
  }

  private func destinationRow(
    _ destination: RoutingDestination,
    systemImage: String
  ) -> some View {
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
    .badge(readinessLabel(for: destination))
  }

  private var onDeviceDestinations: [RoutingDestination] {
    appModel.destinations.filter { $0.boundary == .onDevice }
  }

  private var hostedDestinations: [RoutingDestination] {
    appModel.destinations.filter { $0.boundary == .hosted }
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
