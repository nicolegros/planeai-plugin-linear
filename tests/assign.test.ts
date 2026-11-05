import assert from "node:assert/strict";
import { beforeEach, test } from "node:test";
import { installDom, press, settle } from "./ui-harness.js";
import { fakeHost, mountInto } from "./fake-host.js";
import assign from "../ui/assign.js";

const issue = {
  issue_id: "uuid-1",
  identifier: "OPS-40",
  task_key: "ENG-1",
  title: "Ship the plugin",
  description: "Copy the GitHub plugin layout.",
};
const planeai = { id: "project-1", name: "PlaneAI", path: "/code/planeai", hidden: false };
const archive = { id: "project-2", name: "Archive", path: "/code/archive", hidden: false };

beforeEach(() => {
  installDom();
});

function mountAssign({ projects = [planeai, archive], createChild = (_params: unknown) => ({ task: { key: "PLA-9" } }), current = () => issue } = {}) {
  let projectList = projects;
  const host = fakeHost(
    { "linear.assignment.current": current },
    {
      "projects.list": () => ({ projects: projectList }),
      "tasks.createChild": (params) => createChild(params),
    },
  );
  const mounted = mountInto(assign, host.context);
  return { host, ...mounted, setProjects: (next: typeof projects) => (projectList = next) };
}

const picker = (root: Element) => root.querySelector<HTMLInputElement>("input[aria-label='PlaneAI project']")!;
const assignButton = (root: Element) => root.querySelector<HTMLButtonElement>("button.primary")!;

async function choose(root: Element, name: string) {
  const input = picker(root);
  input.dispatchEvent(new FocusEvent("focus"));
  input.value = name.slice(0, 3);
  input.dispatchEvent(new Event("input"));
  const option = [...root.querySelectorAll<HTMLButtonElement>("[role='option']")].find((candidate) => candidate.textContent === name)!;
  option.click();
  await settle();
}

test("previews the selected issue and keeps Assign disabled until a project is chosen", async () => {
  const { root } = mountAssign();
  await settle();
  assert.equal(root.querySelector(".preview .key")?.textContent, "OPS-40");
  assert.equal(root.querySelector(".preview h3")?.textContent, "Ship the plugin");
  assert.equal(root.querySelector(".preview p")?.textContent, "Copy the GitHub plugin layout.");
  assert.equal(assignButton(root).disabled, true);
  assert.equal(root.querySelector("[data-keyboard-mode]")?.textContent, "NORMALpress a key to focus field");
});

test("Mod+Enter creates a child of the issue's task in the chosen project, then closes", async () => {
  const { host, root } = mountAssign();
  await settle();
  await choose(root, "Archive");
  assert.equal(picker(root).value, "Archive");
  assert.equal(assignButton(root).disabled, false);
  press("Enter", root.querySelector("form")!, { metaKey: true });
  await settle();
  const created = host.rpcCalls.find((call) => call.method === "tasks.createChild")!.params as Record<string, string>;
  assert.match(created.operation_id, /^linear-assign:uuid-1:[0-9a-f-]{36}$/);
  assert.deepEqual({ ...created, operation_id: "" }, {
    project_path: "/code/archive",
    parent_key: "ENG-1",
    title: "Ship the plugin",
    description: "Copy the GitHub plugin layout.",
    operation_id: "",
  });
  assert.equal(host.changes(), 1);
  assert.deepEqual(host.navigation, ["close"]);
});

test("a failed assignment shows the error and stays open for a retry with the same operation", async () => {
  let attempts = 0;
  const operations: string[] = [];
  const { host, root } = mountAssign({
    createChild: (params: any) => {
      operations.push(params.operation_id);
      attempts += 1;
      if (attempts === 1) throw new Error("project is hidden");
      return { task: { key: "PLA-9" } };
    },
  });
  await settle();
  await choose(root, "PlaneAI");
  root.querySelector("form")!.dispatchEvent(new Event("submit", { cancelable: true }));
  await settle();
  assert.equal(root.querySelector("[role='alert']")?.textContent, "project is hidden");
  assert.deepEqual(host.navigation, []);
  root.querySelector("form")!.dispatchEvent(new Event("submit", { cancelable: true }));
  await settle();
  assert.deepEqual(host.navigation, ["close"]);
  assert.equal(operations[0], operations[1]);
});

test("N opens PlaneAI's project form and selects the project it created", async () => {
  const { host, root, setProjects } = mountAssign();
  const created = { id: "project-3", name: "Linear plugin", path: "/code/linear" };
  host.onProjectForm(async () => {
    setProjects([planeai, archive, { ...created, hidden: false }]);
    return created;
  });
  await settle();
  press("n", root.querySelector(".assignment")!);
  await settle();
  assert.equal(picker(root).value, "Linear plugin");
  assert.equal(assignButton(root).disabled, false);
  assert.equal(host.rpcCalls.filter((call) => call.method === "projects.list").length, 2);
});

test("P focuses the project picker in insert mode; Escape returns to normal mode, then closes", async () => {
  const { host, root } = mountAssign();
  await settle();
  const wrapper = root.querySelector<HTMLElement>(".assignment")!;
  press("p", wrapper);
  assert.equal(document.activeElement, picker(root));
  assert.equal(root.querySelector("[data-keyboard-mode]")?.textContent, "INSERTesc → normal mode");
  press("Escape", picker(root));
  assert.equal(root.querySelector("[data-keyboard-mode]")?.textContent, "INSERTesc → normal mode", "the first Escape closes the open list");
  press("Escape", picker(root));
  assert.equal(root.querySelector("[data-keyboard-mode]")?.textContent, "NORMALpress a key to focus field");
  assert.deepEqual(host.navigation, []);
  press("Escape", wrapper);
  assert.deepEqual(host.navigation, ["close"]);
});

test("without projects it offers to create one", async () => {
  const { root } = mountAssign({ projects: [] });
  await settle();
  assert.equal(root.querySelector(".muted")?.textContent, "No PlaneAI projects available. Press N to create one.");
  assert.equal(picker(root), null);
});

test("an issue that is no longer selectable shows the sidecar's reason", async () => {
  const { root } = mountAssign({
    current: () => {
      throw new Error("this Linear issue is no longer available");
    },
  });
  await settle();
  assert.equal(root.querySelector("[role='alert']")?.textContent, "this Linear issue is no longer available");
});
