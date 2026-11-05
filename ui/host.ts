/** The slice of PlaneAI's local plugin frame bridge this plugin uses, including host API v7. */

export type ToastKind = "error" | "warning" | "info" | "success";

export interface ToastAction {
  key: string;
  label: string;
  run(): void | Promise<void>;
}

export interface Toast {
  id: string;
  kind: ToastKind;
  message: string;
  detail?: string;
  actions?: ToastAction[];
  /** A function runs when the user dismisses the toast; "disabled" hides the dismiss control. */
  dismiss?: "enabled" | "disabled" | (() => void | Promise<void>);
}

export interface NewProject {
  id: string;
  name: string;
  path: string;
}

/** Host API v7: toasts with frame callbacks and the app's project form. */
export interface Interaction {
  showToast(toast: Toast): Promise<unknown>;
  dismissToast(id: string): Promise<void>;
  openProjectForm(): Promise<NewProject | null>;
}

export interface SidebarRow {
  id: string;
  onSelect?(): void;
  onCollapse?(): void;
  onExpand?(): void;
  onFocus?(active: boolean): void;
}

export interface LinearHost {
  /** This plugin's sidecar. */
  call<T>(method: string, params?: unknown): Promise<T>;
  /** PlaneAI's own data RPC, checked against the manifest's capabilities. */
  rpc: { call<T>(method: string, params?: unknown): Promise<T> };
  navigation: {
    open(pluginId: string, contributionId: string): void;
    close(): void;
    openPreferences(): void;
  };
  sidebar: {
    register(rows: SidebarRow[]): () => void;
    select(rowId: string): void;
    handleKeydown(event: KeyboardEvent): void;
  };
  data: {
    changed(): Promise<void>;
    onChanged?(listener: () => void): () => void;
    /** Host API v7: PlaneAI's tasks changed, for plugins holding `tasks.sync`. */
    onTaskDataChanged?(listener: () => void): () => void;
    notify(message: string, kind?: ToastKind): void;
  };
  /** Present only on hosts that grant host API v7. */
  interaction?: Interaction;
}

export interface LinearContext {
  host: LinearHost;
}

export interface Entrypoint {
  mount(root: HTMLElement, context: LinearContext): () => void;
}

export const PLUGIN_ID = "linear";
