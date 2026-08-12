import QtQuick
import Quickshell
import Quickshell.Io
import "Model.js" as Model

Item {
  id: root
  visible: false

  property var shell: null
  property var manifest: null

  property string endpoint: Model.DEFAULT_ENDPOINT
  property int refreshIntervalSec: 15
  property string configPath: ""

  property bool binaryInstalled: false
  property string binaryPath: ""
  property bool unitInstalled: false
  property bool systemdActive: false
  property bool reachable: false
  property bool degraded: false
  property bool offline: false
  property bool operatorDataAvailable: false
  property bool dryRun: false

  property var healthModels: []
  property var missingKeys: []
  property var modelDetails: []
  property var recentReport: ({ valid: false, total: 0, byModel: {}, recent: [] })
  property var savingsReport: ({ valid: false, requests: 0, saved: 0, savedPct: 0, unit: "relative", priced: false })
  readonly property var routingStats: Model.routingStats(recentReport, modelDetails)

  property string lastError: ""
  property string operatorError: ""
  property string actionMessage: ""
  property string actionError: ""
  property string actionKind: ""
  property double lastUpdatedMs: 0

  readonly property bool busy: binaryProcess.running || unitProcess.running
    || systemdProcess.running || healthProcess.running || modelsProcess.running
    || recentProcess.running || savingsProcess.running || actionProcess.running
  readonly property bool localEndpoint: Model.serviceInstallArguments(endpoint, configPath) !== null
  readonly property string statusText: !localEndpoint
    ? (!reachable ? "Remote unreachable"
      : offline ? "Remote online · offline mode"
      : degraded ? "Remote online · needs attention"
      : "Remote online")
    : !binaryInstalled ? "Router not installed"
    : !unitInstalled ? "Service not installed"
    : !systemdActive ? "Service stopped"
    : !reachable ? "Gateway unreachable"
    : offline ? "Gateway online · offline mode"
    : degraded ? "Online · needs attention"
    : "Online"

  function configure(values) {
    var settings = values || {}
    var nextEndpoint = Model.normalizedEndpoint(settings.endpoint)
    var nextInterval = Model.boundedInteger(settings.refreshIntervalSec, 15, 5, 300)
    var nextConfig = String(settings.configPath || "").trim()
    var changed = endpoint !== nextEndpoint || refreshIntervalSec !== nextInterval || configPath !== nextConfig
    endpoint = nextEndpoint
    refreshIntervalSec = nextInterval
    configPath = nextConfig
    refreshTimer.interval = refreshIntervalSec * 1000
    if (changed) Qt.callLater(refresh)
  }

  function refresh() {
    if (!binaryProcess.running) binaryProcess.running = true
    if (!unitProcess.running) unitProcess.running = true
    if (!systemdProcess.running) systemdProcess.running = true
    if (!healthProcess.running) {
      healthProcess.command = curlCommand("/healthz")
      healthProcess.running = true
    }
    if (!modelsProcess.running) {
      modelsProcess.command = curlCommand("/router/models")
      modelsProcess.running = true
    }
    if (!recentProcess.running) {
      recentProcess.command = curlCommand("/router/recent?limit=8")
      recentProcess.running = true
    }
    if (!savingsProcess.running) {
      savingsProcess.command = curlCommand("/v1/savings")
      savingsProcess.running = true
    }
  }

  function curlCommand(path) {
    return ["curl", "--fail", "--silent", "--show-error", "--max-time", "3", endpoint + path]
  }

  function installService() {
    if (!binaryInstalled || actionProcess.running) return
    var args = Model.serviceInstallArguments(endpoint, configPath)
    if (!args) {
      actionError = "Automatic installation requires a loopback HTTP endpoint."
      return
    }
    runAction("install", [binaryPath].concat(args))
  }

  function startOrRestartService() {
    if (!unitInstalled || actionProcess.running) return
    runAction(systemdActive ? "restart" : "start",
      ["systemctl", "--user", systemdActive ? "restart" : "start", "wayfinder-router.service"])
  }

  function stopService() {
    if (!unitInstalled || !systemdActive || actionProcess.running) return
    runAction("stop", ["systemctl", "--user", "stop", "wayfinder-router.service"])
  }

  function runAction(kind, command) {
    actionKind = kind
    actionMessage = ""
    actionError = ""
    actionProcess.command = command
    actionProcess.running = true
  }

  function actionLabel() {
    if (actionProcess.running) return actionKind.charAt(0).toUpperCase() + actionKind.slice(1) + "ing…"
    if (!localEndpoint) return "Remote endpoint"
    if (!binaryInstalled) return "Router missing"
    if (!unitInstalled) return "Install service"
    return systemdActive ? "Restart service" : "Start service"
  }

  Timer {
    id: refreshTimer
    interval: root.refreshIntervalSec * 1000
    running: true
    repeat: true
    triggeredOnStart: true
    onTriggered: root.refresh()
  }

  Timer {
    id: actionRefreshTimer
    interval: 900
    repeat: false
    onTriggered: root.refresh()
  }

  Process {
    id: binaryProcess
    command: ["bash", "-lc", "command -v wayfinder-router"]
    stdout: StdioCollector {
      waitForEnd: true
      onStreamFinished: {
        var path = String(text || "").trim().split("\n")[0]
        root.binaryPath = path
        root.binaryInstalled = path !== ""
      }
    }
    onExited: function(exitCode) {
      if (exitCode !== 0) {
        root.binaryPath = ""
        root.binaryInstalled = false
      }
    }
  }

  Process {
    id: unitProcess
    command: ["systemctl", "--user", "is-enabled", "wayfinder-router.service"]
    stdout: StdioCollector {
      waitForEnd: true
      onStreamFinished: {
        var state = String(text || "").trim()
        root.unitInstalled = ["enabled", "enabled-runtime", "linked", "linked-runtime",
          "static", "indirect", "disabled", "generated", "transient"].indexOf(state) !== -1
      }
    }
    onExited: function(exitCode) {
      if (exitCode !== 0 && !root.unitInstalled) root.unitInstalled = false
    }
  }

  Process {
    id: systemdProcess
    command: ["systemctl", "--user", "is-active", "wayfinder-router.service"]
    stdout: StdioCollector {
      waitForEnd: true
      onStreamFinished: root.systemdActive = String(text || "").trim() === "active"
    }
    onExited: function(exitCode) {
      if (exitCode !== 0) root.systemdActive = false
    }
  }

  Process {
    id: healthProcess
    stdout: StdioCollector {
      waitForEnd: true
      onStreamFinished: {
        var report = Model.health(text)
        root.reachable = report.valid
        root.degraded = report.valid && report.status === "degraded"
        root.offline = report.valid && report.offline
        root.healthModels = report.models
        root.missingKeys = report.missingKeys
        root.lastUpdatedMs = Date.now()
        root.lastError = report.valid ? "" : "No valid response from " + root.endpoint
      }
    }
    stderr: StdioCollector {
      waitForEnd: true
      onStreamFinished: {
        var message = String(text || "").replace(/\s+/g, " ").trim()
        if (message !== "") root.lastError = message.length > 160 ? message.substring(0, 157) + "…" : message
      }
    }
    onExited: function(exitCode) {
      if (exitCode !== 0) root.reachable = false
    }
  }

  Process {
    id: modelsProcess
    stdout: StdioCollector {
      waitForEnd: true
      onStreamFinished: {
        var report = Model.models(text)
        if (report.valid) {
          root.modelDetails = report.models
          root.dryRun = report.dryRun
          root.operatorDataAvailable = true
          root.operatorError = ""
        } else {
          root.modelDetails = []
          root.operatorDataAvailable = false
        }
      }
    }
    onExited: function(exitCode) {
      if (exitCode !== 0) {
        root.modelDetails = []
        root.operatorDataAvailable = false
        root.operatorError = "Operator metadata is unavailable."
      }
    }
  }

  Process {
    id: recentProcess
    stdout: StdioCollector {
      waitForEnd: true
      onStreamFinished: {
        var report = Model.recent(text)
        root.recentReport = report
      }
    }
    onExited: function(exitCode) {
      if (exitCode !== 0) root.recentReport = ({ valid: false, total: 0, byModel: {}, recent: [] })
    }
  }

  Process {
    id: savingsProcess
    stdout: StdioCollector {
      waitForEnd: true
      onStreamFinished: {
        var report = Model.savings(text)
        root.savingsReport = report
      }
    }
    onExited: function(exitCode) {
      if (exitCode !== 0) {
        root.savingsReport = ({ valid: false, requests: 0, saved: 0, savedPct: 0,
          unit: "relative", priced: false })
      }
    }
  }

  Process {
    id: actionProcess
    stdout: StdioCollector {
      id: actionStdout
      waitForEnd: true
    }
    stderr: StdioCollector {
      id: actionStderr
      waitForEnd: true
    }
    onExited: function(exitCode) {
      var detail = String(actionStderr.text || actionStdout.text || "").replace(/\s+/g, " ").trim()
      if (exitCode === 0) {
        root.actionMessage = detail || (root.actionKind + " completed")
        root.actionError = ""
      } else {
        root.actionMessage = ""
        root.actionError = detail || (root.actionKind + " failed")
      }
      actionRefreshTimer.restart()
    }
  }
}
