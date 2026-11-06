import type { LinearHost } from "./host.js";

export type TaskStatus = "todo" | "in_progress" | "in_review" | "done";
export const TASK_STATUSES: TaskStatus[] = ["todo", "in_progress", "in_review", "done"];

export type SourceQuery =
  | { kind: "view"; view: string }
  | { kind: "filter"; filter: Record<string, unknown> };

export type Writeback = { on_start: string | null; on_complete: string | null; comment: boolean };

export type Source = {
  query: SourceQuery;
  status_map: Record<string, TaskStatus>;
  writeback: Writeback | null;
};

export type Settings = {
  sync_interval_ms: number;
  sources: Record<string, Source>;
  key_prefixes?: Record<string, string>;
};

export type WritebackOutcome = {
  identifier: string;
  source: string;
  action: "start" | "complete";
  ok: boolean;
  error: string | null;
};

export type Status = {
  connected: boolean;
  authorizing: boolean;
  oauth_available: boolean;
  auth_kind: "api_key" | "oauth" | null;
  organization: { id: string; name: string; url_key: string } | null;
  last_error: string | null;
  last_writeback: WritebackOutcome | null;
};

export type SyncTotals = {
  created: number;
  updated: number;
  departed: number;
  errors: number;
  error_messages?: string[];
  skipped?: string[];
};

export type SidebarItem = {
  issue_id: string;
  identifier: string;
  title: string;
  status: TaskStatus;
};
export type LinkedIssue = {
  issue_id: string;
  identifier: string;
  task_key: string;
  title: string;
  description: string;
};
export type DepartedIssue = {
  issue_id: string;
  identifier: string;
  task_key: string;
  title: string;
};

/** The sidecar's methods, typed. */
export function sidecar(host: LinearHost) {
  return {
    status: () => host.call<Status>("linear.status"),
    settings: () => host.call<Settings>("linear.settings.get"),
    updateSettings: (settings: Settings) => host.call<Settings>("linear.settings.update", settings),
    syncNow: () => host.call<SyncTotals>("linear.syncNow"),
    connectApiKey: (key: string) => host.call("linear.connect.apiKey", { key }),
    connectStart: (attemptId: string) =>
      host.call<{ authorization_url: string }>("linear.connect.start", { attempt_id: attemptId }),
    connectComplete: (attemptId: string) =>
      host.call("linear.connect.complete", { attempt_id: attemptId }),
    connectCancel: (attemptId: string | null) =>
      host.call("linear.connect.cancel", { attempt_id: attemptId }),
    openBrowser: (url: string) => host.call("linear.open_browser", { url }),
    disconnect: () => host.call("linear.disconnect"),
    sidebarItems: () => host.call<{ items: SidebarItem[] }>("linear.sidebar.items"),
    selectIssue: (issueId: string) => host.call("linear.assignment.select", { issue_id: issueId }),
    currentIssue: () => host.call<LinkedIssue>("linear.assignment.current"),
    departures: () => host.call<{ items: DepartedIssue[] }>("linear.departures.list"),
    resolveDeparture: (issueId: string) =>
      host.call("linear.departures.resolve", { issue_id: issueId }),
    dequeueDeparture: (issueId: string) =>
      host.call("linear.departures.dequeue", { issue_id: issueId }),
  };
}

export function statusLabel(status: TaskStatus): string {
  return { todo: "To do", in_progress: "In progress", in_review: "In review", done: "Done" }[
    status
  ];
}

export function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
