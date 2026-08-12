import assert from "node:assert/strict";
import { access, readFile } from "node:fs/promises";
import { constants } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const manifestPath = path.join(root, "manifest.json");
const manifest = JSON.parse(await readFile(manifestPath, "utf8"));

assert.equal(manifest.schemaVersion, 1);
assert.equal(manifest.id, "io.github.wayfinderstack.wayfinder");
assert.match(manifest.id, /^[a-z0-9]+(?:[._-][a-z0-9]+)+$/);
assert.equal(manifest.name, "Wayfinder");
assert.match(manifest.version, /^\d+\.\d+\.\d+$/);
assert.ok(Array.isArray(manifest.kinds) && manifest.kinds.length > 0);
assert.ok(manifest.kinds.includes("service"));
assert.ok(manifest.kinds.includes("bar-widget"));
assert.equal(typeof manifest.entryPoints, "object");

for (const kind of manifest.kinds) {
  const key = kind === "bar-widget" ? "barWidget" : kind;
  const entry = manifest.entryPoints[key];
  assert.equal(typeof entry, "string", `missing ${key} entry point`);
  assert.ok(!path.isAbsolute(entry) && !entry.includes(".."), `unsafe entry point: ${entry}`);
  await access(path.join(root, entry), constants.R_OK);
}

for (const required of ["README.md", "LICENSE", "NOTICE", "Model.js", "RouteMark.qml", "install.sh", "uninstall.sh"]) {
  await access(path.join(root, required), constants.R_OK);
}

const service = await readFile(path.join(root, "Service.qml"), "utf8");
const widget = await readFile(path.join(root, "BarWidget.qml"), "utf8");
assert.ok(service.includes("wayfinder-router.service"));
assert.ok(service.includes("/healthz"));
assert.ok(widget.includes(manifest.id));
assert.ok(!service.match(/api[_-]?key\s*[:=]\s*["'][^"']+/i), "QML must not contain an API key");

console.log(`validated ${manifest.id} ${manifest.version}`);
