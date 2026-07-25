import SwiftUI

struct SettingsView: View {
  @Environment(AppModel.self) private var appModel
  @State private var exportedConversations: Data?
  @State private var showsClearConfirmation = false
  var openSidebar: (() -> Void)?

  var body: some View {
    @Bindable var appModel = appModel

    Form {
      // Keychain failures render in place with a way forward rather than as
      // a blocking alert (UX-015).
      if let credentialNotice = appModel.credentialNotice {
        Section {
          Label(credentialNotice, systemImage: "exclamationmark.triangle.fill")
            .font(.footnote)
            .foregroundStyle(WayfinderTheme.warning)
          Button("Dismiss") {
            appModel.credentialNotice = nil
          }
          .font(.footnote.weight(.semibold))
        }
      }

      Section("Connections") {
        NavigationLink {
          AccountsView()
        } label: {
          LabeledContent(
            "Accounts",
            value: accountSummary
          )
        }

        NavigationLink {
          APIKeysView()
        } label: {
          LabeledContent(
            "API Keys",
            value: credentialSummary
          )
        }
      }

      Section("Privacy") {
        Picker("Maximum execution boundary", selection: $appModel.privacyPosture) {
          ForEach(PrivacyPostureOption.allCases) { posture in
            Text(posture.title).tag(posture)
          }
        }

        // The consequence of the choice, stated where the choice is made.
        Text(appModel.privacyPosture.consequence)
          .font(.footnote)
          .foregroundStyle(.secondary)
      }

      Section("Runtime") {
        LabeledContent("Router", value: "Embedded Rust core")
        LabeledContent("Provider execution", value: "Direct and on-device")
        LabeledContent("Mac required", value: "No")
      }

      Section {
        Picker("Keep conversations", selection: retentionBinding) {
          ForEach(ConversationRetentionPolicy.allCases) { policy in
            Text(policy.title).tag(policy)
          }
        }

        // One step, not "Prepare" then "Share" (UX-023): the export is
        // built when Settings appears and refreshed as threads change, so
        // the share sheet is always one tap away.
        ShareLink(
          item: exportedConversations ?? Data(),
          preview: SharePreview("Wayfinder conversations.json")
        ) {
          Label("Export Conversations", systemImage: "square.and.arrow.up")
        }
        .disabled(exportedConversations == nil)

        Button(role: .destructive) {
          showsClearConfirmation = true
        } label: {
          Label("Clear All Conversations", systemImage: "trash")
        }
        .disabled(appModel.threads.isEmpty && appModel.draft.isEmpty)
      } header: {
        Text("Conversations")
      } footer: {
        Text(
          "Exports contain conversation text and route receipts, never provider credentials."
        )
      }
    }
    .navigationTitle("Settings")
    .toolbar {
      if let openSidebar {
        SidebarToolbarButton(action: openSidebar)
      }
    }
    .confirmationDialog(
      "Clear all conversations?",
      isPresented: $showsClearConfirmation,
      titleVisibility: .visible
    ) {
      Button("Clear All Conversations", role: .destructive) {
        Task {
          await appModel.deleteAllThreads()
          exportedConversations = nil
        }
      }
      Button("Cancel", role: .cancel) {}
    } message: {
      Text("This permanently removes saved threads and the current draft.")
    }
    .task(id: appModel.threads.count) {
      exportedConversations = await appModel.exportConversations()
    }
  }

  private var retentionBinding: Binding<ConversationRetentionPolicy> {
    Binding(
      get: { appModel.retentionPolicy },
      set: { policy in
        Task {
          await appModel.setRetentionPolicy(policy)
          exportedConversations = nil
        }
      }
    )
  }

  private var credentialSummary: String {
    switch appModel.configuredCredentialCount {
    case 0: "None saved"
    case 1: "1 saved"
    default: "\(appModel.configuredCredentialCount) saved"
    }
  }

  private var accountSummary: String {
    appModel.openRouterAccountState.readiness == .ready
      ? "OpenRouter connected"
      : "None connected"
  }
}
