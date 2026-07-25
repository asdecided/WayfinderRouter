import AuthenticationServices
import SwiftUI

struct AccountsView: View {
  @Environment(AppModel.self) private var appModel
  @State private var webAuthenticationSession =
    SystemWebAuthenticationSession()
  @State private var showsDisconnectConfirmation = false

  var body: some View {
    List {
      Section {
        VStack(alignment: .leading, spacing: 12) {
          HStack(alignment: .firstTextBaseline) {
            VStack(alignment: .leading, spacing: 3) {
              Text("OpenRouter")
                .font(.body.weight(.medium))
              Text(
                "Use an OpenRouter account without copying an API key into Wayfinder."
              )
              .font(.footnote)
              .foregroundStyle(.secondary)
            }

            Spacer(minLength: 12)

            Label(
              statusTitle,
              systemImage: statusSymbol
            )
            .font(.caption.weight(.semibold))
            .foregroundStyle(statusColor)
          }

          if appModel.openRouterAccountState.readiness == .ready {
            Button("Disconnect", role: .destructive) {
              showsDisconnectConfirmation = true
            }
            .buttonStyle(.borderless)
          } else {
            Button {
              connectOpenRouter()
            } label: {
              if appModel.openRouterAccountState.readiness == .authorizing {
                ProgressView()
                  .controlSize(.small)
              } else {
                Text("Continue with OpenRouter")
              }
            }
            .buttonStyle(.borderedProminent)
            .disabled(
              appModel.openRouterAccountState.readiness == .authorizing
            )
          }
        }
        .padding(.vertical, WayfinderSpacing.hairline)
      } footer: {
        Text(
          "OpenRouter issues a user-controlled key after sign-in. Wayfinder stores it only in this device's Keychain. Connecting does not change Automatic routing."
        )
      }

      Section("What this unlocks") {
        LabeledContent("OpenRouter Auto", value: "Uses account credits")
        LabeledContent("OpenRouter Free", value: "Rate limited")

        Text(
          "Free models can have lower availability and stricter limits. Wayfinder never silently moves a Free request to a paid destination."
        )
        .font(.footnote)
        .foregroundStyle(.secondary)
      }

      Section("Other accounts") {
        LabeledContent("ChatGPT / Codex", value: "Paired Mac only")
        LabeledContent("Kimi Code", value: "Awaiting provider approval")

        Text(
          "A consumer subscription is offered only when the provider documents a permitted account flow for third-party apps. OpenAI Platform and Moonshot Platform remain available under API Keys."
        )
        .font(.footnote)
        .foregroundStyle(.secondary)
      }
    }
    .navigationTitle("Accounts")
    .confirmationDialog(
      "Disconnect OpenRouter?",
      isPresented: $showsDisconnectConfirmation,
      titleVisibility: .visible
    ) {
      Button("Disconnect", role: .destructive) {
        Task {
          await appModel.disconnectOpenRouterAccount()
        }
      }
      Button("Cancel", role: .cancel) {}
    } message: {
      Text(
        "Wayfinder will remove the OpenRouter credential from this iPhone. Existing conversations are unaffected."
      )
    }
    .alert(
      "Account connection",
      isPresented: Binding(
        get: { appModel.accountNotice != nil },
        set: { isPresented in
          if !isPresented {
            appModel.accountNotice = nil
          }
        }
      )
    ) {
      Button("OK") {
        appModel.accountNotice = nil
      }
    } message: {
      Text(appModel.accountNotice ?? "")
    }
  }

  private var statusTitle: String {
    switch appModel.openRouterAccountState.readiness {
    case .checking:
      "Checking"
    case .authorizing:
      "Connecting"
    case .ready:
      "Connected"
    case .failed:
      "Needs attention"
    default:
      "Not connected"
    }
  }

  private var statusSymbol: String {
    switch appModel.openRouterAccountState.readiness {
    case .ready:
      "checkmark.circle.fill"
    case .failed:
      "exclamationmark.triangle.fill"
    case .authorizing, .checking:
      "clock"
    default:
      "circle"
    }
  }

  private var statusColor: Color {
    switch appModel.openRouterAccountState.readiness {
    case .ready:
      WayfinderTheme.accent
    case .failed:
      .orange
    default:
      .secondary
    }
  }

  private func connectOpenRouter() {
    Task {
      guard let challenge = await appModel.beginOpenRouterAuthorization() else {
        return
      }

      do {
        let callbackURL = try await webAuthenticationSession.authenticate(
          challenge
        )
        _ = await appModel.completeOpenRouterAuthorization(
          challenge.id,
          callbackURL: callbackURL
        )
      } catch {
        await appModel.cancelOpenRouterAuthorization(challenge.id)
        if !Self.isUserCancellation(error) {
          appModel.accountNotice =
            "Wayfinder could not finish OpenRouter sign-in."
        }
      }
    }
  }

  private static func isUserCancellation(_ error: Error) -> Bool {
    if error is CancellationError {
      return true
    }
    let error = error as NSError
    return error.domain == ASWebAuthenticationSessionError.errorDomain
      && error.code == ASWebAuthenticationSessionError.canceledLogin.rawValue
  }
}
