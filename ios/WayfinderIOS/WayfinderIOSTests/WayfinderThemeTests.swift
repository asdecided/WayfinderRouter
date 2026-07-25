import UIKit
import WayfinderRoutingBridge
import XCTest

@testable import WayfinderIOS

/// UX-011, UX-025. The audit measured the shipped accent at ~2.9:1 on white
/// while it tinted caption-weight text. These tests hold every text-bearing
/// theme colour to WCAG AA in all four appearances, so the claim is checked by
/// CI instead of asserted from inspection.
final class WayfinderThemeContrastTests: XCTestCase {

  // MARK: - Appearances

  private struct Appearance {
    let name: String
    let traits: UITraitCollection
    /// AA for small text. Increased-contrast appearances are held higher,
    /// since a user who asks for more contrast must actually receive it.
    let minimumRatio: Double
  }

  private var appearances: [Appearance] {
    [
      Appearance(name: "light", traits: traits(.light, .normal), minimumRatio: 4.5),
      Appearance(name: "dark", traits: traits(.dark, .normal), minimumRatio: 4.5),
      Appearance(
        name: "light increased contrast",
        traits: traits(.light, .high),
        minimumRatio: 7.0
      ),
      Appearance(
        name: "dark increased contrast",
        traits: traits(.dark, .high),
        minimumRatio: 7.0
      ),
    ]
  }

  // MARK: - Tests

  func testAccentMeetsSmallTextContrastInEveryAppearance() {
    for appearance in appearances {
      assertContrast(
        WayfinderTheme.accentUIColor,
        on: UIColor.systemBackground,
        appearance: appearance,
        label: "accent"
      )
    }
  }

  func testAccentAlsoPassesOnTheRaisedSurfaceItIsUsedOn() {
    // The receipt chip and the composer sit on secondarySystemBackground,
    // not on the canvas.
    for appearance in appearances {
      assertContrast(
        WayfinderTheme.accentUIColor,
        on: UIColor.secondarySystemBackground,
        appearance: appearance,
        label: "accent on raised surface"
      )
    }
  }

  func testSendGlyphContrastsWithItsAccentFill() {
    // Replaces the hardcoded Color.white glyph the audit flagged.
    for appearance in appearances {
      assertContrast(
        WayfinderTheme.onAccentUIColor,
        on: WayfinderTheme.accentUIColor,
        appearance: appearance,
        label: "onAccent over accent"
      )
    }
  }

  func testRouteBoundaryColoursMeetSmallTextContrast() {
    let boundaries: [(String, UIColor)] = [
      ("routeLocal", WayfinderTheme.routeLocalUIColor),
      ("routeLocalNetwork", WayfinderTheme.routeLocalNetworkUIColor),
      ("routeCloud", WayfinderTheme.routeCloudUIColor),
    ]

    for (label, color) in boundaries {
      for appearance in appearances {
        assertContrast(
          color,
          on: UIColor.systemBackground,
          appearance: appearance,
          label: label
        )
        assertContrast(
          color,
          on: UIColor.secondarySystemBackground,
          appearance: appearance,
          label: "\(label) on raised surface"
        )
      }
    }
  }

  func testRouteBoundariesAreDistinguishableFromEachOther() {
    // Boundary is the product's core concept; local and hosted must not read
    // as the same colour. Colour never carries the distinction alone — every
    // boundary also has its own glyph — but it must still be visible.
    for appearance in appearances {
      let local = luminance(WayfinderTheme.routeLocalUIColor, appearance.traits)
      let cloud = luminance(WayfinderTheme.routeCloudUIColor, appearance.traits)
      let network = luminance(
        WayfinderTheme.routeLocalNetworkUIColor,
        appearance.traits
      )
      XCTAssertNotEqual(
        components(WayfinderTheme.routeLocalUIColor, appearance.traits),
        components(WayfinderTheme.routeCloudUIColor, appearance.traits),
        "local and hosted share a value in \(appearance.name)"
      )
      XCTAssertNotEqual(
        components(WayfinderTheme.routeLocalUIColor, appearance.traits),
        components(WayfinderTheme.routeLocalNetworkUIColor, appearance.traits),
        "local and local-network share a value in \(appearance.name)"
      )
      XCTAssertTrue(
        local.isFinite && cloud.isFinite && network.isFinite,
        "boundary luminance is not finite in \(appearance.name)"
      )
    }
  }

  func testEveryBoundaryHasItsOwnGlyph() {
    let symbols = [
      ExecutionBoundary.onDevice.routeSymbolName,
      ExecutionBoundary.localNetwork.routeSymbolName,
      ExecutionBoundary.hosted.routeSymbolName,
    ]
    XCTAssertEqual(Set(symbols).count, symbols.count)
    for symbol in symbols {
      XCTAssertNotNil(
        UIImage(systemName: symbol),
        "\(symbol) is not a system symbol"
      )
    }
  }

  func testReceiptPhrasesUseTheContractExecutionGrammar() {
    // UX-010: WF-DESIGN-0020 prescribes "Ran …", not "Routed to …".
    for boundary in [
      ExecutionBoundary.onDevice, .localNetwork, .hosted,
    ] {
      XCTAssertTrue(
        boundary.receiptPhrase.hasPrefix("Ran "),
        "\(boundary) does not use the \"Ran …\" receipt grammar"
      )
    }
  }

  func testIncreasedContrastIsStrictlyStrongerThanTheDefaultAppearance() {
    let pairs: [(String, UIColor)] = [
      ("accent", WayfinderTheme.accentUIColor),
      ("routeCloud", WayfinderTheme.routeCloudUIColor),
      ("routeLocalNetwork", WayfinderTheme.routeLocalNetworkUIColor),
    ]

    for (label, color) in pairs {
      for style in [UIUserInterfaceStyle.light, .dark] {
        let normal = contrastRatio(
          color,
          UIColor.systemBackground,
          traits(style, .normal)
        )
        let high = contrastRatio(
          color,
          UIColor.systemBackground,
          traits(style, .high)
        )
        XCTAssertGreaterThan(
          high,
          normal,
          "\(label) does not gain contrast under increased contrast"
        )
      }
    }
  }

  // MARK: - Helpers

  private func assertContrast(
    _ foreground: UIColor,
    on background: UIColor,
    appearance: Appearance,
    label: String,
    file: StaticString = #filePath,
    line: UInt = #line
  ) {
    let ratio = contrastRatio(foreground, background, appearance.traits)
    XCTAssertGreaterThanOrEqual(
      ratio,
      appearance.minimumRatio,
      """
      \(label) is \(String(format: "%.2f", ratio)):1 in \(appearance.name), \
      below the required \(appearance.minimumRatio):1
      """,
      file: file,
      line: line
    )
  }

  // `UITraitCollection`'s composing initialisers are soft-deprecated in favour
  // of `init(mutations:)`; the older form is kept here because it is the one
  // shape that is unambiguous across SDKs.
  private func traits(
    _ style: UIUserInterfaceStyle,
    _ contrast: UIAccessibilityContrast
  ) -> UITraitCollection {
    UITraitCollection(traitsFrom: [
      UITraitCollection(userInterfaceStyle: style),
      UITraitCollection(accessibilityContrast: contrast),
    ])
  }

  private func components(
    _ color: UIColor,
    _ traits: UITraitCollection
  ) -> [CGFloat] {
    var red: CGFloat = 0
    var green: CGFloat = 0
    var blue: CGFloat = 0
    var alpha: CGFloat = 0
    color.resolvedColor(with: traits)
      .getRed(&red, green: &green, blue: &blue, alpha: &alpha)
    return [red, green, blue, alpha]
  }

  private func luminance(
    _ color: UIColor,
    _ traits: UITraitCollection
  ) -> Double {
    let parts = components(color, traits)

    func channel(_ value: CGFloat) -> Double {
      let value = Double(value)
      return value <= 0.03928
        ? value / 12.92
        : pow((value + 0.055) / 1.055, 2.4)
    }

    return 0.2126 * channel(parts[0])
      + 0.7152 * channel(parts[1])
      + 0.0722 * channel(parts[2])
  }

  private func contrastRatio(
    _ first: UIColor,
    _ second: UIColor,
    _ traits: UITraitCollection
  ) -> Double {
    let a = luminance(first, traits)
    let b = luminance(second, traits)
    let lighter = max(a, b)
    let darker = min(a, b)
    return (lighter + 0.05) / (darker + 0.05)
  }
}
