import assert from "node:assert/strict";
import { beforeEach, test } from "node:test";
import { deferred, installDom, settle } from "./ui-harness.js";
import { fakeHost, mountInto } from "./fake-host.js";
import departures from "../ui/departures.js";

const one = {
  issue_id: "uuid-1",
  identifier: "ENG-1",
  task_key: "ENG-1",
  title: "Gone from the view",
};
const two = { issue_id: "uuid-2", identifier: "ENG-2", task_key: "ENG-2", title: "Also gone" };

beforeEach(() => {
  installDom();
});

function mountDepartures(answers = {}) {
  let queue = [one, two];
  const host = fakeHost({
    "linear.departures.list": () => ({ items: queue }),
    "linear.departures.resolve": ({ issue_id }) => {
      queue = queue.filter((item) => item.issue_id !== issue_id);
      return { resolved: true };
    },
    "linear.departures.dequeue": ({ issue_id }) => {
      queue = queue.filter((item) => item.issue_id !== issue_id);
      return { dequeued: true };
    },
    ...answers,
  });
  const mounted = mountInto(departures, host.context);
  return { host, ...mounted, queue: () => queue };
}

const visible = (toast) => ({
  ...toast,
  actions: toast.actions?.map(({ key, label }) => ({ key, label })),
  dismiss: typeof toast.dismiss,
});

test("shows the next departed issue in PlaneAI's toast stack, rendering nothing itself", async () => {
  const { host, root } = mountDepartures();
  await settle();
  assert.equal(root.childElementCount, 0);
  assert.deepEqual(visible(host.toasts.at(-1)), {
    id: "departed",
    kind: "warning",
    message: "ENG-1 left your Linear view",
    detail: "Gone from the view",
    actions: [{ key: "Enter", label: "Done" }],
    dismiss: "function",
  });
});

test("Done resolves through the sidecar, then shows the next issue", async () => {
  const { host } = mountDepartures();
  await settle();
  await host.toasts.at(-1)!.actions![0].run();
  await settle();
  assert.deepEqual(host.calls.find((call) => call.method === "linear.departures.resolve")?.params, {
    issue_id: "uuid-1",
  });
  assert.equal(host.toasts.at(-1)!.message, "ENG-2 left your Linear view");
  assert.equal(host.changes(), 1);
});

test("a failed Done keeps the issue queued and reports it", async () => {
  const { host } = mountDepartures({
    "linear.departures.resolve": () => {
      throw new Error("failed to mark ENG-1 done: not granted");
    },
  });
  await settle();
  await host.toasts.at(-1)!.actions![0].run();
  await settle();
  assert.deepEqual(host.notifications, [
    "Could not mark ENG-1 done: failed to mark ENG-1 done: not granted",
  ]);
  assert.equal(host.toasts.at(-1)!.message, "ENG-1 left your Linear view");
});

test("dismissing dequeues the issue without marking it done", async () => {
  const { host, queue } = mountDepartures();
  await settle();
  const dismiss = host.toasts.at(-1)!.dismiss as () => Promise<void>;
  await dismiss();
  await settle();
  assert.deepEqual(
    host.methods().filter((method) => method !== "linear.departures.list"),
    ["linear.departures.dequeue"],
  );
  assert.deepEqual(queue(), [two]);
  assert.equal(host.toasts.at(-1)!.message, "ENG-2 left your Linear view");
});

test("shows progress while Done runs and ignores data refreshes until it settles", async () => {
  const pending = deferred<unknown>();
  const { host } = mountDepartures({ "linear.departures.resolve": () => pending.promise });
  await settle();
  const done = host.toasts.at(-1)!.actions![0].run();
  await settle();
  assert.deepEqual(visible(host.toasts.at(-1)), {
    id: "departed",
    kind: "warning",
    message: "Marking ENG-1 done…",
    detail: "Gone from the view",
    actions: undefined,
    dismiss: "string",
  });
  assert.equal(host.toasts.at(-1)!.dismiss, "disabled");
  const shown = host.toasts.length;
  host.dataChanged();
  await settle();
  assert.equal(host.toasts.length, shown);
  pending.resolve({ resolved: true });
  await done;
  await settle();
  assert.equal(host.toasts.at(-1)!.message, "ENG-1 left your Linear view");
});

test("an empty queue dismisses the toast, and unmounting removes it", async () => {
  const { host, dispose } = mountDepartures({ "linear.departures.list": () => ({ items: [] }) });
  await settle();
  assert.deepEqual(host.toasts, []);
  assert.deepEqual(host.dismissed, ["departed"]);
  dispose();
  assert.deepEqual(host.dismissed, ["departed", "departed"]);
});

test("without host API v7 it asks for a newer PlaneAI instead of failing", async () => {
  const host = fakeHost({}, {}, { v7: false });
  mountInto(departures, host.context);
  assert.deepEqual(host.notifications, [
    "Departed Linear issues need PlaneAI with plugin host API v7.",
  ]);
});
