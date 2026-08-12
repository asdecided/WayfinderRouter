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
  recent: [{ request_id: "abc", model: "local", score: 0.2, mode: "scored", ts: 1700000000 }]
}));
const stats = model.routingStats(parsedRecent, parsedModels.models);
assert.equal(stats.local, 2);
assert.equal(stats.hosted, 1);
assert.equal(stats.total, 3);

assert.deepEqual(
  JSON.parse(JSON.stringify(model.serviceInstallArguments("http://localhost:9000", "/tmp/router.toml"))),
  ["service", "install", "--host", "localhost", "--port", "9000", "--config", "/tmp/router.toml"]
);
assert.equal(model.serviceInstallArguments("https://router.example.com", ""), null);
assert.equal(model.savingsLabel(model.savings('{"requests":4,"saved":1.234,"saved_pct":12.5,"unit":"usd","priced":true}')), "$1.23 saved");

console.log("Wayfinder Omarchy model tests passed");
