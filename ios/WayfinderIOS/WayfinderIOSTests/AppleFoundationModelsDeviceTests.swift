import XCTest

@testable import WayfinderIOS

final class AppleFoundationModelsDeviceTests: XCTestCase {
  func testEligiblePhysicalDeviceCompletesVersionedNativePrompt() async throws {
    #if targetEnvironment(simulator)
    throw XCTSkip("Apple Foundation Models evidence requires a physical device.")
    #else
    let provider = NativeAppleFoundationModelsProvider()
    let availability = await provider.availability()
    let capabilities = await provider.capabilities()
    XCTAssertEqual(
      availability,
      .available,
      "The evidence device must be eligible, enabled, and model-ready."
    )
    guard availability == .available else {
      return
    }

    let request = ProviderExecutionRequest(
      id: UUID(),
      prompt: AppleFoundationModelsPromptContract.deviceEvidencePrompt,
      destinationID: NativeAppleFoundationModelsProvider.destinationID
    )
    var responseBytes = 0
    var deltaCount = 0
    var completionCount = 0

    for try await event in await provider.stream(request) {
      switch event {
      case .delta(let delta):
        responseBytes += delta.utf8.count
        deltaCount += 1
      case .completed:
        completionCount += 1
      }
    }

    XCTAssertGreaterThan(responseBytes, 0)
    XCTAssertLessThanOrEqual(
      responseBytes,
      NativeAppleFoundationModelsProvider.maximumResponseBytes
    )
    XCTAssertGreaterThan(deltaCount, 0)
    XCTAssertEqual(completionCount, 1)
    XCTAssertTrue(capabilities.supportsText)
    XCTAssertTrue(capabilities.supportsStreaming)
    XCTAssertGreaterThan(capabilities.supportedLanguageCount, 0)
    XCTAssertNil(capabilities.contextWindow)

    let metadata = """
      prompt_contract=\(AppleFoundationModelsPromptContract.version)
      availability=\(availability.rawValue)
      supported_language_count=\(capabilities.supportedLanguageCount)
      context_window_reported=false
      response_bytes=\(responseBytes)
      delta_count=\(deltaCount)
      completion_count=\(completionCount)
      response_content_recorded=false
      """
    let attachment = XCTAttachment(
      data: Data(metadata.utf8),
      uniformTypeIdentifier: "public.plain-text"
    )
    attachment.name = "apple-foundation-models-device-evidence.txt"
    attachment.lifetime = .keepAlways
    add(attachment)
    #endif
  }
}
