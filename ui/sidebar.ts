import { errorMessage, sidecar, statusLabel, type SidebarItem } from "./api.js";
import { PLUGIN_ID, type Entrypoint, type SidebarRow } from "./host.js";
import { baseStyles } from "./styles.js";

const styles = `${baseStyles}
  .section-header,.issue { width:100%; min-height:0; display:flex; align-items:center; gap:7px; border:0; border-radius:0; text-align:left; background:transparent; }
  .section-header { padding:5px 8px; font-size:11px; font-weight:600; text-transform:uppercase; letter-spacing:.05em; color:var(--planeai-text-subtle); }
  .issue { padding:6px 8px; font-size:12px; color:var(--planeai-text); }
  .issue:hover,.section-header:hover { background:var(--planeai-surface-raised); }
  .section-header:focus,.issue:focus { outline:none; }
  .section-header.selected,.issue.selected,.section-header:focus-visible,.issue:focus-visible { outline:2px solid var(--planeai-accent); outline-offset:-2px; }
  .dot { width:7px; height:7px; border-radius:99px; background:var(--planeai-text-subtle); flex:0 0 auto; }
  .dot.active { background:var(--planeai-info); }
  .dot.done { background:var(--planeai-success); }
  .title { overflow:hidden; text-overflow:ellipsis; white-space:nowrap; }
  .count { margin-left:auto; color:var(--planeai-text-subtle); font-size:10px; }
`;

const dotClass = (item: SidebarItem) =>
  item.status === "done" ? "done" : item.status === "todo" ? "" : "active";

/** The Linear section of PlaneAI's sidebar: synced issues, opening the assign dialog on select. */
const entrypoint: Entrypoint = {
  mount(root, context) {
    const { host } = context;
    const api = sidecar(host);
    const style = document.createElement("style");
    style.textContent = styles;
    const section = document.createElement("section");
    root.replaceChildren(style, section);
    let items: SidebarItem[] = [];
    let collapsed = false;
    let selected = "";
    let disposed = false;
    let unregister = () => {};

    const assign = async (item: SidebarItem) => {
      try {
        await api.selectIssue(item.issue_id);
        host.navigation.open(PLUGIN_ID, "assign");
      } catch (error) {
        host.data.notify(`Could not open ${item.identifier}: ${errorMessage(error)}`);
      }
    };
    const focusRow = (rowId: string) => {
      selected = rowId;
      render(rowId);
    };
    const blur = (active: boolean, rowId: string) => {
      if (active) {
        if (selected !== rowId) focusRow(rowId);
      } else if (selected) {
        selected = "";
        render();
      }
    };
    const toggle = () => {
      collapsed = !collapsed;
      focusRow("header");
    };

    const render = (focusRowId: string | null = null) => {
      unregister();
      const header = document.createElement("button");
      header.className = `section-header${selected === "header" ? " selected" : ""}`;
      header.setAttribute("aria-expanded", String(!collapsed));
      header.setAttribute("aria-controls", "linear-sidebar-issues");
      header.textContent = `${collapsed ? "›" : "⌄"} Linear`;
      const count = document.createElement("span");
      count.className = "count";
      count.textContent = String(items.length);
      header.append(count);
      header.onclick = () => {
        host.sidebar.select("header");
        toggle();
      };
      header.onkeydown = (event) => host.sidebar.handleKeydown(event);
      const region = document.createElement("div");
      region.id = "linear-sidebar-issues";
      region.setAttribute("role", "group");
      region.setAttribute("aria-label", "Linear issues");
      section.replaceChildren(header, region);
      if (focusRowId === "header") header.focus();

      const rows: SidebarRow[] = [
        {
          id: "header",
          onSelect: toggle,
          onCollapse: () => {
            collapsed = true;
            focusRow("header");
          },
          onExpand: () => {
            collapsed = false;
            focusRow("header");
          },
          onFocus: (active) => blur(active, "header"),
        },
      ];
      if (!collapsed) {
        for (const item of items) {
          const rowId = `issue:${item.issue_id}`;
          const row = document.createElement("button");
          row.className = `issue${selected === rowId ? " selected" : ""}`;
          const dot = document.createElement("span");
          dot.className = `dot ${dotClass(item)}`.trim();
          const key = document.createElement("span");
          key.className = "key";
          key.textContent = item.identifier;
          const title = document.createElement("span");
          title.className = "title";
          title.textContent = item.title;
          row.append(dot, key, title);
          row.setAttribute("aria-label", `${item.identifier}: ${item.title}. Status: ${statusLabel(item.status)}`);
          row.onclick = () => {
            host.sidebar.select(rowId);
            focusRow(rowId);
            void assign(item);
          };
          row.onkeydown = (event) => {
            if (event.key === "Enter" || event.key === " ") {
              event.preventDefault();
              event.stopPropagation();
              row.click();
              return;
            }
            host.sidebar.handleKeydown(event);
          };
          region.append(row);
          if (focusRowId === rowId) row.focus();
          rows.push({
            id: rowId,
            onSelect: () => {
              focusRow(rowId);
              void assign(item);
            },
            onCollapse: () => focusRow("header"),
            onFocus: (active) => blur(active, rowId),
          });
        }
      }
      unregister = host.sidebar.register(rows);
    };

    let running = false;
    let again = false;
    const refresh = async () => {
      if (running) {
        again = true;
        return;
      }
      running = true;
      try {
        do {
          again = false;
          items = (await api.sidebarItems()).items;
          if (!disposed) render();
        } while (again && !disposed);
      } catch {
        if (!disposed) render();
      } finally {
        running = false;
      }
    };
    const requestRefresh = () => void refresh();
    const unsubscribeData = host.data.onChanged?.(requestRefresh) ?? (() => {});
    const unsubscribeTasks = host.data.onTaskDataChanged?.(requestRefresh) ?? (() => {});
    render();
    requestRefresh();
    return () => {
      disposed = true;
      unsubscribeData();
      unsubscribeTasks();
      unregister();
      root.replaceChildren();
    };
  },
};

export default entrypoint;
