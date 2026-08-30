import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const read = relativePath => readFile(path.join(root, relativePath), "utf8");

const [rawCompatibility, rawManifest, installer, workflow, readme, matrix, troubleshooting] =
  await Promise.all([
    read("compatibility.json"),
    read("manifest.json"),
    read("install.sh"),
    read(".github/workflows/ci.yml"),
    read("README.md"),
    read("docs/compatibility.md"),
    read("docs/troubleshooting.md"),
  ]);

const compatibility = JSON.parse(rawCompatibility);
const manifest = JSON.parse(rawManifest);
const fullCommit = /^[0-9a-f]{40}$/;
const semver = /^(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)$/;

assert.equal(compatibility.schemaVersion, 1);
assert.match(compatibility.checkedAt, /^\d{4}-\d{2}-\d{2}$/);
assert.equal(compatibility.plugin.id, manifest.id);
assert.equal(compatibility.plugin.version, manifest.version);
assert.equal(compatibility.omarchy.channel, "quattro");
assert.equal(compatibility.omarchy.version, "4.0.0.alpha");
assert.match(compatibility.omarchy.commit, fullCommit);
assert.equal(compatibility.omarchy.manifestSchemaVersion, manifest.schemaVersion);
assert.equal(compatibility.quickshell.package, "quickshell");
assert.match(compatibility.quickshell.versionFloor, semver);
assert.match(compatibility.quickshell.contractCommit, fullCommit);
assert.match(compatibility.router.release, semver);
assert.equal(compatibility.router.libc, "glibc");
assert.deepEqual(compatibility.router.architectures, [
  "x86_64-unknown-linux-gnu",
  "aarch64-unknown-linux-gnu",
]);
assert.equal(compatibility.router.projectProfiles, "available");

const installerVersion = installer.match(
  /^router_version="((?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*))"$/m
)?.[1];
assert.equal(installerVersion, compatibility.router.release);
for (const target of compatibility.router.architectures) {
  const architecture = target.split("-")[0];
  assert.ok(installer.includes(`router_sha256_${architecture}=`), `missing ${architecture} digest`);
}

const agents = new Map(compatibility.codingAgents.map(agent => [agent.id, agent]));
assert.equal(agents.size, 5);
assert.equal(agents.get("codex")?.evidence, "real-tool-round-trip");
assert.equal(agents.get("claude-code")?.evidence, "real-tool-round-trip");
assert.equal(agents.get("opencode")?.evidence, "real-stream-tool-error-cancel");
assert.equal(agents.get("pi")?.evidence, "real-stream-tool-error-cancel");
assert.equal(agents.get("aider")?.evidence, "real-stream-edit");
assert.ok(workflow.includes(`@openai/codex@${agents.get("codex")?.version}`));
assert.ok(workflow.includes(`@anthropic-ai/claude-code@${agents.get("claude-code")?.version}`));
assert.ok(workflow.includes(`opencode-linux-x64@${agents.get("opencode")?.version}`));
assert.ok(workflow.includes(`@earendil-works/pi-coding-agent@${agents.get("pi")?.version}`));
assert.ok(workflow.includes(`aider-chat==${agents.get("aider")?.version}`));
assert.ok(workflow.includes("compatibility.json"), "CI must read the Omarchy compatibility pin");

assert.equal(compatibility.lifecycle.freshInstall, "release-gated");
assert.equal(compatibility.lifecycle.standardInstallation, "contract-validated");
assert.equal(compatibility.lifecycle.existingRouterNoClobber, "release-gated");
assert.equal(compatibility.lifecycle.routerUpgrade, "contract-validated");
assert.equal(compatibility.lifecycle.routerRollback, "contract-validated");

for (const value of [
  compatibility.omarchy.commit,
  compatibility.omarchy.version,
  compatibility.quickshell.versionFloor,
  compatibility.router.release,
  agents.get("codex")?.version,
  agents.get("claude-code")?.version,
  agents.get("opencode")?.version,
  agents.get("pi")?.version,
  agents.get("aider")?.version,
]) {
  assert.ok(matrix.includes(value), `compatibility matrix must include ${value}`);
}

assert.ok(readme.includes("docs/compatibility.md"));
assert.ok(readme.includes("docs/troubleshooting.md"));
assert.ok(readme.includes("omarchy plugin add https://github.com/asdecided/omarchy-wayfinder.git --enable"));
assert.ok(troubleshooting.includes("Upgrade or roll back a plugin-owned Router"));
assert.ok(troubleshooting.includes("journalctl --user -u wayfinder-router.service"));

console.log(
  `validated compatibility: Omarchy ${compatibility.omarchy.version} @ ${compatibility.omarchy.commit}, `
  + `Quickshell ${compatibility.quickshell.versionFloor}+, Router ${compatibility.router.release}`
);
