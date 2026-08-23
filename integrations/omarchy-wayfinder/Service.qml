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
  property string projectRoot: ""
  property string defaultConfigPath: ""
  readonly property string effectiveConfigPath: String(configPath || "").trim() !== ""
    ? String(configPath || "").trim() : defaultConfigPath

  property bool binaryInstalled: false
  property string binaryPath: ""
  property bool unitInstalled: false
  property bool systemdActive: false
  property bool reachable: false
  property bool degraded: false
  property bool offline: false
  property bool operatorDataAvailable: false
  property bool dryRun: false
  property bool capabilityChecked: false
  property bool projectSupported: false
  property string routerVersion: ""

  property bool configChecked: false
  property bool configExists: false
  property bool doctorChecked: false
  property bool configValid: false
  property bool doctorOk: false
  property var doctorMissingEnvironment: []
  property string doctorError: ""
  property bool setupRequested: false
  property string setupStage: "idle"

  property bool projectChecked: false
  property var projectReport: Model.emptyProjectStatus()
  property string projectError: ""
  property string projectMessage: ""
  property string projectActionKind: ""
  property string pendingProjectToken: ""

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
    || defaultConfigProcess.running || configProbeProcess.running || doctorProcess.running
    || capabilityProcess.running || projectStatusProcess.running || projectActionProcess.running
  readonly property bool projectBusy: capabilityProcess.running || projectStatusProcess.running
    || projectActionProcess.running
  readonly property bool localEndpoint: Model.serviceInstallArguments(endpoint, effectiveConfigPath) !== null
  readonly property var setupState: Model.setupState({
    localEndpoint: localEndpoint,
    binaryInstalled: binaryInstalled,
    configPath: effectiveConfigPath,
    configChecked: configChecked,
    configExists: configExists,
    doctorChecked: doctorChecked,
    configValid: configValid,
    unitInstalled: unitInstalled,
    systemdActive: systemdActive,
    reachable: reachable,
    missingEnvironment: doctorMissingEnvironment
  })
  readonly property string setupDetail: doctorError !== "" ? doctorError : setupState.detail
  readonly property var projectState: Model.projectState({
    configured: String(projectRoot || "").trim() !== "",
    capabilityChecked: capabilityChecked,
    supported: projectSupported,
    checked: projectChecked,
    report: projectReport,
    error: projectError
  })
  readonly property string statusText: !localEndpoint
    ? (!reachable ? "Remote unreachable"
      : offline ? "Remote online · offline mode"
      : degraded ? "Remote online · needs attention"
      : "Remote online")
    : setupState.status !== "" ? setupState.status
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
    var nextProject = String(settings.projectRoot || "").trim()
    var changed = endpoint !== nextEndpoint || refreshIntervalSec !== nextInterval || configPath !== nextConfig
    var projectChanged = projectRoot !== nextProject
    endpoint = nextEndpoint
    refreshIntervalSec = nextInterval
    configPath = nextConfig
    projectRoot = nextProject
    refreshTimer.interval = refreshIntervalSec * 1000
    if (changed) {
      configChecked = false
      configExists = false
      doctorChecked = false
      configValid = false
      doctorOk = false
      doctorMissingEnvironment = []
      doctorError = ""
      Qt.callLater(refresh)
    }
    if (projectChanged) {
      projectChecked = false
      projectReport = Model.emptyProjectStatus()
      projectError = ""
      projectMessage = ""
      if (projectSupported && projectRoot !== "") Qt.callLater(refreshProject)
    }
  }

  function refresh() {
    if (!binaryProcess.running) binaryProcess.running = true
    if (!unitProcess.running) unitProcess.running = true
    if (!systemdProcess.running) systemdProcess.running = true
    if (String(configPath || "").trim() === "" && defaultConfigPath === ""
        && !defaultConfigProcess.running) {
      defaultConfigProcess.running = true
    } else {
      probeConfig()
    }
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
    if (binaryInstalled && !capabilityChecked && !capabilityProcess.running) refreshCapabilities()
    if (projectSupported && projectRoot !== "" && !projectActionProcess.running) refreshProject()
  }

  function probeConfig() {
    if (effectiveConfigPath === "" || configProbeProcess.running || doctorProcess.running) return
    configProbeProcess.command = ["test", "-f", effectiveConfigPath]
    configProbeProcess.running = true
  }

  function validatePolicy(forSetup) {
    if (!binaryInstalled || effectiveConfigPath === "" || doctorProcess.running) return
    if (forSetup) {
      setupRequested = true
      setupStage = "doctor"
    }
    doctorProcess.command = [binaryPath, "doctor", "--config", effectiveConfigPath, "--json"]
    doctorProcess.running = true
  }

  function initializePolicy() {
    if (!binaryInstalled || effectiveConfigPath === "" || actionProcess.running) return
    setupRequested = true
    setupStage = "policy"
    runAction("initialize", [
      binaryPath, "init", "--preset", "local", "--path", effectiveConfigPath
    ])
  }

  function continueSetup() {
    if (!setupRequested || actionProcess.running || doctorProcess.running) return
    if (!configExists) {
      initializePolicy()
    } else if (!doctorChecked || !configValid) {
      validatePolicy(true)
    } else if (!unitInstalled) {
      installService(true)
    } else if (!systemdActive) {
      startOrRestartService(true)
    } else {
      setupRequested = false
      setupStage = "complete"
      actionMessage = doctorMissingEnvironment.length > 0
        ? "Wayfinder is running; some provider environment is still missing."
        : "Wayfinder is ready."
      actionError = ""
      refresh()
    }
  }

  function beginSetup() {
    if (!binaryInstalled || !localEndpoint || busy || effectiveConfigPath === "") return
    actionMessage = ""
    actionError = ""
    if (!configChecked) {
      probeConfig()
      return
    }
    if (!configExists) {
      initializePolicy()
    } else if (!doctorChecked || !configValid) {
      validatePolicy(true)
    } else if (!unitInstalled) {
      installService(true)
    } else if (!systemdActive || !reachable) {
      startOrRestartService(true)
    } else {
      startOrRestartService(false)
    }
  }

  function refreshCapabilities() {
    if (!binaryInstalled || binaryPath === "" || capabilityProcess.running) return
    capabilityProcess.command = [binaryPath, "capabilities", "--json"]
    capabilityProcess.running = true
  }

  function refreshProject() {
    if (!binaryInstalled || !projectSupported || projectRoot === ""
        || projectStatusProcess.running || projectActionProcess.running) return
    projectStatusProcess.command = [binaryPath, "project", "status", "--root", projectRoot, "--json"]
    projectStatusProcess.running = true
  }

  function setupProject(token) {
    if (!binaryInstalled || !projectSupported || projectRoot === ""
        || projectStatusProcess.running || projectActionProcess.running
        || !Model.validProjectToken(token)) return
    pendingProjectToken = String(token)
    projectActionKind = "project-setup"
    projectMessage = ""
    projectError = ""
    projectActionProcess.command = [binaryPath, "project", "setup", "--root", projectRoot,
      "--prompt-token", "--json"]
    projectActionProcess.running = true
  }

  function rollbackProject() {
    if (!binaryInstalled || !projectSupported || projectRoot === ""
        || projectStatusProcess.running || projectActionProcess.running || !projectReport.owned) return
    pendingProjectToken = ""
    projectActionKind = "project-rollback"
    projectMessage = ""
    projectError = ""
    projectActionProcess.command = [binaryPath, "project", "rollback", "--root", projectRoot, "--json"]
    projectActionProcess.running = true
  }

  function curlCommand(path) {
    return ["curl", "--fail", "--silent", "--show-error", "--max-time", "3", endpoint + path]
  }

  function installService(forSetup) {
    if (!binaryInstalled || actionProcess.running) return
    var args = Model.serviceInstallArguments(endpoint, effectiveConfigPath)
    if (!args) {
      actionError = "Automatic installation requires a loopback HTTP endpoint."
      setupRequested = false
      return
    }
    if (forSetup) {
      setupRequested = true
      setupStage = "service"
    }
    runAction("install", [binaryPath].concat(args))
  }

  function startOrRestartService(forSetup) {
    if (!unitInstalled || actionProcess.running) return
    if (forSetup) {
      setupRequested = true
      setupStage = systemdActive ? "restart" : "start"
    }
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
    if (actionProcess.running) {
      if (actionKind === "initialize") return "Creating policy…"
      if (actionKind === "install") return "Installing service…"
      if (actionKind === "restart") return "Restarting service…"
      if (actionKind === "start") return "Starting service…"
      if (actionKind === "stop") return "Stopping service…"
      return "Working…"
    }
    if (doctorProcess.running) return "Checking policy…"
    return setupState.action
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
    id: defaultConfigProcess
    command: ["bash", "-lc",
      "printf '%s\\n' \"${XDG_CONFIG_HOME:-$HOME/.config}/wayfinder/wayfinder-router.toml\""]
    stdout: StdioCollector {
      waitForEnd: true
      onStreamFinished: {
        var path = String(text || "").trim().split("\\n")[0]
        if (path !== "") {
          root.defaultConfigPath = path
          Qt.callLater(root.probeConfig)
        }
      }
    }
    stderr: StdioCollector {
      waitForEnd: true
      onStreamFinished: {
        var detail = String(text || "").replace(/\\s+/g, " ").trim()
        if (detail !== "") root.doctorError = detail
      }
    }
  }

  Process {
    id: configProbeProcess
    onExited: function(exitCode) {
      root.configChecked = true
      root.configExists = exitCode === 0
      if (!root.configExists) {
        root.doctorChecked = false
        root.configValid = false
        root.doctorOk = false
        root.doctorMissingEnvironment = []
        root.doctorError = ""
      } else if (root.binaryInstalled) {
        Qt.callLater(function() { root.validatePolicy(false) })
      }
    }
  }

  Process {
    id: doctorProcess
    stdout: StdioCollector {
      id: doctorStdout
      waitForEnd: true
    }
    stderr: StdioCollector {
      id: doctorStderr
      waitForEnd: true
    }
    onExited: function(exitCode) {
      var report = Model.doctor(doctorStdout.text)
      root.doctorChecked = true
      root.configValid = report.valid
      root.doctorOk = report.valid && report.ok
      root.doctorMissingEnvironment = report.valid ? report.missingEnvironment : []
      if (report.valid) {
        root.doctorError = ""
        if (root.setupRequested) Qt.callLater(root.continueSetup)
      } else {
        var detail = String(doctorStderr.text || "").replace(/\\s+/g, " ").trim()
        root.doctorError = detail !== ""
          ? (detail.length > 240 ? detail.substring(0, 237) + "…" : detail)
          : "The existing policy could not be validated."
        root.setupRequested = false
        root.setupStage = "failed"
      }
    }
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
        if (root.binaryInstalled) {
          Qt.callLater(root.probeConfig)
          Qt.callLater(root.refreshCapabilities)
        }
      }
    }
    onExited: function(exitCode) {
      if (exitCode !== 0) {
        root.binaryPath = ""
        root.binaryInstalled = false
        root.capabilityChecked = false
        root.projectSupported = false
        root.routerVersion = ""
      }
    }
  }

  Process {
    id: capabilityProcess
    stdout: StdioCollector {
      id: capabilityStdout
      waitForEnd: true
    }
    stderr: StdioCollector {
      id: capabilityStderr
      waitForEnd: true
    }
    onExited: function(exitCode) {
      var report = Model.capabilities(capabilityStdout.text)
      root.capabilityChecked = true
      root.routerVersion = report.valid ? report.version : ""
      root.projectSupported = exitCode === 0 && report.valid && report.projectSupported
      if (!root.projectSupported) {
        root.projectChecked = false
        root.projectReport = Model.emptyProjectStatus()
      } else if (root.projectRoot !== "") {
        Qt.callLater(root.refreshProject)
      }
    }
  }

  Process {
    id: projectStatusProcess
    stdout: StdioCollector {
      id: projectStatusStdout
      waitForEnd: true
    }
    stderr: StdioCollector {
      id: projectStatusStderr
      waitForEnd: true
    }
    onExited: function(exitCode) {
      root.projectChecked = true
      if (exitCode === 0) {
        var report = Model.projectStatus(projectStatusStdout.text)
        root.projectReport = report
        root.projectError = report.valid ? "" : "The Router returned an invalid project status."
      } else {
        root.projectReport = Model.emptyProjectStatus()
        var detail = Model.projectError(projectStatusStderr.text || projectStatusStdout.text)
        root.projectError = detail || "The repository could not be inspected."
      }
    }
  }

  Process {
    id: projectActionProcess
    stdinEnabled: true
    stdout: StdioCollector {
      id: projectActionStdout
      waitForEnd: true
    }
    stderr: StdioCollector {
      id: projectActionStderr
      waitForEnd: true
    }
    onStarted: {
      if (root.projectActionKind === "project-setup" && root.pendingProjectToken !== "") {
        projectActionProcess.write(root.pendingProjectToken + "\n")
      }
      root.pendingProjectToken = ""
    }
    onExited: function(exitCode) {
      root.pendingProjectToken = ""
      root.projectChecked = true
      if (exitCode === 0) {
        var report = Model.projectStatus(projectActionStdout.text)
        if (report.valid) {
          root.projectReport = report
          root.projectError = ""
          root.projectMessage = root.projectActionKind === "project-rollback"
            ? "Owned project state rolled back."
            : (report.status === "unchanged" ? "Project profile already active."
              : "Project profile created; the Router will hot reload it.")
        } else {
          root.projectReport = Model.emptyProjectStatus()
          root.projectMessage = ""
          root.projectError = "The Router returned an invalid project result."
        }
      } else {
        root.projectMessage = ""
        var detail = Model.projectError(projectActionStderr.text || projectActionStdout.text)
        root.projectError = detail || (root.projectActionKind === "project-rollback"
          ? "Project rollback failed." : "Project setup failed.")
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
      if (detail.length > 240) detail = detail.substring(0, 237) + "…"
      if (exitCode === 0) {
        root.actionMessage = detail || (root.actionKind + " completed")
        root.actionError = ""
        if (root.actionKind === "initialize") {
          root.configChecked = true
          root.configExists = true
          root.doctorChecked = false
          root.configValid = false
          Qt.callLater(function() { root.validatePolicy(true) })
          return
        }
        if (root.setupRequested
            && (root.actionKind === "install" || root.actionKind === "start"
              || root.actionKind === "restart")) {
          root.setupRequested = false
          root.setupStage = "complete"
          root.actionMessage = root.doctorMissingEnvironment.length > 0
            ? "Service started. Some provider environment is still missing."
            : "Wayfinder setup completed."
        }
      } else {
        root.actionMessage = ""
        root.actionError = detail || (root.actionKind + " failed")
        root.setupRequested = false
        root.setupStage = "failed"
      }
      actionRefreshTimer.restart()
    }
  }
}
