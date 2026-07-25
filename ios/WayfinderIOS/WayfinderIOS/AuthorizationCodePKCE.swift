import CryptoKit
import Foundation
import Security

enum ProviderAuthenticationMode: String, Codable, Sendable {
  case none
  case apiKey
  case oauthAuthorizationCodePKCE
  case oauthDeviceCode
  case verifiedDesktopRuntime
  case pairedHost
}

enum ProviderAccountReadiness: String, Codable, Sendable {
  case checking
  case signedOut
  case authorizing
  case ready
  case reauthenticationRequired
  case usageLimited
  case modelUnavailable
  case networkUnavailable
  case unsupportedPlatform
  case unavailable
  case failed
}

struct ProviderAccountState: Equatable, Sendable {
  let providerID: String
  let readiness: ProviderAccountReadiness
}

struct AuthorizationChallenge: Equatable, Identifiable, Sendable {
  let id: UUID
  let providerID: String
  let authorizationURL: URL
  let callbackURLScheme: String
}

protocol ProviderAccountController: Sendable {
  func state() async -> ProviderAccountState
  func beginAuthorization() async throws -> AuthorizationChallenge
  func completeAuthorization(
    _ authorizationID: UUID,
    callbackURL: URL
  ) async throws -> ProviderAccountState
  func cancelAuthorization(_ authorizationID: UUID) async
  func refresh() async -> ProviderAccountState
  func signOut() async throws
}

enum OAuthTokenRequestEncoding: Sendable {
  case json
  case form
}

struct AuthorizationCodePKCEConfiguration: Sendable {
  let providerID: String
  let authorizationEndpoint: URL
  let tokenEndpoint: URL
  let callbackURL: URL
  let clientID: String?
  let scopes: [String]
  let callbackParameterName: String
  let additionalAuthorizationParameters: [String: String]
  let tokenRequestEncoding: OAuthTokenRequestEncoding
  let credentialResponseField: String
  let credentialID: CredentialID

  init(
    providerID: String,
    authorizationEndpoint: URL,
    tokenEndpoint: URL,
    callbackURL: URL,
    clientID: String? = nil,
    scopes: [String] = [],
    callbackParameterName: String = "redirect_uri",
    additionalAuthorizationParameters: [String: String] = [:],
    tokenRequestEncoding: OAuthTokenRequestEncoding = .form,
    credentialResponseField: String = "access_token",
    credentialID: CredentialID
  ) {
    self.providerID = providerID
    self.authorizationEndpoint = authorizationEndpoint
    self.tokenEndpoint = tokenEndpoint
    self.callbackURL = callbackURL
    self.clientID = clientID
    self.scopes = scopes
    self.callbackParameterName = callbackParameterName
    self.additionalAuthorizationParameters = additionalAuthorizationParameters
    self.tokenRequestEncoding = tokenRequestEncoding
    self.credentialResponseField = credentialResponseField
    self.credentialID = credentialID
  }
}

enum AuthorizationCodePKCEError: LocalizedError, Equatable, Sendable {
  case invalidConfiguration
  case authorizationInProgress
  case noPendingAuthorization
  case callbackMismatch
  case stateMismatch
  case authorizationDenied
  case missingCode
  case invalidResponse
  case responseTooLarge
  case rejected
  case invalidCredential

  var errorDescription: String? {
    switch self {
    case .invalidConfiguration:
      "This account connection is not configured correctly."
    case .authorizationInProgress:
      "An account connection is already in progress."
    case .noPendingAuthorization:
      "This account connection has expired. Start again."
    case .callbackMismatch, .stateMismatch:
      "The account connection could not be verified."
    case .authorizationDenied:
      "The account connection was not approved."
    case .missingCode, .invalidResponse, .invalidCredential:
      "The provider returned an incomplete account connection."
    case .responseTooLarge:
      "The provider returned too much account data."
    case .rejected:
      "The provider rejected the account connection."
    }
  }
}

protocol OAuthNonceGenerator: Sendable {
  func state() throws -> String
  func verifier() throws -> String
}

struct SecureOAuthNonceGenerator: OAuthNonceGenerator {
  func state() throws -> String {
    try randomBase64URL(byteCount: 32)
  }

  func verifier() throws -> String {
    try randomBase64URL(byteCount: 48)
  }

  private func randomBase64URL(byteCount: Int) throws -> String {
    var bytes = [UInt8](repeating: 0, count: byteCount)
    guard SecRandomCopyBytes(kSecRandomDefault, bytes.count, &bytes) == errSecSuccess
    else {
      throw AuthorizationCodePKCEError.invalidConfiguration
    }
    return Data(bytes).base64URLEncodedString()
  }
}

actor AuthorizationCodePKCEController: ProviderAccountController {
  static let maximumTokenResponseBytes = 65_536

  private struct PendingAuthorization: Sendable {
    let id: UUID
    let state: String
    let verifier: String
  }

  private let configuration: AuthorizationCodePKCEConfiguration
  private let credentialStore: any CredentialStore
  private let httpClient: any HTTPDataClient
  private let nonceGenerator: any OAuthNonceGenerator
  private var pending: PendingAuthorization?
  private var currentReadiness: ProviderAccountReadiness = .checking

  init(
    configuration: AuthorizationCodePKCEConfiguration,
    credentialStore: any CredentialStore,
    httpClient: any HTTPDataClient = URLSessionHTTPDataClient(),
    nonceGenerator: any OAuthNonceGenerator = SecureOAuthNonceGenerator()
  ) {
    self.configuration = configuration
    self.credentialStore = credentialStore
    self.httpClient = httpClient
    self.nonceGenerator = nonceGenerator
  }

  func state() -> ProviderAccountState {
    snapshot()
  }

  func beginAuthorization() throws -> AuthorizationChallenge {
    guard pending == nil else {
      throw AuthorizationCodePKCEError.authorizationInProgress
    }
    guard
      configuration.authorizationEndpoint.scheme == "https",
      configuration.tokenEndpoint.scheme == "https",
      let callbackScheme = configuration.callbackURL.scheme,
      !callbackScheme.isEmpty
    else {
      throw AuthorizationCodePKCEError.invalidConfiguration
    }

    let state = try nonceGenerator.state()
    let verifier = try nonceGenerator.verifier()
    guard
      !state.isEmpty,
      (43...128).contains(verifier.count),
      verifier.unicodeScalars.allSatisfy(Self.isValidVerifierCharacter)
    else {
      throw AuthorizationCodePKCEError.invalidConfiguration
    }

    let id = UUID()
    let pending = PendingAuthorization(
      id: id,
      state: state,
      verifier: verifier
    )
    self.pending = pending
    currentReadiness = .authorizing

    return AuthorizationChallenge(
      id: id,
      providerID: configuration.providerID,
      authorizationURL: try authorizationURL(for: pending),
      callbackURLScheme: callbackScheme
    )
  }

  func completeAuthorization(
    _ authorizationID: UUID,
    callbackURL: URL
  ) async throws -> ProviderAccountState {
    guard let pending, pending.id == authorizationID else {
      throw AuthorizationCodePKCEError.noPendingAuthorization
    }

    do {
      let code = try authorizationCode(
        callbackURL: callbackURL,
        expectedState: pending.state
      )
      let credential = try await exchange(
        code: code,
        verifier: pending.verifier
      )
      try await credentialStore.save(
        secret: credential,
        for: configuration.credentialID
      )
      self.pending = nil
      currentReadiness = .ready
      return snapshot()
    } catch {
      self.pending = nil
      currentReadiness = .failed
      throw error
    }
  }

  func cancelAuthorization(_ authorizationID: UUID) {
    guard pending?.id == authorizationID else {
      return
    }
    pending = nil
    currentReadiness = .signedOut
  }

  func refresh() async -> ProviderAccountState {
    do {
      currentReadiness = try await credentialStore.contains(
        configuration.credentialID
      ) ? .ready : .signedOut
    } catch {
      currentReadiness = .failed
    }
    return snapshot()
  }

  func signOut() async throws {
    pending = nil
    try await credentialStore.delete(configuration.credentialID)
    currentReadiness = .signedOut
  }

  private func snapshot() -> ProviderAccountState {
    ProviderAccountState(
      providerID: configuration.providerID,
      readiness: currentReadiness
    )
  }

  private func authorizationURL(
    for pending: PendingAuthorization
  ) throws -> URL {
    guard var components = URLComponents(
      url: configuration.authorizationEndpoint,
      resolvingAgainstBaseURL: false
    ) else {
      throw AuthorizationCodePKCEError.invalidConfiguration
    }

    var queryItems = components.queryItems ?? []
    if let clientID = configuration.clientID {
      queryItems.append(URLQueryItem(name: "client_id", value: clientID))
    }
    queryItems.append(
      URLQueryItem(
        name: configuration.callbackParameterName,
        value: configuration.callbackURL.absoluteString
      )
    )
    if !configuration.scopes.isEmpty {
      queryItems.append(
        URLQueryItem(name: "scope", value: configuration.scopes.joined(separator: " "))
      )
    }
    queryItems.append(URLQueryItem(name: "state", value: pending.state))
    queryItems.append(
      URLQueryItem(
        name: "code_challenge",
        value: Self.challenge(for: pending.verifier)
      )
    )
    queryItems.append(URLQueryItem(name: "code_challenge_method", value: "S256"))
    for (name, value) in configuration.additionalAuthorizationParameters.sorted(
      by: { $0.key < $1.key }
    ) {
      queryItems.append(URLQueryItem(name: name, value: value))
    }
    components.queryItems = queryItems

    guard let url = components.url else {
      throw AuthorizationCodePKCEError.invalidConfiguration
    }
    return url
  }

  private func authorizationCode(
    callbackURL: URL,
    expectedState: String
  ) throws -> String {
    guard
      callbackURL.scheme == configuration.callbackURL.scheme,
      callbackURL.host == configuration.callbackURL.host,
      callbackURL.path == configuration.callbackURL.path,
      callbackURL.fragment == nil
    else {
      throw AuthorizationCodePKCEError.callbackMismatch
    }

    guard let components = URLComponents(
      url: callbackURL,
      resolvingAgainstBaseURL: false
    ) else {
      throw AuthorizationCodePKCEError.invalidResponse
    }
    let values = Dictionary(grouping: components.queryItems ?? [], by: \.name)
    if values["error"]?.isEmpty == false {
      throw AuthorizationCodePKCEError.authorizationDenied
    }
    guard
      let states = values["state"]?.compactMap(\.value),
      states.count == 1,
      states[0] == expectedState
    else {
      throw AuthorizationCodePKCEError.stateMismatch
    }
    guard
      let codes = values["code"]?.compactMap(\.value),
      codes.count == 1,
      !codes[0].isEmpty
    else {
      throw AuthorizationCodePKCEError.missingCode
    }
    return codes[0]
  }

  private func exchange(
    code: String,
    verifier: String
  ) async throws -> String {
    var fields = [
      "code": code,
      "code_verifier": verifier,
      "redirect_uri": configuration.callbackURL.absoluteString,
    ]
    if let clientID = configuration.clientID {
      fields["client_id"] = clientID
    }

    var request = URLRequest(url: configuration.tokenEndpoint)
    request.httpMethod = "POST"
    request.timeoutInterval = 30
    request.setValue("application/json", forHTTPHeaderField: "Accept")
    switch configuration.tokenRequestEncoding {
    case .json:
      request.setValue("application/json", forHTTPHeaderField: "Content-Type")
      request.httpBody = try JSONSerialization.data(
        withJSONObject: fields,
        options: [.sortedKeys]
      )
    case .form:
      request.setValue(
        "application/x-www-form-urlencoded",
        forHTTPHeaderField: "Content-Type"
      )
      var components = URLComponents()
      components.queryItems = fields.sorted(by: { $0.key < $1.key }).map {
        URLQueryItem(name: $0.key, value: $0.value)
      }
      request.httpBody = components.percentEncodedQuery?.data(using: .utf8)
    }

    let response = try await httpClient.data(request)
    guard (200...299).contains(response.statusCode) else {
      throw AuthorizationCodePKCEError.rejected
    }
    guard response.body.count <= Self.maximumTokenResponseBytes else {
      throw AuthorizationCodePKCEError.responseTooLarge
    }
    guard
      let payload = try? JSONSerialization.jsonObject(with: response.body)
        as? [String: Any],
      let credential = payload[configuration.credentialResponseField] as? String,
      !credential.isEmpty,
      credential.utf8.count <= KeychainCredentialStore.maximumSecretBytes
    else {
      throw AuthorizationCodePKCEError.invalidCredential
    }
    return credential
  }

  private static func challenge(for verifier: String) -> String {
    Data(SHA256.hash(data: Data(verifier.utf8))).base64URLEncodedString()
  }

  private static func isValidVerifierCharacter(
    _ scalar: Unicode.Scalar
  ) -> Bool {
    scalar.properties.isAlphabetic
      || scalar.properties.numericType != nil
      || scalar == "-"
      || scalar == "."
      || scalar == "_"
      || scalar == "~"
  }
}

private extension Data {
  func base64URLEncodedString() -> String {
    base64EncodedString()
      .replacingOccurrences(of: "+", with: "-")
      .replacingOccurrences(of: "/", with: "_")
      .replacingOccurrences(of: "=", with: "")
  }
}
