import assert from "node:assert/strict";
import { beforeEach, test } from "node:test";
import { installDom, press, settle } from "./ui-harness.js";
import { fakeHost, mountInto } from "./fake-host.js";
import sidebar from "../ui/sidebar.js";

const items = [
  { issue_id: "uuid-1", identifier: "ENG-1", title: "Sync the sidebar", status: "todo" },
  { issue_id: "uuid-2", identifier: "ENG-2", title: "Ship the plugin", status: "in_review" },
];

beforeEach(() => {
  installDom();
});

function mountSidebar(answers = {}) {
  let current = items;
  const host = fakeHost({
    "linear.sidebar.items": () => ({ items: current }),
    "linear.assignment.select": () => ({ selected: true }),
    ...answers,
  });
  const { root, dispose } = mountInto(sidebar, host.context);
  return { host, root, dispose, setItems: (next: typeof items) => (current = next) };
}

const rows = (root: Element) => [...root.querySelectorAll<HTMLButtonElement>(".issue")];

test("lists synced issues with their identifier, title, status dot and accessible labels", async () => {
  const { root } = mountSidebar();
  await settle();
  assert.equal(root.querySelector(".section-header")?.textContent, "Linear 2");
  assert.deepEqual(
    rows(root).map((row) => row.getAttribute("aria-label")),
    ["ENG-1: Sync the sidebar. Status: To do", "ENG-2: Ship the plugin. Status: In review"],
  );
  assert.deepEqual(
    rows(root).map((row) => row.querySelector(".dot")?.className),
    ["dot", "dot active"],
  );
  assert.equal(root.querySelector("[role='group']")?.getAttribute("aria-label"), "Linear issues");
});

test("registers the header and each issue for sidebar keyboard navigation", async () => {
  const { host } = mountSidebar();
  await settle();
  assert.deepEqual(host.registrations.at(-1)?.map((row) => row.id), ["header", "issue:uuid-1", "issue:uuid-2"]);
});

test("Enter on an issue selects it, then opens the assign dialog for it", async () => {
  const { host, root } = mountSidebar();
  await settle();
  press("Enter", rows(root)[1]);
  await settle();
  assert.deepEqual(host.selected, ["issue:uuid-2"]);
  assert.deepEqual(host.calls.find((call) => call.method === "linear.assignment.select")?.params, { issue_id: "uuid-2" });
  assert.deepEqual(host.navigation, ["open:linear:assign"]);
});

test("keyboard selection through the host registration opens the dialog too", async () => {
  const { host } = mountSidebar();
  await settle();
  host.registrations.at(-1)?.find((row) => row.id === "issue:uuid-1")?.onSelect?.();
  await settle();
  assert.deepEqual(host.navigation, ["open:linear:assign"]);
});

test("a failed selection is reported instead of opening the dialog", async () => {
  const { host, root } = mountSidebar({
    "linear.assignment.select": () => {
      throw new Error("this Linear issue is no longer available");
    },
  });
  await settle();
  rows(root)[0].click();
  await settle();
  assert.deepEqual(host.navigation, []);
  assert.deepEqual(host.notifications, ["Could not open ENG-1: this Linear issue is no longer available"]);
});

test("the header collapses the list and leaves only itself registered", async () => {
  const { host, root } = mountSidebar();
  await settle();
  root.querySelector<HTMLButtonElement>(".section-header")!.click();
  assert.equal(rows(root).length, 0);
  assert.equal(root.querySelector(".section-header")?.getAttribute("aria-expanded"), "false");
  assert.deepEqual(host.registrations.at(-1)?.map((row) => row.id), ["header"]);
});

test("refreshes when plugin data or PlaneAI task data changes, and unsubscribes on unmount", async () => {
  const { host, root, dispose, setItems } = mountSidebar();
  await settle();
  setItems([items[0]]);
  host.taskDataChanged();
  await settle();
  assert.equal(rows(root).length, 1);
  setItems([]);
  host.dataChanged();
  await settle();
  assert.equal(rows(root).length, 0);
  dispose();
  assert.equal(host.listeners(), 0);
});
