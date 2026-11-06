import assert from "node:assert/strict";
import { beforeEach, test } from "node:test";
import { change, deferred, installDom, settle } from "./ui-harness.js";
import { fakeHost, mountInto } from "./fake-host.js";
import preferences, { DEFAULT_FILTER } from "../ui/preferences.js";

const disconnected = {
  connected: false, authorizing: false, oauth_available: false, auth_kind: null,
  organization: null, last_error: null, last_writeback: null,
};
const connected = {
  ...disconnected, connected: true, auth_kind: "api_key", organization: { id: "org-1", name: "Acme", url_key: "acme" },
};
const engSource = {
  query: { kind: "filter", filter: { team: { key: { eq: "ENG" } } } },
  status_map: { "In QA": "in_review" },
  writeback: null,
};

beforeEach(() => {
  installDom();
});

function mountPreferences({ status = connected as any, settings = { sync_interval_ms: 60000, sources: { eng: engSource } } as any, answers = {} } = {}) {
  let currentStatus = status;
  const saved: any[] = [];
  const host = fakeHost({
    "linear.status": () => currentStatus,
    "linear.settings.get": () => settings,
    "linear.settings.update": (next) => {
      saved.push(next);
      return next;
    },
    ...answers,
  });
  const mounted = mountInto(preferences, host.context);
  return { host, ...mounted, saved, setStatus: (next) => (currentStatus = next) };
}

const buttonNamed = (root: Element, name: string) =>
  [...root.querySelectorAll<HTMLButtonElement>("button")].find((button) => button.textContent === name);
const fieldNamed = <T extends HTMLElement>(root: Element, label: string) => {
  const heading = [...root.querySelectorAll("label")].find((candidate) => candidate.textContent === label)!;
  return root.querySelector<T>(`#${heading.htmlFor}`)!;
};

test("connects with a pasted personal API key", async () => {
  const { host, root, setStatus } = mountPreferences({
    status: disconnected,
    answers: { "linear.connect.apiKey": () => ({ connected: true, organization_name: "Acme" }) },
  });
  await settle();
  assert.equal(root.querySelector(".card .muted")?.textContent, "Not connected");
  assert.equal(buttonNamed(root, "Connect with Linear"), undefined, "this build has no OAuth client");
  const key = root.querySelector<HTMLInputElement>("input[aria-label='Linear API key']")!;
  assert.equal(key.type, "password");
  assert.equal(buttonNamed(root, "Connect")!.disabled, true);
  key.value = "lin_api_secret";
  key.dispatchEvent(new Event("input"));
  setStatus(connected);
  buttonNamed(root, "Connect")!.click();
  await settle();
  assert.deepEqual(host.calls.find((call) => call.method === "linear.connect.apiKey")?.params, { key: "lin_api_secret" });
  assert.equal(root.querySelector(".card .muted")?.textContent, "Connected to Acme (acme) with an API key");
  assert.equal(host.changes(), 1);
});

test("offers OAuth when the build has a client id, and runs the browser flow with one attempt id", async () => {
  const { host, root, setStatus } = mountPreferences({
    status: { ...disconnected, oauth_available: true },
    answers: {
      "linear.connect.start": () => ({ authorization_url: "https://linear.app/oauth/authorize?x" }),
      "linear.open_browser": () => ({ opened: true }),
      "linear.connect.complete": () => {
        setStatus({ ...connected, auth_kind: "oauth" });
        return { authorizing: true };
      },
    },
  });
  await settle();
  buttonNamed(root, "Connect with Linear")!.click();
  await settle();
  const attempts = host.calls.filter((call) => call.method.startsWith("linear.connect")).map((call) => (call.params as any).attempt_id);
  assert.equal(new Set(attempts).size, 1);
  assert.deepEqual(host.calls.find((call) => call.method === "linear.open_browser")?.params, { url: "https://linear.app/oauth/authorize?x" });
  assert.equal(root.querySelector(".card .muted")?.textContent, "Connected to Acme (acme) with OAuth");
});

test("adds a source that syncs the user's open issues", async () => {
  const { root, saved } = mountPreferences();
  await settle();
  buttonNamed(root, "Add source")!.click();
  await settle();
  assert.deepEqual(saved.at(-1).sources.source, { query: { kind: "filter", filter: DEFAULT_FILTER }, status_map: {}, writeback: null });
  assert.deepEqual(DEFAULT_FILTER, {
    or: [{ assignee: { isMe: { eq: true } } }, { assignee: { null: true } }],
    state: { type: { nin: ["completed", "canceled"] } },
  });
});

test("edits the IssueFilter JSON and refuses to save invalid JSON", async () => {
  const { root, saved } = mountPreferences();
  await settle();
  const filter = fieldNamed<HTMLTextAreaElement>(root, "IssueFilter JSON");
  change(filter, "{ not json");
  assert.match(root.querySelector("[role='alert']")!.textContent!, /^IssueFilter is not valid JSON: /);
  change(fieldNamed<HTMLTextAreaElement>(root, "IssueFilter JSON"), "[]");
  assert.equal(root.querySelector("[role='alert']")!.textContent, "IssueFilter must be a JSON object");
  assert.deepEqual(saved, []);
  change(fieldNamed<HTMLTextAreaElement>(root, "IssueFilter JSON"), '{"team":{"key":{"eq":"OPS"}}}');
  await settle();
  assert.deepEqual(saved.at(-1).sources.eng.query, { kind: "filter", filter: { team: { key: { eq: "OPS" } } } });
  assert.equal(root.querySelector("[role='alert']"), null);
});

test("switches a source to a custom view and saves it once a URL is entered", async () => {
  const { root, saved } = mountPreferences();
  await settle();
  change(root.querySelector<HTMLSelectElement>("select[aria-label='Query kind for eng']")!, "view");
  assert.deepEqual(saved, [], "an empty view is not saved");
  change(fieldNamed<HTMLInputElement>(root, "Custom view URL"), " https://linear.app/acme/view/mine-0a1b2c3d4e5f ");
  await settle();
  assert.deepEqual(saved.at(-1).sources.eng.query, { kind: "view", view: "https://linear.app/acme/view/mine-0a1b2c3d4e5f" });
});

test("edits writeback targets and the comment toggle, and clears writeback when all are unset", async () => {
  const { root, saved } = mountPreferences();
  await settle();
  change(fieldNamed<HTMLInputElement>(root, "When work starts, move to"), " In Progress ");
  await settle();
  assert.deepEqual(saved.at(-1).sources.eng.writeback, { on_start: "In Progress", on_complete: null, comment: false });
  change(fieldNamed<HTMLInputElement>(root, "When the task is done, move to"), "Done");
  await settle();
  change(root.querySelector<HTMLInputElement>("input[type='checkbox']")!, true);
  await settle();
  assert.deepEqual(saved.at(-1).sources.eng.writeback, { on_start: "In Progress", on_complete: "Done", comment: true });
  change(fieldNamed<HTMLInputElement>(root, "When work starts, move to"), "");
  await settle();
  change(fieldNamed<HTMLInputElement>(root, "When the task is done, move to"), "");
  await settle();
  change(root.querySelector<HTMLInputElement>("input[type='checkbox']")!, false);
  await settle();
  assert.equal(saved.at(-1).sources.eng.writeback, null);
});

test("edits, adds and removes status mappings", async () => {
  const { root, saved } = mountPreferences();
  await settle();
  change(root.querySelector<HTMLSelectElement>("select[aria-label='PlaneAI status for In QA']")!, "done");
  await settle();
  assert.deepEqual(saved.at(-1).sources.eng.status_map, { "In QA": "done" });
  buttonNamed(root, "Add status mapping")!.click();
  await settle();
  assert.deepEqual(saved.at(-1).sources.eng.status_map, { "In QA": "done", "Linear state": "todo" });
  root.querySelector<HTMLButtonElement>("button[aria-label='Remove status mapping for In QA']")!.click();
  await settle();
  assert.deepEqual(saved.at(-1).sources.eng.status_map, { "Linear state": "todo" });
});

test("renames a source and edits the sync interval in seconds", async () => {
  const { root, saved } = mountPreferences();
  await settle();
  change(fieldNamed<HTMLInputElement>(root, "Source name"), "engineering");
  await settle();
  assert.deepEqual(Object.keys(saved.at(-1).sources), ["engineering"]);
  change(fieldNamed<HTMLInputElement>(root, "Sync interval (seconds)"), "120");
  await settle();
  assert.equal(saved.at(-1).sync_interval_ms, 120000);
  change(fieldNamed<HTMLInputElement>(root, "Sync interval (seconds)"), "2");
  await settle();
  assert.equal(saved.length, 2);
  assert.equal(root.querySelector("[role='alert']")?.textContent, "Sync interval must be a whole number of seconds, at least 10.");
});

test("serializes saves without dropping an earlier optimistic change", async () => {
  const first = deferred<unknown>();
  const order: string[] = [];
  const { root } = mountPreferences({
    answers: {
      "linear.settings.update": async (next) => {
        order.push(JSON.stringify(next.sources.eng.status_map));
        if (order.length === 1) await first.promise;
        return next;
      },
    },
  });
  await settle();
  change(root.querySelector<HTMLSelectElement>("select[aria-label='PlaneAI status for In QA']")!, "done");
  buttonNamed(root, "Add status mapping")!.click();
  await settle();
  assert.deepEqual(order, ['{"In QA":"done"}']);
  first.resolve(null);
  await settle();
  await settle();
  assert.deepEqual(order, ['{"In QA":"done"}', '{"In QA":"done","Linear state":"todo"}']);
});

test("Sync Now shows totals and every source error", async () => {
  const { root } = mountPreferences({
    answers: {
      "linear.syncNow": () => ({
        created: 2, updated: 1, departed: 0, errors: 1,
        error_messages: ["ops: Linear GraphQL error: Invalid filter"],
      }),
    },
  });
  await settle();
  buttonNamed(root, "Sync Now")!.click();
  await settle();
  assert.equal(root.querySelector(".warning")?.textContent, "2 created · 1 updated · 0 departed · 1 error");
  assert.deepEqual([...root.querySelectorAll("ul.error li")].map((item) => item.textContent), ["ops: Linear GraphQL error: Invalid filter"]);
});

test("Sync Now names the issues a PlaneAI task's key kept from syncing", async () => {
  const { root } = mountPreferences({
    answers: {
      "linear.syncNow": () => ({ created: 1, updated: 0, departed: 0, errors: 0, skipped: ["PLA-1", "PLA-2"] }),
    },
  });
  await settle();
  buttonNamed(root, "Sync Now")!.click();
  await settle();
  assert.deepEqual([...root.querySelectorAll(".muted, .warning")].map((line) => line.textContent).slice(-2), [
    "1 created · 0 updated · 0 departed · 0 errors",
    "Not synced, because a PlaneAI task already uses the key: PLA-1, PLA-2",
  ]);
});

test("a rejected save shows the sidecar's validation error", async () => {
  const { root } = mountPreferences({
    answers: {
      "linear.settings.update": () => {
        throw new Error('Linear source "eng": requires a non-empty custom view URL or id');
      },
    },
  });
  await settle();
  buttonNamed(root, "Add status mapping")!.click();
  await settle();
  assert.equal(root.querySelector("[role='alert']")?.textContent, 'Linear source "eng": requires a non-empty custom view URL or id');
});
