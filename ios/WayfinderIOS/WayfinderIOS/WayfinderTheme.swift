import SwiftUI
import UIKit

/// Semantic design tokens for the mobile client.
///
/// Every appearance is designed rather than derived: light, dark, and the
/// increased-contrast variants of both carry their own values. Colours are
/// declared as trait-resolved `UIColor`s instead of asset-catalog lookups so
/// the same definitions are reachable from unit tests, which assert their
/// contrast ratios (`WayfinderThemeContrastTests`). The asset catalogue keeps
/// only the two colour sets the system resolves by name — the global accent
/// and the launch-screen background.
///
/// Route-boundary colour is the one place colour carries meaning: local and
/// hosted execution read differently so the product's core concept has a
/// colour language. Per WF-DESIGN-0020 that language belongs to receipts and
/// destination identity, never to ambient decoration.
enum WayfinderTheme {

  // MARK: - Brand and routing identity

  /// Brand green. Used for active actions and routing identity only.
  ///
  /// The shipped literal (#15AB75) sits at ~2.9:1 on white, below the 4.5:1
  /// floor for the small text it tints, so the light variants are darkened
  /// and the dark variants lightened until every text-bearing use passes AA.
  static let accentUIColor = dynamicColor(
    light: 0x0E_82_59,
    dark: 0x35_C4_8C,
    lightIncreasedContrast: 0x09_55_39,
    darkIncreasedContrast: 0x5F_E0_AF
  )

  /// Foreground for content drawn on top of an ``accent`` fill.
  static let onAccentUIColor = dynamicColor(
    light: 0xFF_FF_FF,
    dark: 0x04_1A_11,
    lightIncreasedContrast: 0xFF_FF_FF,
    darkIncreasedContrast: 0x00_0F_08
  )

  /// On-device execution. Shares the brand hue: local *is* the identity.
  static let routeLocalUIColor = accentUIColor

  /// Hosted execution, ported from the macOS theme's cloud amber and
  /// re-darkened for iOS light mode, where it labels small caption text.
  static let routeCloudUIColor = dynamicColor(
    light: 0x96_56_0A,
    dark: 0xE8_A2_4A,
    lightIncreasedContrast: 0x6E_3F_05,
    darkIncreasedContrast: 0xF5_C0_77
  )

  /// Trusted local-network execution, between on-device and hosted.
  static let routeLocalNetworkUIColor = dynamicColor(
    light: 0x1F_5F_8B,
    dark: 0x6F_B5_E4,
    lightIncreasedContrast: 0x16_46_67,
    darkIncreasedContrast: 0x9C_D2_F2
  )

  // MARK: - Surfaces and chrome

  /// Drawer scrim. Designed per appearance instead of one black opacity that
  /// reads as a grey haze in dark mode.
  static let scrimUIColor = UIColor { traits in
    traits.userInterfaceStyle == .dark
      ? UIColor(white: 0, alpha: 0.52)
      : UIColor(white: 0, alpha: 0.28)
  }

  /// Hairline for the composer and other elevated surfaces.
  static let hairlineUIColor = UIColor { traits in
    switch (traits.userInterfaceStyle, traits.accessibilityContrast) {
    case (.dark, .high): UIColor(white: 1, alpha: 0.42)
    case (.dark, _): UIColor(white: 1, alpha: 0.14)
    case (_, .high): UIColor(white: 0, alpha: 0.42)
    default: UIColor(white: 0, alpha: 0.10)
    }
  }

  /// Elevation shadow. Dark mode uses elevation-by-surface, not drop shadow.
  static let shadowUIColor = UIColor { traits in
    traits.userInterfaceStyle == .dark
      ? UIColor(white: 0, alpha: 0)
      : UIColor(white: 0, alpha: 0.10)
  }

  // MARK: - SwiftUI accessors

  static var accent: Color { Color(uiColor: accentUIColor) }
  static var onAccent: Color { Color(uiColor: onAccentUIColor) }
  static var routeLocal: Color { Color(uiColor: routeLocalUIColor) }
  static var routeCloud: Color { Color(uiColor: routeCloudUIColor) }
  static var routeLocalNetwork: Color { Color(uiColor: routeLocalNetworkUIColor) }
  static var scrim: Color { Color(uiColor: scrimUIColor) }
  static var hairline: Color { Color(uiColor: hairlineUIColor) }
  static var shadow: Color { Color(uiColor: shadowUIColor) }

  /// Canvas behind the transcript.
  static var canvas: Color { Color(uiColor: .systemBackground) }
  /// Quiet raised surface: user bubbles, the composer.
  static var raisedSurface: Color { Color(uiColor: .secondarySystemBackground) }
  /// The surface used for controls sitting on ``raisedSurface``.
  static var controlSurface: Color { Color(uiColor: .tertiarySystemFill) }

  // MARK: - Construction

  private static func dynamicColor(
    light: Int,
    dark: Int,
    lightIncreasedContrast: Int,
    darkIncreasedContrast: Int
  ) -> UIColor {
    UIColor { traits in
      switch (traits.userInterfaceStyle, traits.accessibilityContrast) {
      case (.dark, .high): UIColor(rgb: darkIncreasedContrast)
      case (.dark, _): UIColor(rgb: dark)
      case (_, .high): UIColor(rgb: lightIncreasedContrast)
      default: UIColor(rgb: light)
      }
    }
  }
}

extension UIColor {
  fileprivate convenience init(rgb: Int) {
    self.init(
      red: CGFloat((rgb >> 16) & 0xFF) / 255.0,
      green: CGFloat((rgb >> 8) & 0xFF) / 255.0,
      blue: CGFloat(rgb & 0xFF) / 255.0,
      alpha: 1
    )
  }
}

// MARK: - Route boundary semantics

extension ExecutionBoundary {
  /// The colour that identifies this execution boundary.
  var routeColor: Color {
    switch self {
    case .onDevice: WayfinderTheme.routeLocal
    case .localNetwork: WayfinderTheme.routeLocalNetwork
    case .hosted: WayfinderTheme.routeCloud
    }
  }

  /// The glyph that identifies this execution boundary. Colour never carries
  /// the distinction alone.
  var routeSymbolName: String {
    switch self {
    case .onDevice: "iphone"
    case .localNetwork: "wifi"
    case .hosted: "cloud"
    }
  }

  /// Execution-boundary language for receipts, in the contract's "Ran …" form.
  var receiptPhrase: String {
    switch self {
    case .onDevice: "Ran on this device"
    case .localNetwork: "Ran on a local device"
    case .hosted: "Ran in hosted cloud"
    }
  }
}

// MARK: - Spacing

/// An 8 pt spacing grid. Replaces the per-call literals the audit found.
enum WayfinderSpacing {
  /// 4 pt — the only permitted half step, for text-to-caption pairs.
  static let hairline: CGFloat = 4
  static let xSmall: CGFloat = 8
  static let small: CGFloat = 12
  static let medium: CGFloat = 16
  static let large: CGFloat = 24
  static let xLarge: CGFloat = 32
  static let xxLarge: CGFloat = 40
}

enum WayfinderRadius {
  static let small: CGFloat = 10
  static let medium: CGFloat = 16
  static let large: CGFloat = 20
  static let composer: CGFloat = 24
}

/// Minimum hit target, per the HIG. Composer controls scale from this floor.
enum WayfinderMetrics {
  static let minimumHitTarget: CGFloat = 44
  static let readableWidth: CGFloat = 760
}

// MARK: - Motion

/// Every state change animates with a spring. `.linear` and the default
/// `.easeInOut` are deliberately absent.
enum WayfinderMotion {
  /// Drawer open/close. Interruptible: a spring retargets mid-gesture.
  static let drawer = Animation.spring(response: 0.38, dampingFraction: 0.86)
  /// Transcript growth and scroll correction.
  static let transcript = Animation.spring(response: 0.32, dampingFraction: 0.92)
  /// Small control state changes: send/stop, chips, badges.
  static let control = Animation.spring(response: 0.26, dampingFraction: 0.8)
  /// Content appearing or leaving, such as the scroll-to-bottom pill.
  static let reveal = Animation.spring(response: 0.3, dampingFraction: 0.82)

  /// The Reduce Motion substitute: an explicit short crossfade, never a
  /// removed transition — state changes must stay visible.
  static let crossfade = Animation.easeOut(duration: 0.15)

  /// Resolves an animation against the user's Reduce Motion setting.
  static func resolved(
    _ animation: Animation,
    reduceMotion: Bool
  ) -> Animation {
    reduceMotion ? crossfade : animation
  }
}

extension View {
  /// Applies a spring animation that degrades to a crossfade under Reduce
  /// Motion. Use instead of `.animation(_:value:)` for app state changes.
  func wayfinderAnimation<V: Equatable>(
    _ animation: Animation,
    value: V,
    reduceMotion: Bool
  ) -> some View {
    self.animation(
      WayfinderMotion.resolved(animation, reduceMotion: reduceMotion),
      value: value
    )
  }
}
