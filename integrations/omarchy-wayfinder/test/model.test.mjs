import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import path from "node:path";
import vm from "node:vm";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const source = (await readFile(path.join(root, "Model.js"), "utf8"))
  .replace(/^\.pragma library\s*$/m, "");
const model = { Date, JSON, Math, Number, String, Array, Object, RegExp, isFinite };
vm.createContext(model);
vm.runInContext(source, model, { filename: "Model.js" });

assert.equal(model.normalizedEndpoint("127.0.0.1:8088/"), "http://127.0.0.1:8088");
assert.equal(model.normalizedEndpoint(""), "http://127.0.0.1:8088");
assert.equal(model.boundedInteger("2", 15, 5, 300), 5);

assert.deepEqual(
  JSON.parse(JSON.stringify(model.health('{"status":"degraded","models":["local"],"offline":true,"missing_keys":["cloud"]}'))),
  { valid: true, status: "degraded", models: ["local"], missingKeys: ["cloud"], offline: true }
);
assert.equal(model.health("not-json").valid, false);

assert.deepEqual(
  JSON.parse(JSON.stringify(model.doctor(JSON.stringify({
    schema_version: "1",
    ok: false,
    config: "/home/tom/.config/wayfinder/wayfinder-router.toml",
    destinations: 2,
    missing_environment: ["OPENAI_API_KEY"],
    gateway_reachable: false
  })))),
  {
    valid: true,
    ok: false,
    config: "/home/tom/.config/wayfinder/wayfinder-router.toml",
    destinations: 2,
    missingEnvironment: ["OPENAI_API_KEY"],
    gatewayReachable: false
  }
);
assert.equal(model.doctor("not-json").valid, false);

const capabilityReport = model.capabilities(JSON.stringify({
  schema_version: "1",
  implementation: "rust",
  version: "1.0.0",
  native_commands: ["project setup", "project status", "project rollback"]
}));
assert.equal(capabilityReport.valid, true);
assert.equal(capabilityReport.projectSupported, true);
assert.equal(model.capabilities('{"schema_version":"1","implementation":"rust","native_commands":[]}').projectSupported, false);

const setupRequiredProject = model.projectStatus(JSON.stringify({
  schema_version: 1,
  status: "setup-required",
  canonical_repository: null,
  repository_root: "/home/tom/code/project",
  profile_directory: null,
  owned: false,
  token_source: null,
  token_matches: null,
  setup_required: true,
  profile_modified: false
}));
assert.equal(setupRequiredProject.valid, true);
assert.equal(setupRequiredProject.setupRequired, true);
assert.equal(model.projectState({
  configured: true,
  capabilityChecked: true,
  supported: true,
  checked: true,
  report: setupRequiredProject
}).action, "Set up project");

const readyProject = model.projectStatus(JSON.stringify({
  schema_version: 1,
  status: "ready",
  canonical_repository: "asdecided/WayfinderRouter",
  repository_url: "https://github.com/asdecided/WayfinderRouter",
  repository_root: "/home/tom/code/WayfinderRouter",
  profile_directory: "/home/tom/.config/wayfinder/projects/project-abc",
  profile_id: "project-abc",
  workspace_id: "project-abc",
  key_id: "project-abc",
  owned: true,
  token_source: "interactive-prompt",
  token_matches: true,
  setup_required: false,
  profile_modified: true
}));
const readyProjectState = model.projectState({
  configured: true,
  capabilityChecked: true,
  supported: true,
  checked: true,
  report: readyProject
});
assert.equal(readyProjectState.ready, true);
assert.equal(readyProjectState.status, "Custom project profile");
assert.match(readyProjectState.detail, /token matches/i);
assert.equal(model.projectState({ configured: true, capabilityChecked: true, supported: false }).step, "unsupported");
assert.equal(model.validProjectToken("correct horse battery staple"), true);
assert.equal(model.validProjectToken("bad\nsecret"), false);
assert.equal(model.projectError("Wayfinder project token (input is not persisted):\nwayfinder-router: failed"), "wayfinder-router: failed");

const projectValuePayload = {
  schema_version: "wf-project-value-v1",
  workspace_id: "project-abc",
  generated_at_ts: 1788041000,
  accounting: {
    period_days: 30,
    through_utc: "2026-08-29",
    first_observed_utc: "2026-08-20",
    last_observed_utc: "2026-08-29",
    attribution_scope: "workspace-attributed-requests-recorded-after-schema-activation",
    unit: "usd",
    priced: true,
    requests: 4,
    estimated_requests: 1,
    tokens: 8000,
    realized: 0.04,
    baseline: 0.08,
    saved: 0.04,
    saved_pct: 50,
    by_route: { local: { requests: 3 }, cloud: { requests: 1 } }
  },
  delivery: {
    retention: "process-local-bounded-shared-ring",
    shared_capacity: 200,
    retained: 5,
    first_observed_ts: 1788030000,
    last_observed_ts: 1788040000,
    terminal: 4,
    succeeded: 2,
    failed: 1,
    cancelled: 0,
    cache_hits: 1,
    in_progress: 1,
    delivery_unobserved: 0,
    failure_rate_pct: 25,
    boundaries: { on_device: 2, local_network: 1, hosted: 1, unknown: 1 },
    by_route: { local: 3, cloud: 2 }
  },
  quality: {
    status: "not-collected",
    eligible_receipts: 4,
    labelled_receipts: 0,
    coverage_pct: 0,
    correction_rate_pct: null,
    reason: "explicit user outcome labels are not collected by this schema"
  },
  baseline: {
    kind: "dearest-configured-rate",
    routes: ["cloud"],
    rate_per_1k: 0.01,
    unit: "usd",
    price_table_version: "abcdef012345"
  },
  limitations: ["delivery evidence is bounded to the current process shared ring"]
};
const projectValue = model.projectValue(JSON.stringify(projectValuePayload));
assert.equal(projectValue.valid, true);
assert.equal(projectValue.workspaceId, "project-abc");
assert.equal(projectValue.accounting.estimatedRequests, 1);
assert.equal(projectValue.delivery.failureRatePct, 25);
assert.equal(projectValue.quality.correctionRatePct, null);
assert.equal(model.projectValueWindow(projectValue), "30 DAYS");
assert.equal(model.projectValueSavingsLabel(projectValue), "$0.04 saved");
assert.equal(model.projectFailureLabel(projectValue), "25.0% delivery failures");
assert.equal(model.projectQualityLabel(projectValue), "Corrections not collected · 0 / 4 labelled");
assert.equal(model.projectBaselineLabel(projectValue), "$0.0100/1k current baseline · cloud · prices abcdef01");
assert.match(model.projectValueRemediation(projectValue), /1 recent delivery failure needs attention/);
assert.deepEqual(
  JSON.parse(JSON.stringify(model.projectBoundaryStats(projectValue))),
  {
    onDevice: 2,
    localNetwork: 1,
    local: 3,
    hosted: 1,
    unknown: 1,
    total: 5,
    onDeviceFraction: 0.4,
    localNetworkFraction: 0.2,
    hostedFraction: 0.2,
    unknownFraction: 0.2
  }
);
assert.equal(model.projectValue('{"schema_version":"wrong"}').valid, false);
const aboveBaselinePayload = structuredClone(projectValuePayload);
aboveBaselinePayload.accounting.saved = -0.02;
aboveBaselinePayload.accounting.saved_pct = -25;
assert.equal(
  model.projectValueSavingsLabel(model.projectValue(JSON.stringify(aboveBaselinePayload))),
  "$0.02 above baseline"
);
const missingCorrectionField = structuredClone(projectValuePayload);
delete missingCorrectionField.quality.correction_rate_pct;
assert.equal(model.projectValue(JSON.stringify(missingCorrectionField)).valid, false);

const setupBase = {
  localEndpoint: true,
  binaryInstalled: true,
  configPath: "/home/tom/.config/wayfinder/wayfinder-router.toml",
  configChecked: true,
  configExists: false,
  doctorChecked: false,
  configValid: false,
  unitInstalled: false,
  systemdActive: false,
  reachable: false,
  missingEnvironment: []
};
const routerMissing = { ...setupBase, binaryInstalled: false };
assert.equal(model.setupState(routerMissing).actionable, true);
assert.equal(model.setupState(routerMissing).action, "Set up Wayfinder");
assert.match(model.setupState(routerMissing).detail, /SHA-256/);
assert.equal(model.setupState(setupBase).action, "Set up Wayfinder");
assert.equal(model.setupState(setupBase).step, "policy");

const existingInvalid = { ...setupBase, configExists: true, doctorChecked: true };
assert.equal(model.setupState(existingInvalid).action, "Recheck policy");
assert.match(model.setupState(existingInvalid).detail, /never overwritten/i);

const serviceNeeded = {
  ...existingInvalid,
  configValid: true,
  missingEnvironment: ["OPENAI_API_KEY"]
};
assert.equal(model.setupState(serviceNeeded).action, "Install service");
assert.match(model.setupState(serviceNeeded).detail, /OPENAI_API_KEY/);

const ready = {
  ...serviceNeeded,
  unitInstalled: true,
  systemdActive: true,
  reachable: true
};
assert.equal(model.setupState(ready).ready, true);
assert.equal(model.setupState(ready).action, "Restart service");

const parsedModels = model.models(JSON.stringify({
  models: [
    { name: "local", endpoint: "http://127.0.0.1:11434/v1", provider: "openai-compatible", model: "qwen", tier: "local", key_ok: true },
    { name: "cloud", endpoint: "https://api.example.com/v1", provider: "openai-compatible", model: "frontier", tier: "cloud", key_ok: false }
  ],
  dry_run: false
}));
assert.equal(parsedModels.valid, true);
assert.equal(model.isLocalModel(parsedModels.models[0]), true);
assert.equal(model.isLocalModel(parsedModels.models[1]), false);

const parsedRecent = model.recent(JSON.stringify({
  total: 3,
  by_model: { local: 2, cloud: 1 },
  recent: [{
    request_id: "abc",
    model: "local",
    served_by: "cloud",
    score: 0.2,
    mode: "scored",
    policy_version: "policy-v1",
    snapshot_id: "snapshot-v1",
    policy_profile: "workspace",
    execution_boundary: "hosted",
    outcome: "failed",
    http_status: 502,
    error_type: "wayfinder_router_upstream_error",
    workspace: "project-abc",
    ts: 1700000000
  }]
}));
const stats = model.routingStats(parsedRecent, parsedModels.models);
assert.equal(stats.local, 2);
assert.equal(stats.hosted, 1);
assert.equal(stats.total, 3);
assert.equal(parsedRecent.recent[0].servedBy, "cloud");
assert.equal(parsedRecent.recent[0].executionBoundary, "hosted");
assert.equal(parsedRecent.recent[0].policyProfile, "workspace");
assert.equal(parsedRecent.recent[0].httpStatus, 502);
assert.equal(model.routeTitle(parsedRecent.recent[0]), "cloud");
assert.equal(model.outcomeLabel(parsedRecent.recent[0].outcome), "FAILED");
assert.match(model.receiptDetail(parsedRecent.recent[0]), /HOSTED · PROFILE workspace/);
assert.match(model.receiptDetail(parsedRecent.recent[0]), /Selected local · Automatic · score 0.20/);
assert.equal(model.receiptNeedsAttention(parsedRecent.recent[0]), true);
assert.equal(model.latestActionableReceipt(parsedRecent.recent), parsedRecent.recent[0]);
assert.equal(model.workspaceReceipts(parsedRecent, "project-abc", 4, true).length, 1);
assert.equal(model.workspaceReceipts(parsedRecent, "other-project", 4, true).length, 0);
assert.equal(
  model.receiptRemediation(parsedRecent.recent[0]),
  "Retry, then inspect Router logs if it repeats."
);

const cachedReceipt = model.recent(JSON.stringify({
  total: 1,
  recent: [{
    request_id: "cache",
    model: "local",
    served_by: "local",
    mode: "pinned",
    execution_boundary: "on-device",
    outcome: "cache-hit",
    ts: 1700000000
  }]
})).recent[0];
assert.equal(model.boundaryLabel(cachedReceipt.executionBoundary), "ON DEVICE");
assert.equal(model.outcomeLabel(cachedReceipt.outcome), "CACHE");
assert.equal(model.routeReason(cachedReceipt), "Pinned by request");
assert.equal(model.receiptRemediation(cachedReceipt), "");

assert.equal(
  model.receiptRemediation({ outcome: "failed", errorType: "wayfinder_router_unauthorized" }),
  "Check the provider credential."
);
assert.equal(
  model.receiptRemediation({ outcome: "failed", errorType: "wayfinder_router_upstream_error", httpStatus: 401 }),
  "Check the provider credential."
);
assert.equal(
  model.receiptRemediation({ outcome: "failed", errorType: "wayfinder_router_misconfigured" }),
  "Run wayfinder-router doctor and repair the policy."
);
assert.equal(
  model.receiptRemediation({ outcome: "cancelled", errorType: "wayfinder_router_cancelled" }),
  "Client disconnected before completion."
);

assert.deepEqual(
  JSON.parse(JSON.stringify(model.serviceInstallArguments("http://localhost:9000", "/tmp/router.toml"))),
  ["service", "install", "--host", "localhost", "--port", "9000", "--config", "/tmp/router.toml"]
);
assert.equal(model.serviceInstallArguments("https://router.example.com", ""), null);
assert.equal(model.savingsLabel(model.savings('{"requests":4,"saved":1.234,"saved_pct":12.5,"unit":"usd","priced":true}')), "$1.23 saved");

console.log("Wayfinder Omarchy model tests passed");
