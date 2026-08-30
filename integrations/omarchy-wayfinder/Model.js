.pragma library

var DEFAULT_ENDPOINT = "http://127.0.0.1:8088"

function normalizedEndpoint(value) {
  var endpoint = String(value || "").trim()
  if (endpoint === "") endpoint = DEFAULT_ENDPOINT
  if (!/^https?:\/\//i.test(endpoint)) endpoint = "http://" + endpoint
  return endpoint.replace(/\/+$/, "")
}

function boundedInteger(value, fallback, minimum, maximum) {
  var parsed = parseInt(String(value), 10)
  if (!isFinite(parsed)) parsed = fallback
  return Math.max(minimum, Math.min(maximum, parsed))
}

function parsedObject(raw) {
  try {
    var value = JSON.parse(String(raw || ""))
    return value && typeof value === "object" && !Array.isArray(value) ? value : null
  } catch (_) {
    return null
  }
}

function doctor(raw) {
  var value = parsedObject(raw)
  if (!value || String(value.schema_version || "") !== "1"
      || typeof value.ok !== "boolean" || !isFinite(Number(value.destinations))) {
    return {
      valid: false,
      ok: false,
      config: "",
      destinations: 0,
      missingEnvironment: [],
      gatewayReachable: false
    }
  }
  return {
    valid: true,
    ok: value.ok === true,
    config: String(value.config || ""),
    destinations: Math.max(0, Number(value.destinations || 0)),
    missingEnvironment: Array.isArray(value.missing_environment)
      ? value.missing_environment.map(String) : [],
    gatewayReachable: value.gateway_reachable === true
  }
}

function capabilities(raw) {
  var value = parsedObject(raw)
  var nativeCommands = value && Array.isArray(value.native_commands)
    ? value.native_commands.map(String) : []
  if (!value || String(value.schema_version || "") !== "1"
      || String(value.implementation || "") === "" || !Array.isArray(value.native_commands)) {
    return { valid: false, version: "", nativeCommands: [], projectSupported: false }
  }
  return {
    valid: true,
    version: String(value.version || ""),
    nativeCommands: nativeCommands,
    projectSupported: nativeCommands.indexOf("project setup") !== -1
      && nativeCommands.indexOf("project status") !== -1
      && nativeCommands.indexOf("project rollback") !== -1
  }
}

function emptyProjectStatus() {
  return {
    valid: false,
    status: "unknown",
    canonicalRepository: "",
    repositoryUrl: "",
    repositoryRoot: "",
    profileDirectory: "",
    profileId: "",
    workspaceId: "",
    keyId: "",
    owned: false,
    tokenSource: "",
    tokenMatches: null,
    setupRequired: false,
    profileModified: false
  }
}

function projectStatus(raw) {
  var value = parsedObject(raw)
  if (!value || Number(value.schema_version) !== 1 || typeof value.status !== "string"
      || typeof value.owned !== "boolean" || typeof value.setup_required !== "boolean"
      || typeof value.profile_modified !== "boolean") {
    return emptyProjectStatus()
  }
  return {
    valid: true,
    status: String(value.status),
    canonicalRepository: String(value.canonical_repository || ""),
    repositoryUrl: String(value.repository_url || ""),
    repositoryRoot: String(value.repository_root || ""),
    profileDirectory: String(value.profile_directory || ""),
    profileId: String(value.profile_id || ""),
    workspaceId: String(value.workspace_id || ""),
    keyId: String(value.key_id || ""),
    owned: value.owned === true,
    tokenSource: String(value.token_source || ""),
    tokenMatches: typeof value.token_matches === "boolean" ? value.token_matches : null,
    setupRequired: value.setup_required === true,
    profileModified: value.profile_modified === true
  }
}

function projectState(state) {
  var input = state || {}
  var report = input.report && typeof input.report === "object"
    ? input.report : emptyProjectStatus()
  var error = String(input.error || "")
  if (!input.configured) {
    return { ready: false, actionable: false, step: "root", urgent: false,
      status: "No repository selected", title: "Choose a repository",
      action: "Choose project", detail: "Set a local repository root in the plugin settings." }
  }
  if (input.capabilityChecked && !input.supported) {
    return { ready: false, actionable: false, step: "unsupported", urgent: true,
      status: "Router update required", title: "Project profiles unavailable",
      action: "Update Router", detail: "The installed Router does not advertise the project lifecycle contract." }
  }
  if (!input.capabilityChecked || !input.checked) {
    return { ready: false, actionable: false, step: "checking", urgent: false,
      status: "Checking repository", title: "Inspecting project context",
      action: "Checking…", detail: "Wayfinder is resolving owned project state without changing the repository." }
  }
  if (error !== "" || !report.valid) {
    return { ready: false, actionable: false, step: "error", urgent: true,
      status: "Project needs attention", title: "Repository could not be inspected",
      action: "Retry", detail: error || "The Router returned an invalid project status." }
  }
  if (report.setupRequired || !report.owned) {
    return { ready: false, actionable: true, step: "setup", urgent: false,
      status: "Project setup required", title: "Add a repository profile",
      action: "Set up project",
      detail: "Enter a token you keep. Only its SHA-256 hash is stored outside the repository." }
  }
  if (report.tokenMatches === false) {
    return { ready: true, actionable: false, step: "ready", urgent: true,
      status: "Project token mismatch", title: report.canonicalRepository || "Project profile active",
      action: "Profile active", detail: "The loaded project token does not match this owned profile." }
  }
  var detail = report.profileModified
    ? "The owned routing profile has transparent user changes."
    : "The deterministic routing scaffold is unchanged."
  if (report.tokenMatches === true) detail += " The project token matches."
  else detail += " Supply the same token when launching a supported coding agent."
  return { ready: true, actionable: false, step: "ready", urgent: false,
    status: report.profileModified ? "Custom project profile" : "Project profile active",
    title: report.canonicalRepository || "Project profile active",
    action: "Profile active", detail: detail }
}

function projectError(raw) {
  return String(raw || "")
    .replace(/Wayfinder project token \(input is not persisted\):/g, "")
    .replace(/\s+/g, " ").trim()
}

function validProjectToken(value) {
  var token = String(value || "")
  return token.length > 0 && token.length <= 512 && !/[\u0000-\u001f\u007f]/.test(token)
}

function setupState(state) {
  var input = state || {}
  var missing = Array.isArray(input.missingEnvironment)
    ? input.missingEnvironment.map(String) : []
  var missingDetail = missing.length > 0
    ? " Missing environment: " + missing.join(", ") + "."
    : ""

  if (!input.localEndpoint) {
    return { ready: false, actionable: false, step: "remote", status: "",
      title: "Remote endpoint", action: "Remote endpoint",
      detail: "This endpoint is observed but not managed." }
  }
  if (!input.binaryInstalled) {
    return { ready: false, actionable: true, step: "binary", status: "Setup required",
      title: "Install the reviewed Router", action: "Set up Wayfinder",
      detail: "Downloads the pinned native Router, verifies its SHA-256 digest, then continues setup." }
  }
  if (String(input.configPath || "") === "") {
    return { ready: false, actionable: false, step: "path", status: "Preparing setup",
      title: "Resolve the policy path", action: "Preparing…",
      detail: "Wayfinder is resolving a user-owned configuration path." }
  }
  if (!input.configChecked) {
    return { ready: false, actionable: false, step: "probe", status: "Checking setup",
      title: "Check existing setup", action: "Checking…",
      detail: "Wayfinder is checking for an existing policy without changing it." }
  }
  if (!input.configExists) {
    return { ready: false, actionable: true, step: "policy", status: "Setup required",
      title: "Create a local policy", action: "Set up Wayfinder",
      detail: "Creates a no-clobber local policy, validates it, and installs the user service." }
  }
  if (!input.doctorChecked) {
    return { ready: false, actionable: true, step: "doctor", status: "Checking policy",
      title: "Validate the policy", action: "Check policy",
      detail: "Runs the native doctor against the existing policy before service installation." }
  }
  if (!input.configValid) {
    return { ready: false, actionable: true, step: "repair", status: "Policy needs attention",
      title: "Repair the existing policy", action: "Recheck policy",
      detail: "Existing policies are never overwritten. Fix this one, then run the check again." }
  }
  if (!input.unitInstalled) {
    return { ready: false, actionable: true, step: "service", status: "Service not installed",
      title: "Install the user service", action: "Install service",
      detail: "The policy is valid." + missingDetail
        + " Installation uses the reviewed systemd user-service boundary." }
  }
  if (!input.systemdActive) {
    return { ready: true, actionable: true, step: "start", status: "",
      title: "Start the Router", action: "Start service",
      detail: "The service is installed but stopped." + missingDetail }
  }
  if (!input.reachable) {
    return { ready: true, actionable: true, step: "restart", status: "",
      title: "Recover the Router", action: "Restart service",
      detail: "The service is active but its loopback gateway is not reachable." + missingDetail }
  }
  return { ready: true, actionable: true, step: "ready", status: "",
    title: "Wayfinder is ready", action: "Restart service",
    detail: missingDetail.trim() }
}

function health(raw) {
  var value = parsedObject(raw)
  if (!value || (value.status !== "ok" && value.status !== "degraded")) {
    return { valid: false, status: "unreachable", models: [], missingKeys: [], offline: false }
  }
  return {
    valid: true,
    status: String(value.status),
    models: Array.isArray(value.models) ? value.models.map(String) : [],
    missingKeys: Array.isArray(value.missing_keys) ? value.missing_keys.map(String) : [],
    offline: value.offline === true
  }
}

function models(raw) {
  var value = parsedObject(raw)
  if (!value || !Array.isArray(value.models)) return { valid: false, models: [], dryRun: false }
  var output = []
  for (var i = 0; i < value.models.length; i++) {
    var model = value.models[i]
    if (!model || typeof model !== "object" || !model.name) continue
    output.push({
      name: String(model.name),
      endpoint: String(model.endpoint || ""),
      provider: String(model.provider || ""),
      providerModel: String(model.model || ""),
      tier: model.tier === null || model.tier === undefined ? "" : String(model.tier),
      keyReady: model.key_ok === true
    })
  }
  return { valid: true, models: output, dryRun: value.dry_run === true }
}

function recent(raw) {
  var value = parsedObject(raw)
  if (!value || !Array.isArray(value.recent)) return { valid: false, total: 0, byModel: {}, recent: [] }
  var entries = []
  for (var i = 0; i < value.recent.length; i++) {
    var item = value.recent[i]
    if (!item || typeof item !== "object") continue
    entries.push({
      requestId: String(item.request_id || ""),
      model: String(item.model || "unknown"),
      score: Number(item.score || 0),
      mode: String(item.mode || "scored"),
      policyVersion: String(item.policy_version || ""),
      snapshotId: String(item.snapshot_id || ""),
      policyProfile: String(item.policy_profile || ""),
      servedBy: String(item.served_by || ""),
      executionBoundary: String(item.execution_boundary || ""),
      outcome: String(item.outcome || ""),
      httpStatus: item.http_status !== null && item.http_status !== undefined
        && isFinite(Number(item.http_status)) ? Number(item.http_status) : null,
      errorType: String(item.error_type || ""),
      timestamp: Number(item.ts || 0),
      saved: item.cost && isFinite(Number(item.cost.saved)) ? Number(item.cost.saved) : null,
      unit: item.cost ? String(item.cost.unit || "") : "",
      estimated: !!(item.cost && item.cost.estimated),
      cache: String(item.cache || "")
    })
  }
  return {
    valid: true,
    total: Math.max(0, Number(value.total || 0)),
    byModel: value.by_model && typeof value.by_model === "object" ? value.by_model : {},
    recent: entries
  }
}

function routeTitle(entry) {
  if (!entry) return "unknown"
  return shortModel(String(entry.servedBy || entry.model || "unknown"))
}

function routeReason(entry) {
  if (!entry) return "Route details unavailable"
  var mode = String(entry.mode || "")
  var reason = ""
  if (mode === "scored") reason = "Automatic · score " + fixed(entry.score, 2)
  else if (mode === "pinned") reason = "Pinned by request"
  else if (mode === "slash-pinned") reason = "Pinned by route"
  else if (mode !== "") reason = mode.replace(/[-_]+/g, " ")
  else reason = "Routing mode unavailable"

  var selected = String(entry.model || "")
  var served = String(entry.servedBy || "")
  if (selected !== "" && served !== "" && selected !== served) {
    reason = "Selected " + shortModel(selected) + " · " + reason
  }
  return reason
}

function boundaryLabel(value) {
  var boundary = String(value || "")
  if (boundary === "on-device") return "ON DEVICE"
  if (boundary === "local-network") return "LOCAL NET"
  if (boundary === "hosted") return "HOSTED"
  return "BOUNDARY ?"
}

function outcomeLabel(value) {
  var outcome = String(value || "")
  if (outcome === "succeeded") return "DONE"
  if (outcome === "streaming") return "LIVE"
  if (outcome === "failed") return "FAILED"
  if (outcome === "cancelled") return "CANCELLED"
  if (outcome === "cache-hit") return "CACHE"
  return "PENDING"
}

function receiptDetail(entry) {
  if (!entry) return ""
  return receiptContext(entry) + " · " + routeReason(entry)
}

function receiptContext(entry) {
  if (!entry) return ""
  var parts = [boundaryLabel(entry.executionBoundary)]
  if (String(entry.policyProfile || "") !== "") parts.push("PROFILE " + entry.policyProfile)
  return parts.join(" · ")
}

function receiptNeedsAttention(entry) {
  return !!entry && String(entry.outcome || "") === "failed"
}

function receiptRemediation(entry) {
  if (!entry) return ""
  var outcome = String(entry.outcome || "")
  if (outcome === "cancelled") return "Client disconnected before completion."
  if (outcome !== "failed") return ""

  var errorType = String(entry.errorType || "")
  var httpStatus = Number(entry.httpStatus)
  if (errorType === "wayfinder_router_unauthorized" || httpStatus === 401 || httpStatus === 403) {
    return "Check the provider credential."
  }
  if (errorType === "wayfinder_router_rate_limited"
      || errorType === "wayfinder_router_usage_limited"
      || errorType === "wayfinder_router_budget_exhausted" || httpStatus === 429) {
    return "Check provider limits or budget, then retry."
  }
  if (errorType === "wayfinder_router_circuit_open") {
    return "The provider circuit is open; retry shortly."
  }
  if (errorType === "wayfinder_router_not_ready"
      || errorType === "wayfinder_router_misconfigured"
      || errorType === "wayfinder_router_destination_ineligible"
      || errorType === "wayfinder_router_offline_unavailable") {
    return "Run wayfinder-router doctor and repair the policy."
  }
  if (errorType === "wayfinder_router_busy"
      || errorType === "wayfinder_router_overloaded") {
    return "The Router is busy; retry shortly."
  }
  if (errorType === "wayfinder_router_state_error") {
    return "Restart the Router, then run wayfinder-router doctor."
  }
  if (errorType === "wayfinder_router_request_too_large"
      || errorType === "wayfinder_router_token_bound_required") {
    return "Reduce the request context or configure a token limit."
  }
  return "Retry, then inspect Router logs if it repeats."
}

function latestActionableReceipt(entries) {
  var values = Array.isArray(entries) ? entries : []
  for (var i = 0; i < values.length; i++) {
    if (receiptNeedsAttention(values[i])) return values[i]
  }
  return null
}

function savings(raw) {
  var value = parsedObject(raw)
  if (!value) {
    return { valid: false, requests: 0, saved: 0, savedPct: 0, unit: "relative", priced: false }
  }
  return {
    valid: true,
    requests: Math.max(0, Number(value.requests || 0)),
    saved: Number(value.saved || 0),
    savedPct: Number(value.saved_pct || 0),
    unit: String(value.unit || "relative"),
    priced: value.priced === true,
    realized: Number(value.realized || 0),
    baseline: Number(value.baseline || 0)
  }
}

function isLoopbackEndpoint(endpoint) {
  return /^https?:\/\/(127(?:\.\d+){3}|localhost|\[::1\])(?::\d+)?(?:\/|$)/i.test(String(endpoint || ""))
}

function isLocalModel(model) {
  if (!model) return false
  var tier = String(model.tier || "").toLowerCase()
  var provider = String(model.provider || "").toLowerCase()
  return tier === "local" || tier === "cheap" || tier === "low"
    || provider === "ollama" || provider === "lm-studio" || provider === "llama.cpp"
    || provider === "vllm" || isLoopbackEndpoint(model.endpoint)
}

function routingStats(report, modelList) {
  var modelMap = {}
  for (var i = 0; i < modelList.length; i++) modelMap[modelList[i].name] = modelList[i]
  var local = 0
  var hosted = 0
  var unknown = 0
  var counts = report && report.byModel ? report.byModel : {}
  for (var name in counts) {
    var count = Math.max(0, Number(counts[name] || 0))
    if (!modelMap[name]) unknown += count
    else if (isLocalModel(modelMap[name])) local += count
    else hosted += count
  }
  var total = local + hosted + unknown
  return {
    local: local,
    hosted: hosted,
    unknown: unknown,
    total: total,
    localFraction: total > 0 ? local / total : 0
  }
}

function serviceInstallArguments(endpoint, configPath) {
  var normalized = normalizedEndpoint(endpoint)
  var match = normalized.match(/^http:\/\/(127(?:\.\d+){3}|localhost|\[::1\])(?::(\d+))?$/i)
  if (!match) return null
  var port = match[2] || "8088"
  var numericPort = Number(port)
  if (!isFinite(numericPort) || numericPort < 1 || numericPort > 65535) return null
  var host = match[1] === "[::1]" ? "::1" : match[1]
  var args = ["service", "install", "--host", host, "--port", String(numericPort)]
  var config = String(configPath || "").trim()
  if (config !== "") args.push("--config", config)
  return args
}

function shortModel(name) {
  var value = String(name || "unknown")
  return value.length > 28 ? value.substring(0, 25) + "…" : value
}

function fixed(value, digits) {
  var number = Number(value)
  return isFinite(number) ? number.toFixed(digits) : "0"
}

function savingsLabel(report) {
  if (!report || !report.valid || report.requests === 0) return "No accounted requests"
  if (report.priced && report.unit === "usd") return "$" + fixed(report.saved, 2) + " saved"
  return fixed(report.savedPct, 1) + "% saved"
}

function relativeTime(timestamp, nowMs) {
  var delta = Math.max(0, Number(nowMs || Date.now()) - Number(timestamp || 0) * 1000)
  if (delta < 60000) return "now"
  var minutes = Math.floor(delta / 60000)
  if (minutes < 60) return minutes + "m"
  var hours = Math.floor(minutes / 60)
  if (hours < 24) return hours + "h"
  return Math.floor(hours / 24) + "d"
}
