import assert from "node:assert/strict";
import { access, readFile, stat } from "node:fs/promises";
import { constants } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const manifestPath = path.join(root, "manifest.json");
const manifest = JSON.parse(await readFile(manifestPath, "utf8"));

assert.equal(manifest.schemaVersion, 1);
assert.equal(manifest.id, "io.github.asdecided.wayfinder");
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

for (const required of [
  "README.md",
  "LICENSE",
  "NOTICE",
  "Model.js",
  "RouteMark.qml",
  "install.sh",
  "uninstall.sh",
  "scripts/router-lifecycle.sh",
  "scripts/omarchy-native-smoke.sh",
  "test/router-lifecycle.test.sh",
  "test/omarchy-native-smoke.test.sh",
]) {
  await access(path.join(root, required), constants.R_OK);
}

const service = await readFile(path.join(root, "Service.qml"), "utf8");
const widget = await readFile(path.join(root, "BarWidget.qml"), "utf8");
const installer = await readFile(path.join(root, "install.sh"), "utf8");
const uninstaller = await readFile(path.join(root, "uninstall.sh"), "utf8");
const readme = await readFile(path.join(root, "README.md"), "utf8");
const model = await readFile(path.join(root, "Model.js"), "utf8");
const installerStat = await stat(path.join(root, "install.sh"));
const nativeSmokeStat = await stat(path.join(root, "scripts/omarchy-native-smoke.sh"));
assert.notEqual(installerStat.mode & 0o111, 0, "install.sh must remain directly executable");
assert.notEqual(nativeSmokeStat.mode & 0o111, 0, "native smoke must remain directly executable");
assert.ok(service.includes("wayfinder-router.service"));
assert.ok(service.includes("/healthz"));
assert.ok(service.includes('"init", "--preset", "local", "--path"'));
assert.ok(service.includes('"doctor", "--config"'));
assert.ok(service.includes("effectiveConfigPath"));
assert.ok(service.includes('"capabilities", "--json"'));
assert.ok(service.includes('"project", "status", "--root"'));
assert.ok(service.includes('"project", "setup", "--root"'));
assert.ok(service.includes('"project", "rollback", "--root"'));
assert.ok(service.includes('"--prompt-token", "--json"'));
assert.ok(service.includes("stdinEnabled: true"));
assert.ok(service.includes("projectActionProcess.write"));
assert.ok(model.includes("function setupState(state)"));
assert.ok(model.includes("function projectState(state)"));
assert.ok(model.includes("function projectStatus(raw)"));
assert.ok(model.includes("function receiptRemediation(entry)"));
assert.ok(widget.includes(manifest.id));
assert.ok(widget.includes("FIRST RUN"));
assert.ok(widget.includes("PROJECT PROFILE"));
assert.ok(widget.includes("password: true"));
assert.ok(widget.includes("Confirm rollback"));
assert.ok(widget.includes("Model.receiptContext(modelData)"));
assert.ok(widget.includes("Model.routeReason(modelData)"));
assert.ok(widget.includes("Model.receiptRemediation(root.actionableReceipt)"));
assert.ok(!service.match(/api[_-]?key\s*[:=]\s*["'][^"']+/i), "QML must not contain an API key");
assert.ok(!service.includes("WAYFINDER_PROJECT_TOKEN"), "QML must not read or persist the project token environment");

const projectRootSetting = manifest.barWidget.schema.find(entry => entry.key === "projectRoot");
assert.ok(projectRootSetting, "manifest must expose the repository root setting");
assert.equal(projectRootSetting.type, "path");
assert.equal(manifest.barWidget.defaults.projectRoot, "");

const versionMatch = installer.match(/^router_version="(\d{4}\.\d+\.\d+)"$/m);
assert.ok(versionMatch, "installer must pin a Router CalVer release");
assert.ok(
  readme.includes(`router-v${versionMatch[1]}`),
  "README must identify the pinned Router release"
);

for (const architecture of ["x86_64", "aarch64"]) {
  const digest = installer.match(new RegExp(`^router_sha256_${architecture}="([0-9a-f]{64})"$`, "m"));
  assert.ok(digest, `installer must pin the ${architecture} archive digest`);
}

assert.ok(installer.includes("sha256sum --check --strict"), "installer must verify the archive digest");
assert.ok(installer.includes("--proto '=https'"), "installer must restrict release downloads to HTTPS");
assert.ok(!installer.includes("cargo install"), "native installation must not require Cargo");
assert.ok(!service.includes("bash -lc \"wayfinder-router init"), "setup arguments must not use shell interpolation");
assert.ok(installer.includes("binary_sha256="), "installer must record installed-binary provenance");
assert.ok(installer.includes("--upgrade-router"), "installer must expose explicit Router upgrade");
assert.ok(installer.includes("--rollback-router"), "installer must expose explicit Router rollback");
assert.ok(installer.includes("router_promote_candidate"), "installer must use the reviewed promotion contract");
assert.ok(installer.includes("router_acquire_lock"), "installer must serialize Router lifecycle actions");
assert.ok(uninstaller.includes("--remove-owned-router"), "uninstaller must require an explicit binary-removal flag");
assert.ok(uninstaller.includes("router_load_current"), "uninstaller must verify binary ownership by digest");
assert.ok(
  readme.includes("An existing\n`wayfinder-router` executable is never replaced."),
  "README must document existing-binary ownership"
);
assert.ok(readme.includes("Project profiles"), "README must document project profiles");
assert.ok(readme.includes("stdin"), "README must document the project-token stdin boundary");
assert.ok(readme.includes("docs/native-smoke.md"), "README must link the native smoke gate");

console.log(`validated ${manifest.id} ${manifest.version}`);
