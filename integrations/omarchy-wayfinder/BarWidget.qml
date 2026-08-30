import QtQuick
import Quickshell
import qs.Commons
import qs.Ui
import "Model.js" as Model

BarWidget {
  id: root
  moduleName: "io.github.asdecided.wayfinder"

  readonly property var wayfinder: bar && bar.shell ? bar.shell.serviceFor(moduleName) : null
  property bool popupOpen: false
  property bool rollbackArmed: false
  property double nowMs: Date.now()

  readonly property color foreground: bar ? bar.foreground : Color.foreground
  readonly property color statusColor: !wayfinder || !wayfinder.reachable
    ? Color.urgent
    : (wayfinder.degraded ? Color.urgent : Color.accent)
  readonly property var recentEntries: wayfinder && wayfinder.recentReport.valid
    ? wayfinder.recentReport.recent.slice(0, 4) : []
  readonly property var actionableReceipt: Model.latestActionableReceipt(recentEntries)
  readonly property var visibleModels: wayfinder ? wayfinder.modelDetails.slice(0, 5) : []

  function configureService() {
    if (wayfinder && typeof wayfinder.configure === "function") wayfinder.configure(settings)
  }

  function open() {
    popupOpen = true
    configureService()
    if (wayfinder) wayfinder.refresh()
  }

  function close() {
    popupOpen = false
    rollbackArmed = false
    projectToken.text = ""
  }
  function toggle() { popupOpen ? close() : open() }
  readonly property bool opened: popupOpen

  function copyEndpoint() {
    if (!wayfinder) return
    Quickshell.execDetached(["bash", "-lc",
      "printf %s " + Util.shellQuote(wayfinder.endpoint) + " | wl-copy"])
  }

  function primaryAction() {
    if (!wayfinder || !wayfinder.localEndpoint) return
    wayfinder.beginSetup()
  }

  function submitProjectToken() {
    if (!wayfinder || !Model.validProjectToken(projectToken.text) || wayfinder.projectBusy) return
    var token = projectToken.text
    projectToken.text = ""
    wayfinder.setupProject(token)
  }

  function requestProjectRollback() {
    if (!wayfinder || wayfinder.projectBusy || !wayfinder.projectReport.owned) return
    if (!rollbackArmed) {
      rollbackArmed = true
      rollbackReset.restart()
      return
    }
    rollbackArmed = false
    rollbackReset.stop()
    wayfinder.rollbackProject()
  }

  function modelFor(name) {
    if (!wayfinder) return null
    for (var i = 0; i < wayfinder.modelDetails.length; i++) {
      if (wayfinder.modelDetails[i].name === name) return wayfinder.modelDetails[i]
    }
    return null
  }

  function routeColor(name) {
    var model = modelFor(name)
    return model && Model.isLocalModel(model) ? Color.accent : foreground
  }

  function receiptColor(entry) {
    return Model.receiptNeedsAttention(entry) ? Color.urgent : routeColor(entry.servedBy || entry.model)
  }

  function actionDetail() {
    if (!wayfinder) return ""
    if (wayfinder.actionError !== "") return wayfinder.actionError
    if (wayfinder.actionMessage !== "") return wayfinder.actionMessage
    if (!wayfinder.localEndpoint) return "This remote endpoint is observed but not managed."
    if (wayfinder.setupDetail !== "") return wayfinder.setupDetail
    if (wayfinder.operatorError !== "" && wayfinder.reachable) return wayfinder.operatorError
    return ""
  }

  function projectDetail() {
    if (!wayfinder) return ""
    if (wayfinder.projectError !== "") return wayfinder.projectError
    if (wayfinder.projectMessage !== "") return wayfinder.projectMessage
    return wayfinder.projectState.detail
  }

  onSettingsChanged: configureService()
  onWayfinderChanged: configureService()
  Component.onCompleted: Qt.callLater(configureService)

  Timer {
    interval: 30000
    running: root.popupOpen
    repeat: true
    triggeredOnStart: true
    onTriggered: root.nowMs = Date.now()
  }


  Timer {
    id: rollbackReset
    interval: 8000
    repeat: false
    onTriggered: root.rollbackArmed = false
  }

  implicitWidth: button.implicitWidth
  implicitHeight: button.implicitHeight

  BarIconButton {
    id: button
    anchors.fill: parent
    bar: root.bar
    active: root.popupOpen
    useActiveColor: false
    tooltipText: root.wayfinder ? "Wayfinder · " + root.wayfinder.statusText : "Wayfinder"
    iconComponent: Component {
      RouteMark {
        markColor: root.bar ? root.bar.barForeground : Color.foreground
        statusColor: root.statusColor
        reachable: !!root.wayfinder && root.wayfinder.reachable
      }
    }
    onPressed: function(mouseButton) {
      if (mouseButton === Qt.MiddleButton) root.copyEndpoint()
      else if (mouseButton === Qt.RightButton) {
        if (root.wayfinder) root.wayfinder.refresh()
      } else root.toggle()
    }
  }

  PopupCard {
    id: popup
    anchorItem: button
    bar: root.bar
    owner: root
    open: root.popupOpen
    contentWidth: fittedContentWidth(Style.space(370), Style.space(430))
    contentHeight: fittedContentHeight(content.implicitHeight, Style.space(760))

    Column {
      id: content
      anchors.fill: parent
      spacing: Style.space(12)

      Row {
        width: parent.width
        spacing: Style.space(12)

        RouteMark {
          width: Style.space(34)
          height: width
          anchors.verticalCenter: parent.verticalCenter
          markColor: root.foreground
          statusColor: root.statusColor
          reachable: !!root.wayfinder && root.wayfinder.reachable
        }

        Column {
          width: parent.width - Style.space(46)
          anchors.verticalCenter: parent.verticalCenter
          spacing: Style.space(2)

          Row {
            width: parent.width
            Text {
              text: "Wayfinder"
              color: root.foreground
              font.family: root.bar.fontFamily
              font.pixelSize: Style.font.title
              font.bold: true
            }
            Item { width: Math.max(0, parent.width - parent.children[0].implicitWidth - statusLabel.implicitWidth) ; height: 1 }
            Text {
              id: statusLabel
              text: root.wayfinder ? root.wayfinder.statusText : "Starting…"
              color: root.statusColor
              font.family: root.bar.fontFamily
              font.pixelSize: Style.font.bodySmall
            }
          }

          Text {
            width: parent.width
            text: root.wayfinder ? root.wayfinder.endpoint : Model.DEFAULT_ENDPOINT
            color: Qt.darker(root.foreground, 1.5)
            font.family: root.bar.fontFamily
            font.pixelSize: Style.font.caption
            elide: Text.ElideMiddle
          }
        }
      }

      PanelSeparator { foreground: root.foreground }

      Column {
        width: parent.width
        visible: !!root.wayfinder && !root.wayfinder.setupState.ready
        spacing: Style.space(4)

        Text {
          text: "FIRST RUN"
          color: Qt.darker(root.foreground, 1.5)
          font.family: root.bar.fontFamily
          font.pixelSize: Style.font.caption
          font.bold: true
          font.letterSpacing: 1.1
        }
        Text {
          width: parent.width
          text: root.wayfinder ? root.wayfinder.setupState.title : "Check setup"
          color: root.foreground
          font.family: root.bar.fontFamily
          font.pixelSize: Style.font.subtitle
          font.bold: true
          wrapMode: Text.Wrap
        }
        Text {
          width: parent.width
          text: root.wayfinder ? root.wayfinder.effectiveConfigPath : ""
          color: Qt.darker(root.foreground, 1.55)
          font.family: root.bar.fontFamily
          font.pixelSize: Style.font.caption
          elide: Text.ElideMiddle
        }
      }

      Column {
        width: parent.width
        visible: !!root.wayfinder && root.wayfinder.localEndpoint
          && root.wayfinder.setupState.ready
          && (root.wayfinder.projectSupported || root.wayfinder.projectRoot !== "")
        spacing: Style.space(5)

        Row {
          width: parent.width

          Text {
            text: "PROJECT PROFILE"
            color: Qt.darker(root.foreground, 1.5)
            font.family: root.bar.fontFamily
            font.pixelSize: Style.font.caption
            font.bold: true
            font.letterSpacing: 1.1
          }

          Item {
            width: Math.max(0, parent.width - parent.children[0].implicitWidth
              - projectStatusLabel.implicitWidth)
            height: 1
          }

          Text {
            id: projectStatusLabel
            text: root.wayfinder ? root.wayfinder.projectState.status : ""
            color: root.wayfinder && root.wayfinder.projectState.urgent
              ? Color.urgent : root.foreground
            font.family: root.bar.fontFamily
            font.pixelSize: Style.font.caption
            font.bold: true
          }
        }

        Text {
          width: parent.width
          text: root.wayfinder ? root.wayfinder.projectState.title : ""
          color: root.foreground
          font.family: root.bar.fontFamily
          font.pixelSize: Style.font.bodySmall
          font.bold: true
          elide: Text.ElideRight
        }

        Text {
          width: parent.width
          visible: !!root.wayfinder && root.wayfinder.projectRoot !== ""
          text: root.wayfinder && root.wayfinder.projectReport.canonicalRepository !== ""
            ? root.wayfinder.projectReport.canonicalRepository + " · " + root.wayfinder.projectRoot
            : (root.wayfinder ? root.wayfinder.projectRoot : "")
          color: Qt.darker(root.foreground, 1.55)
          font.family: root.bar.fontFamily
          font.pixelSize: Style.font.caption
          elide: Text.ElideMiddle
        }

        Text {
          width: parent.width
          text: root.projectDetail()
          color: root.wayfinder && (root.wayfinder.projectError !== ""
            || root.wayfinder.projectState.urgent) ? Color.urgent : Qt.darker(root.foreground, 1.45)
          font.family: root.bar.fontFamily
          font.pixelSize: Style.font.caption
          wrapMode: Text.Wrap
        }

        TextField {
          id: projectToken
          visible: !!root.wayfinder && root.wayfinder.projectState.step === "setup"
          width: parent.width
          password: true
          placeholderText: "Project token · passed once over stdin"
          enabled: !!root.wayfinder && !root.wayfinder.projectBusy
          onAccepted: root.submitProjectToken()
        }

        Row {
          width: parent.width
          spacing: Style.space(6)
          visible: !!root.wayfinder && root.wayfinder.projectRoot !== ""

          Button {
            id: projectSetupButton
            visible: !!root.wayfinder && root.wayfinder.projectState.step === "setup"
            text: root.wayfinder && root.wayfinder.projectActionKind === "project-setup"
              && root.wayfinder.projectBusy ? "Setting up…" : "Set up project"
            foreground: root.foreground
            bordered: true
            enabled: !!root.wayfinder && Model.validProjectToken(projectToken.text)
              && !root.wayfinder.projectBusy
            onClicked: root.submitProjectToken()
          }

          Button {
            id: projectRollbackButton
            visible: !!root.wayfinder && root.wayfinder.projectReport.owned
            text: root.rollbackArmed ? "Confirm rollback" : "Roll back"
            foreground: root.rollbackArmed ? Color.urgent : root.foreground
            bordered: root.rollbackArmed
            enabled: !!root.wayfinder && !root.wayfinder.projectBusy
            onClicked: root.requestProjectRollback()
          }

          Item { width: Math.max(0, parent.width
            - (projectSetupButton.visible ? projectSetupButton.width : 0)
            - (projectRollbackButton.visible ? projectRollbackButton.width : 0)
            - projectRefresh.width - Style.space(18)); height: 1 }

          Button {
            id: projectRefresh
            iconText: "↻"
            tooltipText: "Refresh repository profile"
            foreground: root.foreground
            enabled: !!root.wayfinder && root.wayfinder.projectSupported
              && !root.wayfinder.projectBusy
            onClicked: root.wayfinder.refreshProject()
          }
        }
      }

      Column {
        width: parent.width
        spacing: Style.space(7)

        Row {
          width: parent.width
          Text {
            text: "ROUTING"
            color: Qt.darker(root.foreground, 1.5)
            font.family: root.bar.fontFamily
            font.pixelSize: Style.font.caption
            font.bold: true
            font.letterSpacing: 1.1
          }
          Item { width: Math.max(0, parent.width - parent.children[0].implicitWidth - routeTotal.implicitWidth); height: 1 }
          Text {
            id: routeTotal
            text: root.wayfinder && root.wayfinder.recentReport.valid
              ? root.wayfinder.recentReport.total + " recent" : "No route data"
            color: Qt.darker(root.foreground, 1.5)
            font.family: root.bar.fontFamily
            font.pixelSize: Style.font.caption
          }
        }

        Rectangle {
          width: parent.width
          height: Style.space(6)
          radius: height / 2
          color: Qt.rgba(root.foreground.r, root.foreground.g, root.foreground.b, 0.12)
          clip: true

          Rectangle {
            width: parent.width * (root.wayfinder ? root.wayfinder.routingStats.localFraction : 0)
            height: parent.height
            color: Color.accent
          }
        }

        Row {
          width: parent.width
          Text {
            text: root.wayfinder ? "Local " + root.wayfinder.routingStats.local : "Local 0"
            color: Color.accent
            font.family: root.bar.fontFamily
            font.pixelSize: Style.font.bodySmall
          }
          Item { width: Math.max(0, parent.width - parent.children[0].implicitWidth - hostedLabel.implicitWidth); height: 1 }
          Text {
            id: hostedLabel
            text: root.wayfinder ? "Hosted " + root.wayfinder.routingStats.hosted : "Hosted 0"
            color: root.foreground
            font.family: root.bar.fontFamily
            font.pixelSize: Style.font.bodySmall
          }
        }
      }

      Column {
        width: parent.width
        visible: root.recentEntries.length > 0
        spacing: Style.space(7)

        Repeater {
          model: root.recentEntries

          Row {
            required property var modelData
            width: parent.width
            height: Style.space(42)
            spacing: Style.space(8)

            Rectangle {
              width: Style.space(6)
              height: width
              radius: width / 2
              color: root.receiptColor(modelData)
              anchors.verticalCenter: parent.verticalCenter
            }

            Column {
              width: Math.max(0, parent.width - receiptState.implicitWidth - Style.space(28))
              spacing: Style.space(1)
              anchors.verticalCenter: parent.verticalCenter

              Text {
                width: parent.width
                text: Model.routeTitle(modelData)
                color: root.foreground
                font.family: root.bar.fontFamily
                font.pixelSize: Style.font.bodySmall
                font.bold: true
                elide: Text.ElideRight
              }
              Text {
                width: parent.width
                text: Model.receiptContext(modelData)
                color: Qt.darker(root.foreground, 1.55)
                font.family: root.bar.fontFamily
                font.pixelSize: Style.font.caption
                elide: Text.ElideRight
              }
              Text {
                width: parent.width
                text: Model.routeReason(modelData)
                color: Qt.darker(root.foreground, 1.55)
                font.family: root.bar.fontFamily
                font.pixelSize: Style.font.caption
                elide: Text.ElideRight
              }
            }

            Column {
              id: receiptState
              anchors.verticalCenter: parent.verticalCenter
              spacing: Style.space(1)

              Text {
                text: Model.outcomeLabel(modelData.outcome)
                  + (modelData.httpStatus !== null ? " " + modelData.httpStatus : "")
                color: root.receiptColor(modelData)
                font.family: root.bar.fontFamily
                font.pixelSize: Style.font.caption
                font.bold: true
                anchors.right: parent.right
              }
              Text {
                text: Model.relativeTime(modelData.timestamp, root.nowMs)
                color: Qt.darker(root.foreground, 1.55)
                font.family: root.bar.fontFamily
                font.pixelSize: Style.font.caption
                anchors.right: parent.right
              }
            }
          }
        }

        Text {
          width: parent.width
          visible: root.actionableReceipt !== null
          text: root.actionableReceipt
            ? Model.routeTitle(root.actionableReceipt) + ": "
              + Model.receiptRemediation(root.actionableReceipt)
            : ""
          color: Color.urgent
          font.family: root.bar.fontFamily
          font.pixelSize: Style.font.caption
          wrapMode: Text.Wrap
        }
      }

      PanelSeparator { foreground: root.foreground }

      Row {
        width: parent.width
        spacing: Style.space(12)

        Column {
          width: (parent.width - Style.space(12)) / 2
          spacing: Style.space(2)
          Text {
            text: "SAVINGS"
            color: Qt.darker(root.foreground, 1.5)
            font.family: root.bar.fontFamily
            font.pixelSize: Style.font.caption
            font.bold: true
            font.letterSpacing: 1.1
          }
          Text {
            text: Model.savingsLabel(root.wayfinder ? root.wayfinder.savingsReport : null)
            color: root.foreground
            font.family: root.bar.fontFamily
            font.pixelSize: Style.font.subtitle
            font.bold: true
          }
          Text {
            text: root.wayfinder && root.wayfinder.savingsReport.valid
              ? root.wayfinder.savingsReport.requests + " accounted requests" : "Waiting for gateway"
            color: Qt.darker(root.foreground, 1.55)
            font.family: root.bar.fontFamily
            font.pixelSize: Style.font.caption
          }
        }

        Column {
          width: (parent.width - Style.space(12)) / 2
          spacing: Style.space(2)
          Text {
            text: "MODELS"
            color: Qt.darker(root.foreground, 1.5)
            font.family: root.bar.fontFamily
            font.pixelSize: Style.font.caption
            font.bold: true
            font.letterSpacing: 1.1
          }
          Text {
            text: root.wayfinder ? root.wayfinder.healthModels.length + " configured" : "0 configured"
            color: root.foreground
            font.family: root.bar.fontFamily
            font.pixelSize: Style.font.subtitle
            font.bold: true
          }
          Text {
            text: root.wayfinder && root.wayfinder.missingKeys.length > 0
              ? root.wayfinder.missingKeys.length + " need credentials" : "Credentials ready"
            color: root.wayfinder && root.wayfinder.missingKeys.length > 0 ? Color.urgent : Qt.darker(root.foreground, 1.55)
            font.family: root.bar.fontFamily
            font.pixelSize: Style.font.caption
          }
        }
      }

      Column {
        width: parent.width
        visible: root.visibleModels.length > 0
        spacing: Style.space(4)

        Repeater {
          model: root.visibleModels

          Row {
            required property var modelData
            width: parent.width
            height: Style.space(20)

            Text {
              width: parent.width - modelState.implicitWidth - Style.space(10)
              text: Model.shortModel(modelData.name)
              color: root.foreground
              font.family: root.bar.fontFamily
              font.pixelSize: Style.font.bodySmall
              elide: Text.ElideRight
            }
            Text {
              id: modelState
              text: modelData.keyReady ? (Model.isLocalModel(modelData) ? "LOCAL" : "READY") : "KEY"
              color: modelData.keyReady ? (Model.isLocalModel(modelData) ? Color.accent : root.foreground) : Color.urgent
              font.family: root.bar.fontFamily
              font.pixelSize: Style.font.caption
              font.bold: true
            }
          }
        }
      }

      Text {
        width: parent.width
        visible: root.actionDetail() !== ""
        text: root.actionDetail()
        color: root.wayfinder && root.wayfinder.actionError !== "" ? Color.urgent : Qt.darker(root.foreground, 1.45)
        font.family: root.bar.fontFamily
        font.pixelSize: Style.font.caption
        wrapMode: Text.Wrap
      }

      Row {
        width: parent.width
        spacing: Style.space(6)

        Button {
          text: root.wayfinder ? root.wayfinder.actionLabel() : "Starting…"
          foreground: root.foreground
          bordered: true
          enabled: !!root.wayfinder && root.wayfinder.localEndpoint
            && root.wayfinder.setupState.actionable && !root.wayfinder.busy
          onClicked: root.primaryAction()
        }

        Button {
          text: "Copy endpoint"
          foreground: root.foreground
          onClicked: root.copyEndpoint()
        }

        Item { width: Math.max(0, parent.width - parent.children[0].width - parent.children[1].width - refreshButton.width - Style.space(18)); height: 1 }

        Button {
          id: refreshButton
          iconText: "↻"
          tooltipText: "Refresh Wayfinder status"
          foreground: root.foreground
          enabled: !!root.wayfinder && !root.wayfinder.busy
          onClicked: root.wayfinder.refresh()
        }
      }
    }
  }
}
