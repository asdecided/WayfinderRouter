import AppKit
import SwiftUI
import XCTest
@testable import WayfinderMacCore

final class PopoverPresentationTests: XCTestCase {
    func testHostedReadinessFoldsIntoTheSingleOverallHealthState() {
        let presentation = PopoverPresentation(overview: overview(
            gateway: .running(detail: "2 configured models"),
            hosted: .checkKeys(detail: "CLOUD_KEY")
        ))

        XCTAssertEqual(presentation.overallStatus, "Degraded")
    }

    func testOfflineGatewayRemainsTheOverallState() {
        let presentation = PopoverPresentation(overview: overview(
            gateway: .offline(detail: "Offline mode keeps delivery local"),
            hosted: .disabled(detail: "Hosted delivery is disabled")
        ))

        XCTAssertEqual(presentation.overallStatus, "Offline")
    }

    func testEndpointStatusSummarisesAllConfiguredEndpoints() {
        let status = EndpointStatusPresentation(overview: overview(endpoints: [
            EndpointDisplayStatus(name: "local", providerName: "Ollama", modelName: "llama3", state: .ready),
            EndpointDisplayStatus(name: "cloud", providerName: "Anthropic", modelName: "claude", state: .checkKey),
        ]))

        XCTAssertEqual(status.summary, "1 issue")
        XCTAssertEqual(status.accessibilityLabel, "Endpoint Status, 1 issue. Ollama, Ready, Anthropic, Check Key")
        XCTAssertNil(status.emptyMessage)
    }

    func testEndpointStatusDistinguishesCheckingEmptyAndUnavailable() {
        let checking = EndpointStatusPresentation(overview: overview(
            gateway: .checking(detail: "checking")
        ))
        XCTAssertEqual(checking.summary, "Checking")
        XCTAssertEqual(checking.emptyMessage, "Checking endpoint status…")

        let empty = EndpointStatusPresentation(overview: overview())
        XCTAssertEqual(empty.summary, "None")
        XCTAssertEqual(empty.emptyMessage, "No configured endpoints")

        let unavailable = EndpointStatusPresentation(overview: overview(
            gateway: .unreachable(detail: "down"),
            hosted: .unavailable(detail: "down")
        ))
        XCTAssertEqual(unavailable.summary, "Unavailable")
        XCTAssertEqual(unavailable.emptyMessage, "Endpoint status unavailable")
    }

    func testRoutingUsesCountsAndCompositionWhenAvailable() {
        let routing = RoutingPopoverPresentation(stats: stats(
            localPercent: 0.6,
            localCount: 3,
            cloudCount: 2,
            total: 5
        ))

        XCTAssertTrue(routing.hasDecisions)
        XCTAssertEqual(routing.localFraction, 0.6)
        XCTAssertEqual(routing.totalText, "5 turns")
        XCTAssertEqual(routing.localText, "Local 3 · 60%")
        XCTAssertEqual(routing.cloudText, "Cloud 2 · 40%")
    }

    func testPanelSizingUsesTheV1WidthAndMaximumHeight() {
        XCTAssertEqual(PopoverPanelSizing.targetWidth, 340)
        XCTAssertEqual(PopoverPanelSizing.maximumHeight, 420)
        XCTAssertEqual(PopoverPanelSizing.clampedHeight(312.2), 313)
        XCTAssertEqual(PopoverPanelSizing.clampedHeight(999), 420)
    }

    func testPanelPlacementAlignsToTheStatusItemLeftEdgeAndClampsToScreen() {
        XCTAssertEqual(
            PopoverPanelPlacement.leftAlignedX(
                anchorMinX: 120,
                visibleMinX: 0,
                visibleMaxX: 1_000,
                panelWidth: 340,
                inset: 8
            ),
            120
        )
        XCTAssertEqual(
            PopoverPanelPlacement.leftAlignedX(
                anchorMinX: 900,
                visibleMinX: 0,
                visibleMaxX: 1_000,
                panelWidth: 340,
                inset: 8
            ),
            652
        )
        XCTAssertEqual(
            PopoverPanelPlacement.leftAlignedX(
                anchorMinX: 0,
                visibleMinX: 0,
                visibleMaxX: 1_000,
                panelWidth: 340,
                inset: 8
            ),
            8
        )
    }

    func testEndpointSubmenuPlacementPrefersRightThenFallsBackLeft() {
        let visible = CGRect(x: 0, y: 0, width: 1_200, height: 800)
        let size = CGSize(width: 280, height: 188)

        let right = EndpointSubmenuPlacement.frame(
            anchorFrame: CGRect(x: 300, y: 500, width: 340, height: 36),
            parentFrame: CGRect(x: 300, y: 300, width: 340, height: 300),
            visibleFrame: visible,
            size: size
        )
        XCTAssertEqual(right.origin.x, 644)
        XCTAssertEqual(right.maxY, 536)

        let left = EndpointSubmenuPlacement.frame(
            anchorFrame: CGRect(x: 850, y: 500, width: 340, height: 36),
            parentFrame: CGRect(x: 850, y: 300, width: 340, height: 300),
            visibleFrame: visible,
            size: size
        )
        XCTAssertEqual(left.origin.x, 566)
        XCTAssertEqual(left.maxY, 536)
    }

    func testEndpointSubmenuSizingClampsLongLists() {
        XCTAssertEqual(EndpointSubmenuSizing.contentHeight(rowCount: 0), 52)
        XCTAssertEqual(EndpointSubmenuSizing.contentHeight(rowCount: 5), 228)
        XCTAssertEqual(EndpointSubmenuSizing.contentHeight(rowCount: 100), 360)
    }

    @MainActor
    func testEndpointSubmenuKeyboardSelectionClampsAndResets() {
        let state = EndpointSubmenuState()

        state.present(itemCount: 3)
        XCTAssertTrue(state.isPresented)
        XCTAssertEqual(state.selectedIndex, 0)

        state.moveSelection(by: 1, itemCount: 3)
        XCTAssertEqual(state.selectedIndex, 1)
        state.moveSelection(by: 20, itemCount: 3)
        XCTAssertEqual(state.selectedIndex, 2)
        state.moveSelection(by: -20, itemCount: 3)
        XCTAssertEqual(state.selectedIndex, 0)

        state.dismiss()
        XCTAssertFalse(state.isPresented)
        XCTAssertNil(state.selectedIndex)
    }

    @MainActor
    func testRepresentativePopoverStatesFitWithoutScrolling() {
        let healthy = PopoverPresentation(overview: overview(
            stats: stats(
                localPercent: 0.6,
                localCount: 3,
                cloudCount: 2,
                total: 5
            )
        ))
        let degraded = PopoverPresentation(overview: overview(
            gateway: .degraded(detail: "Missing a required provider key; open Settings to fix it"),
            hosted: .checkKeys(detail: "CLOUD_KEY")
        ))
        let empty = PopoverPresentation(overview: overview(
            gateway: .checking(detail: "Checking gateway status"),
            hosted: .checking(detail: "Checking configured models"),
            stats: stats()
        ))

        for presentation in [healthy, degraded, empty] {
            let size = measuredSize(presentation)
            XCTAssertLessThanOrEqual(size.width, PopoverPanelSizing.targetWidth)
            XCTAssertLessThanOrEqual(size.height, PopoverPanelSizing.maximumHeight)
            XCTAssertGreaterThan(size.height, 0)
        }
    }

    private func overview(
        gateway: GatewayDisplayState = .running(detail: "2 configured models"),
        hosted: HostedDisplayState = .ready(detail: "Hosted models ready"),
        endpoints: [EndpointDisplayStatus] = [],
        stats: RoutingStats? = nil
    ) -> GatewayOverview {
        GatewayOverview(
            gateway: gateway,
            hosted: hosted,
            endpoints: endpoints,
            routingStats: stats ?? self.stats(),
            updatedAt: Date(timeIntervalSince1970: 0)
        )
    }

    private func stats(
        localPercent: Double = 0,
        localCount: Int? = nil,
        cloudCount: Int? = nil,
        total: Int? = 0
    ) -> RoutingStats {
        RoutingStats(
            localPercent: localPercent,
            cloudPercent: max(0, 1 - localPercent),
            localRouteCount: localCount,
            cloudRouteCount: cloudCount,
            totalTurns: total,
            savedToday: 0,
            savedLast30Days: 0,
            cloudSpendToday: 0,
            percentVsAlwaysCloud: 0.29,
            isPriced: false,
            hasSavings: false,
            updatedAt: Date(timeIntervalSince1970: 0),
            isRunning: true
        )
    }

    @MainActor
    private func measuredSize(_ presentation: PopoverPresentation) -> NSSize {
        let content = WayfinderPopoverContent(
            presentation: presentation,
            chatAvailability: ReleaseFeaturePolicy.current[.chat],
            onOpenChat: {},
            onEndpointAnchorFrameChange: { _ in },
            onEndpointHoverChange: { _, _ in },
            onOpenEndpointStatus: { _ in },
            onCloseEndpointStatus: {},
            onOpenSettings: {},
            onQuit: {}
        )
        .environmentObject(EndpointSubmenuState())
        let hostingController = NSHostingController(rootView: content)
        hostingController.view.frame = NSRect(
            x: 0,
            y: 0,
            width: PopoverPanelSizing.targetWidth,
            height: PopoverPanelSizing.maximumHeight
        )
        hostingController.view.layoutSubtreeIfNeeded()
        return hostingController.view.fittingSize
    }
}
