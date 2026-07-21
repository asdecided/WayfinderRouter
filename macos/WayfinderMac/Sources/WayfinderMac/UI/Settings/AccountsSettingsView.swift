import AppKit
import SwiftUI

public struct AccountsSettingsView: View {
    @ObservedObject private var accountState: CodexAccountSettingsState
    @State private var confirmSignOut = false
    @State private var copiedSetup = false

    public init(accountState: CodexAccountSettingsState) {
        self.accountState = accountState
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            VStack(alignment: .leading, spacing: 6) {
                Text("Accounts")
                    .font(.title3.weight(.semibold))
                Text("Connect services that use an account rather than a provider API key.")
                    .font(.callout)
                    .foregroundStyle(.secondary)
            }

            Form {
                Section("ChatGPT") {
                    accountContent
                }

                Section("How this connection works") {
                    Text("The local gateway asks Codex to manage ChatGPT sign-in. Wayfinder receives account status and available model names, never access tokens.")
                        .font(.callout)
                    Text("ChatGPT requests are hosted and leave this Mac. OpenAI Platform API keys remain separate under Keys.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
            .formStyle(.grouped)
        }
        .padding(.horizontal, 28)
        .padding(.top, 24)
        .padding(.bottom, 16)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .task {
            if accountState.state == .checking {
                await accountState.refresh()
            }
        }
        .confirmationDialog(
            "Sign out of ChatGPT?",
            isPresented: $confirmSignOut,
            titleVisibility: .visible
        ) {
            Button("Sign Out", role: .destructive) {
                Task { await accountState.signOut() }
            }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("ChatGPT routes will be unavailable until you sign in again.")
        }
    }

    @ViewBuilder
    private var accountContent: some View {
        switch accountState.state {
        case .checking:
            statusRow(
                title: "Checking account…",
                detail: "Asking the local gateway for ChatGPT account status.",
                symbol: "person.crop.circle.badge.clock",
                showsProgress: true
            )
        case .setupRequired:
            setupRequiredContent
        case .signedOut:
            signedOutContent
        case .awaitingBrowser(let login):
            pendingBrowserContent(login)
        case .awaitingDeviceCode(let login):
            pendingDeviceCodeContent(login)
        case .connected(let profile, let models):
            connectedContent(profile: profile, models: models)
        case .reauthenticationRequired(let detail):
            reauthenticationContent(detail: detail)
        case .unavailable(let detail):
            unavailableContent(detail: detail)
        case .failed(let message):
            failedContent(message: message)
        }
    }

    private var setupRequiredContent: some View {
        VStack(alignment: .leading, spacing: 14) {
            statusRow(
                title: "Add ChatGPT to Wayfinder",
                detail: "ChatGPT account access is optional and has not been added to this gateway yet.",
                symbol: "person.crop.circle.badge.plus",
                tint: WayfinderTheme.local
            )

            VStack(alignment: .leading, spacing: 8) {
                setupStep(1, "Copy the configuration below.")
                setupStep(2, "Open your gateway config and paste it at the end.")
                setupStep(3, "Restart the gateway, then check again to sign in.")
            }

            Text(Self.chatGPTConfiguration)
                .font(.system(.caption, design: .monospaced))
                .textSelection(.enabled)
                .padding(10)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(.background.opacity(0.58), in: RoundedRectangle(cornerRadius: 8))
                .overlay {
                    RoundedRectangle(cornerRadius: 8)
                        .stroke(WayfinderTheme.hairline, lineWidth: 1)
                }

            HStack(spacing: 8) {
                Button(copiedSetup ? "Copied" : "Copy Configuration") {
                    copy(Self.chatGPTConfiguration)
                    copiedSetup = true
                }
                .buttonStyle(.borderedProminent)

                Button("Open Config") {
                    openGatewayConfig()
                }

                Button("Gateway Settings") {
                    NotificationCenter.default.post(
                        name: .wayfinderOpenSettings,
                        object: SettingsSection.gateway
                    )
                }

                Spacer()

                Button("Check Again") {
                    Task { await accountState.refresh() }
                }
                .disabled(accountState.isPerformingAction)
            }

            Text("Signing in will not change Automatic routing. You choose ChatGPT explicitly from Chat after setup.")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
    }

    private var signedOutContent: some View {
        VStack(alignment: .leading, spacing: 12) {
            statusRow(
                title: "Not signed in",
                detail: "Use the models included with your eligible ChatGPT Codex account.",
                symbol: "person.crop.circle"
            )
            HStack(spacing: 8) {
                Button("Sign in with ChatGPT") {
                    beginBrowserLogin()
                }
                .buttonStyle(.borderedProminent)
                .disabled(accountState.isPerformingAction)

                Button("Use Device Code") {
                    Task { _ = await accountState.beginLogin(flow: .deviceCode) }
                }
                .disabled(accountState.isPerformingAction)

                actionProgress
                Spacer()
            }
        }
    }

    private func pendingBrowserContent(_ login: CodexPendingLogin) -> some View {
        VStack(alignment: .leading, spacing: 12) {
            statusRow(
                title: "Finish signing in",
                detail: "Complete ChatGPT sign-in in your browser. This page updates automatically.",
                symbol: "safari"
            )
            HStack(spacing: 8) {
                Button("Open Browser") {
                    NSWorkspace.shared.open(login.url)
                }
                .buttonStyle(.borderedProminent)
                Button("Cancel", role: .cancel) {
                    Task { await accountState.cancelLogin() }
                }
                .disabled(accountState.isPerformingAction)
                actionProgress
                Spacer()
            }
        }
    }

    private func pendingDeviceCodeContent(_ login: CodexPendingLogin) -> some View {
        VStack(alignment: .leading, spacing: 12) {
            statusRow(
                title: "Enter this device code",
                detail: "Open the ChatGPT sign-in page and enter the code below.",
                symbol: "number.square"
            )
            if let code = login.userCode {
                HStack(spacing: 8) {
                    Text(code)
                        .font(.system(.body, design: .monospaced).weight(.semibold))
                        .textSelection(.enabled)
                    Button {
                        copy(code)
                    } label: {
                        Label("Copy Code", systemImage: "doc.on.doc")
                    }
                }
            }
            HStack(spacing: 8) {
                Button("Open Sign-In Page") {
                    NSWorkspace.shared.open(login.url)
                }
                .buttonStyle(.borderedProminent)
                Button("Cancel", role: .cancel) {
                    Task { await accountState.cancelLogin() }
                }
                .disabled(accountState.isPerformingAction)
                actionProgress
                Spacer()
            }
        }
    }

    private func connectedContent(profile: CodexAccountProfile, models: [CodexModel]) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            statusRow(
                title: "Connected",
                detail: connectedDetail(profile),
                symbol: "checkmark.circle.fill",
                tint: WayfinderTheme.local
            )

            if let email = profile.email {
                accountValueRow(label: "Account", value: email)
            }
            if let plan = profile.plan {
                accountValueRow(label: "Plan", value: plan)
            }

            Divider()

            VStack(alignment: .leading, spacing: 5) {
                Text("Available models")
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(.secondary)
                if models.isEmpty {
                    Text(accountState.modelCatalogError ?? "No Codex models are currently available for this account.")
                        .font(.callout)
                        .foregroundStyle(.secondary)
                } else {
                    Text(modelSummary(models))
                        .font(.callout)
                        .textSelection(.enabled)
                }
            }

            HStack(spacing: 8) {
                Button("Refresh") {
                    Task { await accountState.refresh() }
                }
                .disabled(accountState.isPerformingAction)
                Button("Sign Out", role: .destructive) {
                    confirmSignOut = true
                }
                .disabled(accountState.isPerformingAction)
                actionProgress
                Spacer()
            }
        }
    }

    private func reauthenticationContent(detail: String?) -> some View {
        VStack(alignment: .leading, spacing: 12) {
            statusRow(
                title: "Sign in again",
                detail: detail ?? "Your ChatGPT session needs to be renewed.",
                symbol: "person.crop.circle.badge.exclamationmark",
                tint: .orange
            )
            HStack(spacing: 8) {
                Button("Sign in with ChatGPT") {
                    beginBrowserLogin()
                }
                .buttonStyle(.borderedProminent)
                .disabled(accountState.isPerformingAction)
                Button("Sign Out", role: .destructive) {
                    confirmSignOut = true
                }
                .disabled(accountState.isPerformingAction)
                actionProgress
                Spacer()
            }
        }
    }

    private func unavailableContent(detail: String?) -> some View {
        VStack(alignment: .leading, spacing: 12) {
            statusRow(
                title: "Codex unavailable",
                detail: detail ?? "The local gateway could not start its Codex runtime.",
                symbol: "exclamationmark.triangle",
                tint: .orange
            )
            Button("Try Again") {
                Task { await accountState.refresh() }
            }
        }
    }

    private func failedContent(message: String) -> some View {
        VStack(alignment: .leading, spacing: 12) {
            statusRow(
                title: "Could not check ChatGPT",
                detail: message,
                symbol: "exclamationmark.circle",
                tint: .red
            )
            Button("Try Again") {
                Task { await accountState.refresh() }
            }
        }
    }

    private func statusRow(
        title: String,
        detail: String,
        symbol: String,
        tint: Color = .secondary,
        showsProgress: Bool = false
    ) -> some View {
        HStack(alignment: .top, spacing: 10) {
            if showsProgress {
                ProgressView()
                    .controlSize(.small)
                    .frame(width: 18, height: 18)
            } else {
                Image(systemName: symbol)
                    .foregroundStyle(tint)
                    .frame(width: 18, height: 18)
            }
            VStack(alignment: .leading, spacing: 3) {
                Text(title)
                    .font(.body.weight(.medium))
                Text(detail)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            Spacer(minLength: 0)
        }
        .accessibilityElement(children: .combine)
    }

    private func setupStep(_ number: Int, _ text: String) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Text("\(number)")
                .font(.caption2.weight(.semibold))
                .foregroundStyle(.white)
                .frame(width: 18, height: 18)
                .background(WayfinderTheme.local, in: Circle())
            Text(text)
                .font(.callout)
        }
    }

    private func accountValueRow(label: String, value: String) -> some View {
        HStack(alignment: .firstTextBaseline) {
            Text(label)
                .foregroundStyle(.secondary)
                .frame(width: 72, alignment: .leading)
            Text(value)
                .textSelection(.enabled)
            Spacer()
        }
        .font(.callout)
    }

    @ViewBuilder
    private var actionProgress: some View {
        if accountState.isPerformingAction {
            ProgressView()
                .controlSize(.small)
        }
    }

    private func beginBrowserLogin() {
        Task {
            if let url = await accountState.beginLogin(flow: .browser) {
                NSWorkspace.shared.open(url)
            }
        }
    }

    private func connectedDetail(_ profile: CodexAccountProfile) -> String {
        if let plan = profile.plan {
            return "Signed in with a \(plan) ChatGPT account."
        }
        return "Signed in with ChatGPT."
    }

    private func modelSummary(_ models: [CodexModel]) -> String {
        let visible = models.prefix(8).map(\.label).joined(separator: ", ")
        let hiddenCount = models.count - min(models.count, 8)
        return hiddenCount > 0 ? "\(visible), and \(hiddenCount) more" : visible
    }

    private func copy(_ value: String) {
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(value, forType: .string)
    }

    private func openGatewayConfig() {
        let url = URL(fileURLWithPath: GatewayServiceController.defaultConfigPath())
        NSWorkspace.shared.open(url)
    }

    private static let chatGPTConfiguration = """
    [gateway.models.chatgpt-sol]
    provider = "codex-app-server"
    model = "gpt-5.6-sol"
    context_window = 1050000
    """
}
