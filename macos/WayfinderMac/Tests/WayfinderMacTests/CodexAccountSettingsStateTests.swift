import Foundation
import XCTest
@testable import WayfinderMacCore

final class CodexAccountSettingsStateTests: XCTestCase {
    @MainActor
    func testBrowserLoginCanBeCancelledWithoutExposingAnythingButLoginID() async {
        let login = CodexPendingLogin(
            id: "browser-login",
            flow: .browser,
            url: URL(string: "https://auth.openai.com/start")!
        )
        let client = ScriptedCodexAccountClient(
            accountResponse: .signedOut,
            loginResponse: .awaitingBrowser(login),
            cancelResponse: .signedOut
        )
        let state = CodexAccountSettingsState(client: client, automaticallyPollLogin: false)

        await state.refresh()
        XCTAssertEqual(state.state, .signedOut)
        let authorizationURL = await state.beginLogin(flow: .browser)
        XCTAssertEqual(authorizationURL, login.url)
        XCTAssertEqual(state.state, .awaitingBrowser(login))

        await state.cancelLogin()
        XCTAssertEqual(state.state, .signedOut)
        let cancelledLoginID = await client.lastCancelledLoginID()
        XCTAssertEqual(cancelledLoginID, "browser-login")
    }

    @MainActor
    func testDeviceCodeConnectedAndSignOutTransitionsAreDeterministic() async {
        let login = CodexPendingLogin(
            id: "device-login",
            flow: .deviceCode,
            url: URL(string: "https://auth.openai.com/device")!,
            userCode: "CODE"
        )
        let profile = CodexAccountProfile(email: "tom@example.com", plan: "Plus")
        let models = [CodexModel(id: "gpt-5.6-sol", displayName: "GPT-5.6 Sol")]
        let client = ScriptedCodexAccountClient(
            accountResponse: .connected(profile),
            modelsResponse: CodexModelsResponse(models: models),
            loginResponse: .awaitingDeviceCode(login),
            logoutResponse: .signedOut
        )
        let state = CodexAccountSettingsState(client: client, automaticallyPollLogin: false)

        let authorizationURL = await state.beginLogin(flow: .deviceCode)
        XCTAssertNil(authorizationURL)
        XCTAssertEqual(state.state, .awaitingDeviceCode(login))

        await state.refresh()
        XCTAssertEqual(state.state, .connected(profile: profile, models: models))

        await state.signOut()
        XCTAssertEqual(state.state, .signedOut)
    }

    @MainActor
    func testReauthenticationUnavailableAndFailureRemainDistinct() async {
        let client = ScriptedCodexAccountClient(
            accountResponse: .reauthenticationRequired(detail: "Expired")
        )
        let state = CodexAccountSettingsState(client: client, automaticallyPollLogin: false)

        await state.refresh()
        XCTAssertEqual(state.state, .reauthenticationRequired(detail: "Expired"))

        await client.setAccountResponse(.unavailable(detail: "Runtime missing"))
        await state.refresh()
        XCTAssertEqual(state.state, .unavailable(detail: "Runtime missing"))

        await client.failAccountRequests()
        await state.refresh()
        XCTAssertEqual(
            state.state,
            .failed(message: CodexAccountClientError.gatewayStatus(503).localizedDescription)
        )
    }

    @MainActor
    func testMissingChatGPTRouteBecomesGuidedSetupInsteadOfFailure() async {
        let client = ScriptedCodexAccountClient(accountResponse: .signedOut)
        await client.failAccountRequests(with: .gatewayStatus(404))
        let state = CodexAccountSettingsState(client: client, automaticallyPollLogin: false)

        await state.refresh()

        XCTAssertEqual(state.state, .setupRequired)
    }

    @MainActor
    func testConnectedAccountSurvivesModelCatalogFailure() async {
        let profile = CodexAccountProfile(email: nil, plan: "Team")
        let client = ScriptedCodexAccountClient(
            accountResponse: .connected(profile),
            modelsResponse: nil
        )
        let state = CodexAccountSettingsState(client: client, automaticallyPollLogin: false)

        await state.refresh()

        XCTAssertEqual(state.state, .connected(profile: profile, models: []))
        XCTAssertEqual(state.modelCatalogError, "Connected, but the model catalog could not be loaded.")
    }

    @MainActor
    func testSuccessfulAccountTransitionsNotifyTheSharedGatewayState() async {
        let client = ScriptedCodexAccountClient(accountResponse: .signedOut)
        var refreshCount = 0
        let state = CodexAccountSettingsState(
            client: client,
            automaticallyPollLogin: false,
            onAccountStateChanged: { refreshCount += 1 }
        )

        await state.refresh()
        XCTAssertEqual(refreshCount, 1)

        await state.signOut()
        XCTAssertEqual(refreshCount, 2)
    }
}

private actor ScriptedCodexAccountClient: CodexAccountClient {
    private var accountResponse: CodexAccountSnapshot
    private let modelsResponse: CodexModelsResponse?
    private let loginResponse: CodexAccountSnapshot
    private let cancelResponse: CodexAccountSnapshot
    private let logoutResponse: CodexAccountSnapshot
    private var accountError: CodexAccountClientError?
    private var cancelledLoginID: String?

    init(
        accountResponse: CodexAccountSnapshot,
        modelsResponse: CodexModelsResponse? = CodexModelsResponse(models: []),
        loginResponse: CodexAccountSnapshot = .signedOut,
        cancelResponse: CodexAccountSnapshot = .signedOut,
        logoutResponse: CodexAccountSnapshot = .signedOut
    ) {
        self.accountResponse = accountResponse
        self.modelsResponse = modelsResponse
        self.loginResponse = loginResponse
        self.cancelResponse = cancelResponse
        self.logoutResponse = logoutResponse
    }

    func account() async throws -> CodexAccountSnapshot {
        if let accountError { throw accountError }
        return accountResponse
    }

    func models() async throws -> CodexModelsResponse {
        guard let modelsResponse else { throw CodexAccountClientError.gatewayStatus(503) }
        return modelsResponse
    }

    func beginLogin(flow: CodexLoginFlow) async throws -> CodexAccountSnapshot {
        loginResponse
    }

    func cancelLogin(id: String) async throws -> CodexAccountSnapshot {
        cancelledLoginID = id
        return cancelResponse
    }

    func logout() async throws -> CodexAccountSnapshot {
        logoutResponse
    }

    func setAccountResponse(_ response: CodexAccountSnapshot) {
        accountResponse = response
        accountError = nil
    }

    func failAccountRequests(with error: CodexAccountClientError = .gatewayStatus(503)) {
        accountError = error
    }

    func lastCancelledLoginID() -> String? {
        cancelledLoginID
    }
}
