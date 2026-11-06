import type { LinearContext, SidebarRow, Toast } from "../ui/host.js";

type Answer = (params: any) => unknown;

/** A recording stand-in for PlaneAI's local frame bridge; sidecar and host RPC answers are scripted. */
export function fakeHost(
  answers: Record<string, Answer> = {},
  rpcAnswers: Record<string, Answer> = {},
  { v7 = true } = {},
) {
  const calls: Array<{ method: string; params: unknown }> = [];
  const rpcCalls: Array<{ method: string; params: unknown }> = [];
  const navigation: string[] = [];
  const selected: string[] = [];
  const notifications: string[] = [];
  const registrations: SidebarRow[][] = [];
  const unregistered = new Set<SidebarRow[]>();
  const toasts: Toast[] = [];
  const dismissed: string[] = [];
  const dataListeners = new Set<() => void>();
  const taskListeners = new Set<() => void>();
  let changes = 0;
  let projectForm: () => Promise<{ id: string; name: string; path: string } | null> = async () =>
    null;
  const respond = async (table: Record<string, Answer>, method: string, params: unknown) => {
    const answer = table[method];
    if (!answer) throw new Error(`unexpected call ${method}`);
    return answer(params);
  };
  const context: LinearContext = {
    host: {
      call: (async (method: string, params: unknown = null) => {
        calls.push({ method, params });
        return respond(answers, method, params);
      }) as LinearContext["host"]["call"],
      rpc: {
        call: (async (method: string, params: unknown = null) => {
          rpcCalls.push({ method, params });
          return respond(rpcAnswers, method, params);
        }) as LinearContext["host"]["rpc"]["call"],
      },
      navigation: {
        open: (pluginId, contributionId) => navigation.push(`open:${pluginId}:${contributionId}`),
        close: () => navigation.push("close"),
        openPreferences: () => navigation.push("preferences"),
      },
      sidebar: {
        register: (rows) => {
          registrations.push(rows);
          return () => unregistered.add(rows);
        },
        select: (rowId) => selected.push(rowId),
        handleKeydown: () => {},
      },
      data: {
        changed: async () => {
          changes += 1;
        },
        onChanged: (listener) => {
          dataListeners.add(listener);
          return () => dataListeners.delete(listener);
        },
        ...(v7
          ? {
              onTaskDataChanged: (listener: () => void) => {
                taskListeners.add(listener);
                return () => taskListeners.delete(listener);
              },
            }
          : {}),
        notify: (message) => notifications.push(message),
      },
      ...(v7
        ? {
            interaction: {
              showToast: async (toast: Toast) => {
                toasts.push(toast);
              },
              dismissToast: async (id: string) => {
                dismissed.push(id);
              },
              openProjectForm: () => projectForm(),
            },
          }
        : {}),
    },
  };
  return {
    context,
    calls,
    rpcCalls,
    navigation,
    selected,
    notifications,
    registrations,
    toasts,
    dismissed,
    /**
     * Sends row focus changes the way PlaneAI does: one batch addressed to the registration the
     * host last learned about, where the frame drops each event whose registration is gone.
     */
    focusRows: (events: Array<[rowId: string, active: boolean]>) => {
      const known = registrations.at(-1)!;
      for (const [rowId, active] of events) {
        if (!unregistered.has(known)) known.find((row) => row.id === rowId)?.onFocus?.(active);
      }
    },
    changes: () => changes,
    methods: () => calls.map((call) => call.method),
    dataChanged: () => dataListeners.forEach((listener) => listener()),
    taskDataChanged: () => taskListeners.forEach((listener) => listener()),
    listeners: () => dataListeners.size + taskListeners.size,
    onProjectForm: (next: typeof projectForm) => {
      projectForm = next;
    },
  };
}

export function mountInto(
  entrypoint: { mount(root: HTMLElement, context: LinearContext): () => void },
  context: LinearContext,
) {
  const root = document.createElement("div");
  document.body.replaceChildren(root);
  const dispose = entrypoint.mount(root, context);
  return { root, dispose };
}
