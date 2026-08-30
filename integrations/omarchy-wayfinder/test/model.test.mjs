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
  version: "2026.8.1",
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
