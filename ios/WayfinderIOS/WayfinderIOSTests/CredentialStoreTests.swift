import Security
import XCTest

@testable import WayfinderIOS

final class CredentialStoreTests: XCTestCase {
  func testKeychainCreateReadUpdateDeleteLifecycle() async throws {
    let service = "com.wayfinder.router.tests.\(UUID().uuidString)"
    let backend = InMemoryKeychainBackend()
    let store = KeychainCredentialStore(service: service, backend: backend)
    let id = CredentialID(rawValue: "provider.api-key")

    let initiallyConfigured = try await store.contains(id)
    let initialSecret = try await store.readSecret(for: id)
    XCTAssertFalse(initiallyConfigured)
    XCTAssertNil(initialSecret)

    try await store.save(secret: "first-secret", for: id)
    let configured = try await store.contains(id)
    let firstSecret = try await store.readSecret(for: id)
    XCTAssertTrue(configured)
    XCTAssertEqual(firstSecret, "first-secret")

    try await store.save(secret: "replacement-secret", for: id)
    let replacementSecret = try await store.readSecret(for: id)
    XCTAssertEqual(replacementSecret, "replacement-secret")

    try await store.delete(id)
    let configuredAfterDelete = try await store.contains(id)
    let secretAfterDelete = try await store.readSecret(for: id)
    XCTAssertFalse(configuredAfterDelete)
    XCTAssertNil(secretAfterDelete)
  }

  func testKeychainItemUsesDeviceOnlyNonSynchronizingProtection() async throws {
    let service = "com.wayfinder.router.tests.\(UUID().uuidString)"
    let backend = InMemoryKeychainBackend()
    let store = KeychainCredentialStore(service: service, backend: backend)
    let id = CredentialID(rawValue: "provider.api-key")
    try await store.save(secret: "bounded-secret", for: id)

    let savedRequest = await backend.lastSavedRequest()
    let request = try XCTUnwrap(savedRequest)
    XCTAssertEqual(
      request,
      KeychainItemRequest(
        service: service,
        account: id.rawValue,
        protection: .whenUnlockedThisDeviceOnly,
        synchronizable: false
      )
    )
  }

  func testSystemKeychainSmokeWhenTestHostIsSigned() async throws {
    let service = "com.wayfinder.router.tests.\(UUID().uuidString)"
    let store = KeychainCredentialStore(service: service)
    let id = CredentialID(rawValue: "provider.api-key")
    defer {
      deleteKeychainItem(service: service, id: id)
    }

    do {
      try await store.save(secret: "system-keychain-secret", for: id)
    } catch CredentialStoreError.keychainUnavailable {
      throw XCTSkip(
        "The unsigned CI test host cannot access the simulator Keychain."
      )
    }

    let stored = try await store.readSecret(for: id)
    XCTAssertEqual(stored, "system-keychain-secret")
    try await store.delete(id)
  }

  func testCredentialStoreRejectsEmptyAndOversizedSecrets() async {
    let store = InMemoryCredentialStore()
    let id = CredentialID(rawValue: "provider.api-key")

    await XCTAssertThrowsErrorAsync(
      try await store.save(secret: "", for: id)
    ) { error in
      XCTAssertEqual(error as? CredentialStoreError, .invalidSecret)
    }

    let oversized = String(
      repeating: "x",
      count: KeychainCredentialStore.maximumSecretBytes + 1
    )
    await XCTAssertThrowsErrorAsync(
      try await store.save(secret: oversized, for: id)
    ) { error in
      XCTAssertEqual(error as? CredentialStoreError, .invalidSecret)
    }
  }

  func testDeletingMissingCredentialIsIdempotent() async throws {
    let store = InMemoryCredentialStore()
    let id = CredentialID(rawValue: "missing.api-key")

    await store.delete(id)
    await store.delete(id)

    let configured = await store.contains(id)
    XCTAssertFalse(configured)
  }

  private func deleteKeychainItem(service: String, id: CredentialID) {
    SecItemDelete(
      [
        kSecClass: kSecClassGenericPassword,
        kSecAttrService: service,
        kSecAttrAccount: id.rawValue,
      ] as CFDictionary
    )
  }
}

private actor InMemoryKeychainBackend: KeychainBackend {
  private var records: [KeychainItemRequest: Data] = [:]
  private var savedRequest: KeychainItemRequest?

  func save(data: Data, request: KeychainItemRequest) -> OSStatus {
    records[request] = data
    savedRequest = request
    return errSecSuccess
  }

  func read(request: KeychainItemRequest) -> KeychainReadResult {
    guard let data = records[request] else {
      return KeychainReadResult(status: errSecItemNotFound, data: nil)
    }
    return KeychainReadResult(status: errSecSuccess, data: data)
  }

  func status(request: KeychainItemRequest) -> OSStatus {
    records[request] == nil ? errSecItemNotFound : errSecSuccess
  }

  func delete(request: KeychainItemRequest) -> OSStatus {
    records.removeValue(forKey: request) == nil
      ? errSecItemNotFound
      : errSecSuccess
  }

  func lastSavedRequest() -> KeychainItemRequest? {
    savedRequest
  }
}

private func XCTAssertThrowsErrorAsync<T>(
  _ expression: @autoclosure () async throws -> T,
  _ errorHandler: (Error) -> Void,
  file: StaticString = #filePath,
  line: UInt = #line
) async {
  do {
    _ = try await expression()
    XCTFail("Expected expression to throw", file: file, line: line)
  } catch {
    errorHandler(error)
  }
}
