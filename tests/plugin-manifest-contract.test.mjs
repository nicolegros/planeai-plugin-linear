import assert from "node:assert/strict";
import fs from "node:fs";
import test from "node:test";

const manifest = JSON.parse(fs.readFileSync(new URL("../planeai-plugin.json", import.meta.url), "utf8"));
const v7Capabilities = new Set([
  "settings", "projects.read", "sessions.read", "sessions.repository-context", "sessions.prompt",
  "session-events", "sessions.actions", "sessions.advisories", "sessions.complete", "sessions.start", "tasks.read",
  "tasks.create", "tasks.transition", "tasks.sync", "task-events", "providers", "tools",
]);

test("manifest declares host API v7 and the task sync capabilities", () => {
  assert.equal(manifest.id, "linear");
  assert.equal(manifest.source_kind, "local");
  assert.equal(manifest.host_api_version, "planeai.plugin-host.v7");
  assert.deepEqual(manifest.capabilities, ["settings", "projects.read", "tasks.read", "tasks.create", "tasks.sync", "task-events"]);
  assert.ok(manifest.capabilities.every((capability) => v7Capabilities.has(capability)));
});

test("manifest runs linear.syncNow as the background service on sync_interval_ms", () => {
  assert.deepEqual(manifest.background_service, {
    method: "linear.syncNow",
    interval_setting: "sync_interval_ms",
    default_interval_ms: 60000,
  });
});

test("manifest declares the Jira-equivalent UI contributions", () => {
  assert.deepEqual(
    manifest.ui_contributions.map(({ id, placement, entrypoint }) => [id, placement, entrypoint]),
    [
      ["preferences", "preferences", "ui/preferences.js"],
      ["section", "sidebar.section", "ui/sidebar.js"],
      ["departed-interaction", "interaction", "ui/departures.js"],
      ["dashboard", "main-pane", "ui/preferences.js"],
      ["assign", "dialog", "ui/assign.js"],
    ],
  );
  assert.equal(manifest.ui_contributions.find((contribution) => contribution.id === "section").order, 100);
});

test("manifest UI contributions have unique ids and bundled entrypoints", () => {
  const ids = manifest.ui_contributions.map((contribution) => contribution.id);
  assert.equal(new Set(ids).size, ids.length);
  for (const contribution of manifest.ui_contributions) {
    assert.ok(fs.existsSync(new URL(`../${contribution.entrypoint.replace(/\.js$/, ".ts")}`, import.meta.url)), contribution.entrypoint);
  }
});
