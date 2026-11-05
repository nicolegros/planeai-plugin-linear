import { errorMessage, sidecar, type DepartedIssue } from "./api.js";
import type { Entrypoint } from "./host.js";

const TOAST_ID = "departed";

/** Invisible driver of the departure prompt: one toast at a time for issues that left every source. */
const entrypoint: Entrypoint = {
  mount(root, context) {
    const { host } = context;
    const api = sidecar(host);
    const toasts = host.interaction;
    root.replaceChildren();
    if (!toasts) {
      host.data.notify("Departed Linear issues need PlaneAI with plugin host API v7.", "warning");
      return () => {};
    }
    let items: DepartedIssue[] = [];
    let resolving: DepartedIssue | null = null;
    let disposed = false;

    const show = () => {
      if (disposed) return;
      if (resolving) {
        void toasts.showToast({
          id: TOAST_ID,
          kind: "warning",
          message: `Marking ${resolving.identifier} done…`,
          detail: resolving.title,
          dismiss: "disabled",
        });
        return;
      }
      const item = items[0];
      if (!item) {
        void toasts.dismissToast(TOAST_ID);
        return;
      }
      void toasts.showToast({
        id: TOAST_ID,
        kind: "warning",
        message: `${item.identifier} left your Linear view`,
        detail: item.title,
        actions: [{ key: "Enter", label: "Done", run: () => resolve(item) }],
        dismiss: () => dequeue(item),
      });
    };
    const refresh = async () => {
      if (resolving) return;
      try {
        const next = (await api.departures()).items;
        if (disposed || resolving) return;
        items = next;
        show();
      } catch (error) {
        host.data.notify(`Could not load departed Linear issues: ${errorMessage(error)}`);
      }
    };
    const dequeue = async (item: DepartedIssue) => {
      try {
        await api.dequeueDeparture(item.issue_id);
      } catch (error) {
        host.data.notify(`Could not dismiss ${item.identifier}: ${errorMessage(error)}`);
      }
      await refresh();
    };
    const resolve = async (item: DepartedIssue) => {
      resolving = item;
      show();
      try {
        // The sidecar dequeues only after PlaneAI marked the task done.
        await api.resolveDeparture(item.issue_id);
        await host.data.changed();
      } catch (error) {
        host.data.notify(`Could not mark ${item.identifier} done: ${errorMessage(error)}`);
      } finally {
        resolving = null;
      }
      await refresh();
    };
    const unsubscribe = host.data.onChanged?.(() => void refresh()) ?? (() => {});
    void refresh();
    return () => {
      disposed = true;
      unsubscribe();
      void toasts.dismissToast(TOAST_ID);
    };
  },
};

export default entrypoint;
