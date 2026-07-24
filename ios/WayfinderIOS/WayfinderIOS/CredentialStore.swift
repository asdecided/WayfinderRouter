import Foundation
import Security

struct CredentialID: Hashable, RawRepresentable, Sendable {
  let rawValue: String

  init(rawValue: String) {
    self.rawValue = rawValue
  }
}

struct APIKeyProviderDescriptor: Hashable, Identifiable, Sendable {
  let id: CredentialID
  let displayName: String
  let detail: String
  let placeholder: String

  static let supported: [APIKeyProviderDescriptor] = [
    APIKeyProviderDescriptor(
      id: CredentialID(rawValue: "openai-platform.api-key"),
      displayName: "OpenAI Platform",
      detail: "For API usage billed through platform.openai.com",
      placeholder: "sk-…"
    ),
    APIKeyProviderDescriptor(
      id: CredentialID(rawValue: "moonshot-platform.api-key"),
      displayName: "Moonshot / Kimi Platform",
      detail: "For Moonshot Platform API access",
      placeholder: "API key"
    ),
    APIKeyProviderDescriptor(
      id: CredentialID(rawValue: "openrouter.api-key"),
      displayName: "OpenRouter",
      detail: "For models accessed through OpenRouter",
      placeholder: "sk-or-…"
    ),
  ]
}

struct ProviderCredentialStatus: Equatable, Identifiable, Sendable {
  let id: CredentialID
  let isConfigured: Bool
}

enum CredentialStoreError: LocalizedError, Equatable, Sendable {
  case invalidSecret
  case invalidStoredSecret
  case keychainUnavailable

  var errorDescription: String? {
    switch self {
    case .invalidSecret:
      "Enter a valid API key."
    case .invalidStoredSecret:
      "The saved credential could not be read."
    case .keychainUnavailable:
      "The iOS Keychain is currently unavailable."
    }
  }
}

protocol CredentialStore: Sendable {
  func save(secret: String, for id: CredentialID) async throws
  func readSecret(for id: CredentialID) async throws -> String?
  func contains(_ id: CredentialID) async throws -> Bool
  func delete(_ id: CredentialID) async throws
}

enum CredentialProtection: Hashable, Sendable {
  case whenUnlockedThisDeviceOnly
}

struct KeychainItemRequest: Hashable, Sendable {
  let service: String
  let account: String
  let protection: CredentialProtection
  let synchronizable: Bool
}

struct KeychainReadResult: Sendable {
  let status: OSStatus
  let data: Data?
}

protocol KeychainBackend: Sendable {
  func save(data: Data, request: KeychainItemRequest) async -> OSStatus
  func read(request: KeychainItemRequest) async -> KeychainReadResult
  func status(request: KeychainItemRequest) async -> OSStatus
  func delete(request: KeychainItemRequest) async -> OSStatus
}

actor SystemKeychainBackend: KeychainBackend {
  func save(data: Data, request: KeychainItemRequest) -> OSStatus {
    let status = SecItemUpdate(
      baseQuery(for: request) as CFDictionary,
      [
        kSecValueData: data,
        kSecAttrAccessible: accessibility(for: request.protection),
      ] as CFDictionary
    )

    guard status == errSecItemNotFound else {
      return status
    }

    var query = baseQuery(for: request)
    query[kSecValueData] = data
    query[kSecAttrAccessible] = accessibility(for: request.protection)
    return SecItemAdd(query as CFDictionary, nil)
  }

  func read(request: KeychainItemRequest) -> KeychainReadResult {
    var query = baseQuery(for: request)
    query[kSecReturnData] = kCFBooleanTrue
    query[kSecMatchLimit] = kSecMatchLimitOne

    var result: CFTypeRef?
    let status = SecItemCopyMatching(query as CFDictionary, &result)
    return KeychainReadResult(status: status, data: result as? Data)
  }

  func status(request: KeychainItemRequest) -> OSStatus {
    var query = baseQuery(for: request)
    query[kSecMatchLimit] = kSecMatchLimitOne
    return SecItemCopyMatching(query as CFDictionary, nil)
  }

  func delete(request: KeychainItemRequest) -> OSStatus {
    SecItemDelete(baseQuery(for: request) as CFDictionary)
  }

  private func baseQuery(
    for request: KeychainItemRequest
  ) -> [CFString: Any] {
    [
      kSecClass: kSecClassGenericPassword,
      kSecAttrService: request.service,
      kSecAttrAccount: request.account,
      kSecAttrSynchronizable: request.synchronizable
        ? kCFBooleanTrue as Any
        : kCFBooleanFalse as Any,
    ]
  }

  private func accessibility(
    for protection: CredentialProtection
  ) -> CFString {
    switch protection {
    case .whenUnlockedThisDeviceOnly:
      kSecAttrAccessibleWhenUnlockedThisDeviceOnly
    }
  }
}

actor KeychainCredentialStore: CredentialStore {
  static let defaultService = "com.wayfinder.router.ios.credentials"
  static let maximumSecretBytes = 16_384

  private let service: String
  private let backend: any KeychainBackend

  init(
    service: String = KeychainCredentialStore.defaultService,
    backend: any KeychainBackend = SystemKeychainBackend()
  ) {
    self.service = service
    self.backend = backend
  }

  func save(secret: String, for id: CredentialID) async throws {
    guard
      !secret.isEmpty,
      let data = secret.data(using: .utf8),
      data.count <= Self.maximumSecretBytes
    else {
      throw CredentialStoreError.invalidSecret
    }

    let status = await backend.save(
      data: data,
      request: request(for: id)
    )
    if status != errSecSuccess {
      throw CredentialStoreError.keychainUnavailable
    }
  }

  func readSecret(for id: CredentialID) async throws -> String? {
    let result = await backend.read(request: request(for: id))
    if result.status == errSecItemNotFound {
      return nil
    }
    guard result.status == errSecSuccess, let data = result.data else {
      throw CredentialStoreError.keychainUnavailable
    }
    guard let secret = String(data: data, encoding: .utf8) else {
      throw CredentialStoreError.invalidStoredSecret
    }
    return secret
  }

  func contains(_ id: CredentialID) async throws -> Bool {
    let status = await backend.status(request: request(for: id))
    switch status {
    case errSecSuccess:
      return true
    case errSecItemNotFound:
      return false
    default:
      throw CredentialStoreError.keychainUnavailable
    }
  }

  func delete(_ id: CredentialID) async throws {
    let status = await backend.delete(request: request(for: id))
    guard status == errSecSuccess || status == errSecItemNotFound else {
      throw CredentialStoreError.keychainUnavailable
    }
  }

  private func request(for id: CredentialID) -> KeychainItemRequest {
    KeychainItemRequest(
      service: service,
      account: id.rawValue,
      protection: .whenUnlockedThisDeviceOnly,
      synchronizable: false
    )
  }
}

actor InMemoryCredentialStore: CredentialStore {
  private var secrets: [CredentialID: String] = [:]

  func save(secret: String, for id: CredentialID) throws {
    guard
      !secret.isEmpty,
      secret.utf8.count <= KeychainCredentialStore.maximumSecretBytes
    else {
      throw CredentialStoreError.invalidSecret
    }
    secrets[id] = secret
  }

  func readSecret(for id: CredentialID) -> String? {
    secrets[id]
  }

  func contains(_ id: CredentialID) -> Bool {
    secrets[id] != nil
  }

  func delete(_ id: CredentialID) {
    secrets[id] = nil
  }
}
