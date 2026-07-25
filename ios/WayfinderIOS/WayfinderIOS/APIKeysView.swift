import SwiftUI

struct APIKeysView: View {
  @Environment(AppModel.self) private var appModel
  @State private var editingProvider: APIKeyProviderDescriptor?
  @State private var removalProvider: APIKeyProviderDescriptor?

  var body: some View {
    List {
      Section {
        ForEach(APIKeyProviderDescriptor.supported) { provider in
          APIKeyProviderRow(
            provider: provider,
            isConfigured: appModel.isCredentialConfigured(provider.id),
            edit: { editingProvider = provider },
            remove: { removalProvider = provider }
          )
        }
      } footer: {
        Text(
          "Keys remain in this device's Keychain. Adding one does not change Automatic routing or enable a provider until its destination is configured."
        )
      }

      Section("Account access") {
        LabeledContent("OpenRouter", value: "Sign in under Accounts")
        LabeledContent("ChatGPT / Codex", value: "Paired Mac only")
        Text(
          "OpenAI Platform keys and ChatGPT subscriptions are separate. Wayfinder never treats a consumer subscription as general API access."
        )
        .font(.footnote)
        .foregroundStyle(.secondary)
      }
    }
    .navigationTitle("API Keys")
    .sheet(item: $editingProvider) { provider in
      APIKeyEditorSheet(provider: provider)
        .presentationDetents([.medium])
        .presentationDragIndicator(.visible)
    }
    .confirmationDialog(
      "Remove API key?",
      isPresented: Binding(
        get: { removalProvider != nil },
        set: { isPresented in
          if !isPresented {
            removalProvider = nil
          }
        }
      ),
      titleVisibility: .visible
    ) {
      if let provider = removalProvider {
        Button("Remove \(provider.displayName) Key", role: .destructive) {
          removalProvider = nil
          Task {
            await appModel.removeAPIKey(for: provider)
          }
        }
      }
      Button("Cancel", role: .cancel) {
        removalProvider = nil
      }
    } message: {
      Text(
        "Wayfinder will delete this credential from the iOS Keychain. Existing conversations are unaffected."
      )
    }
    .task {
      await appModel.restoreCredentialStatuses()
    }
  }
}

private struct APIKeyProviderRow: View {
  let provider: APIKeyProviderDescriptor
  let isConfigured: Bool
  let edit: () -> Void
  let remove: () -> Void

  var body: some View {
    VStack(alignment: .leading, spacing: 12) {
      HStack(alignment: .firstTextBaseline) {
        VStack(alignment: .leading, spacing: 3) {
          Text(provider.displayName)
            .font(.body.weight(.medium))
          Text(provider.detail)
            .font(.footnote)
            .foregroundStyle(.secondary)
        }

        Spacer(minLength: 12)

        Label(
          isConfigured ? "Saved" : "Not configured",
          systemImage: isConfigured ? "checkmark.circle.fill" : "circle"
        )
        .font(.caption.weight(.semibold))
        .foregroundStyle(isConfigured ? WayfinderTheme.accent : .secondary)
      }

      HStack {
        Button(isConfigured ? "Replace Key" : "Add Key", action: edit)
          .buttonStyle(.bordered)

        if isConfigured {
          Button("Remove", role: .destructive, action: remove)
            .buttonStyle(.borderless)
        }
      }
    }
    .padding(.vertical, 5)
    .accessibilityElement(children: .contain)
  }
}

private struct APIKeyEditorSheet: View {
  @Environment(AppModel.self) private var appModel
  @Environment(\.dismiss) private var dismiss
  @State private var key = ""
  @State private var isSaving = false

  let provider: APIKeyProviderDescriptor

  var body: some View {
    NavigationStack {
      Form {
        Section {
          SecureField(provider.placeholder, text: $key)
            .textInputAutocapitalization(.never)
            .autocorrectionDisabled()
            .accessibilityLabel("\(provider.displayName) API key")
            .accessibilityValue(key.isEmpty ? "Empty" : "Entered")
        } header: {
          Text(provider.displayName)
        } footer: {
          Text(
            "The key is stored with device-only Keychain protection. Wayfinder will never display it again after saving."
          )
        }

        Section {
          Label(
            "Adding this key does not change your routing preferences.",
            systemImage: "checkmark.shield"
          )
          .font(.footnote)
          .foregroundStyle(.secondary)
        }
      }
      .navigationTitle(
        appModel.isCredentialConfigured(provider.id) ? "Replace Key" : "Add Key"
      )
      .navigationBarTitleDisplayMode(.inline)
      .interactiveDismissDisabled(isSaving)
      .toolbar {
        ToolbarItem(placement: .cancellationAction) {
          Button("Cancel") {
            key = ""
            dismiss()
          }
          .disabled(isSaving)
        }

        ToolbarItem(placement: .confirmationAction) {
          Button("Save") {
            isSaving = true
            Task {
              let saved = await appModel.saveAPIKey(key, for: provider)
              isSaving = false
              if saved {
                key = ""
                dismiss()
              }
            }
          }
          .disabled(
            isSaving
              || key.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
          )
        }
      }
      .onDisappear {
        key = ""
      }
    }
  }
}
