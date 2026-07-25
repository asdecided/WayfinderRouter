import Foundation
import OSLog

/// Signposts for the three paths whose cost is only visible on a device
/// (UX-021): launch, thread restore, and stream-delta application.
///
/// Instruments reads these; the numeric budgets they serve — cold launch under
/// 400 ms, no dropped frames at 120 Hz — are Tier-2 checks recorded as pending
/// rows in `docs/mobile-fidelity.md`. Nothing here claims those budgets are met.
enum WayfinderSignposts {
  static let subsystem = "com.wayfinder.router.ios"

  // `OSSignposter` is documented as safe to use from any thread. The explicit
  // annotation keeps that promise legible under strict concurrency rather than
  // depending on the SDK's conformance.
  nonisolated(unsafe) static let launch = OSSignposter(
    subsystem: subsystem,
    category: "launch"
  )
  nonisolated(unsafe) static let conversation = OSSignposter(
    subsystem: subsystem,
    category: "conversation"
  )
  nonisolated(unsafe) static let streaming = OSSignposter(
    subsystem: subsystem,
    category: "streaming"
  )
}

/// How often a streaming reply is written to durable storage.
///
/// The audited build re-encoded and saved the whole thread on every delta,
/// which at real token rates is a per-token write storm. Deltas now only touch
/// memory; storage is checkpointed on a bounded cadence and always on a
/// terminal state, so an interrupted reply still reconciles correctly on the
/// next launch.
struct StreamingCheckpointPolicy: Equatable, Sendable {
  /// Checkpoint after this many deltas, whatever the clock says.
  var deltaInterval: Int
  /// Checkpoint once this much time has passed, however few deltas arrived.
  var minimumInterval: Duration

  static let standard = StreamingCheckpointPolicy(
    deltaInterval: 32,
    minimumInterval: .milliseconds(750)
  )

  /// Writes on every delta. Only for tests that read persisted mid-stream
  /// content back out of the store.
  static let everyDelta = StreamingCheckpointPolicy(
    deltaInterval: 1,
    minimumInterval: .zero
  )
}
