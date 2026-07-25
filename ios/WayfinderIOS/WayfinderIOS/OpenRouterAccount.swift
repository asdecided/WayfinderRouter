import AuthenticationServices
import Foundation
import UIKit

extension AuthorizationCodePKCEConfiguration {
  static let openRouter = AuthorizationCodePKCEConfiguration(
    providerID: "openrouter",
    authorizationEndpoint: URL(string: "https://openrouter.ai/auth")!,
    tokenEndpoint: URL(string: "https://openrouter.ai/api/v1/auth/keys")!,
    callbackURL: URL(
      string:
        "https://github.com/asdecided/WayfinderRouter/oauth/openrouter"
    )!,
    callbackParameterName: "callback_url",
    stateTransport: .callbackPathComponent,
    tokenRequestEncoding: .json,
    includeCallbackInTokenRequest: false,
    additionalTokenParameters: ["code_challenge_method": "S256"],
    credentialResponseField: "key",
    credentialID: OpenAICompatibleConfiguration.openRouter.credentialID
  )
}

@MainActor
final class SystemWebAuthenticationSession: NSObject,
  ASWebAuthenticationPresentationContextProviding
{
  private var session: ASWebAuthenticationSession?
  private var continuation: CheckedContinuation<URL, Error>?

  func authenticate(_ challenge: AuthorizationChallenge) async throws -> URL {
    cancel()

    return try await withTaskCancellationHandler {
      try await withCheckedThrowingContinuation { continuation in
        self.continuation = continuation
        let callback: ASWebAuthenticationSession.Callback
        switch challenge.callback {
        case .customScheme(let scheme):
          callback = .customScheme(scheme)
        case .https(let host, let path):
          callback = .https(host: host, path: path)
        }

        let session = ASWebAuthenticationSession(
          url: challenge.authorizationURL,
          callback: callback
        ) { [weak self] callbackURL, error in
          Task { @MainActor [weak self] in
            guard let self else {
              return
            }
            self.session = nil
            let continuation = self.continuation
            self.continuation = nil

            if let callbackURL {
              continuation?.resume(returning: callbackURL)
            } else if let error {
              continuation?.resume(throwing: error)
            } else {
              continuation?.resume(throwing: CancellationError())
            }
          }
        }
        session.presentationContextProvider = self
        session.prefersEphemeralWebBrowserSession = false
        self.session = session

        guard session.start() else {
          self.session = nil
          self.continuation = nil
          continuation.resume(
            throwing: AuthorizationCodePKCEError.invalidConfiguration
          )
          return
        }
      }
    } onCancel: {
      Task { @MainActor [weak self] in
        self?.cancel()
      }
    }
  }

  func cancel() {
    session?.cancel()
    session = nil
    let continuation = continuation
    self.continuation = nil
    continuation?.resume(throwing: CancellationError())
  }

  func presentationAnchor(
    for session: ASWebAuthenticationSession
  ) -> ASPresentationAnchor {
    let scenes = UIApplication.shared.connectedScenes.compactMap {
      $0 as? UIWindowScene
    }
    if let keyWindow = scenes.flatMap(\.windows).first(where: \.isKeyWindow) {
      return keyWindow
    }
    return scenes.first?.windows.first ?? ASPresentationAnchor()
  }
}
