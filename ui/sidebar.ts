import { errorMessage, sidecar, statusLabel, type SidebarItem } from "./api.js";
import { PLUGIN_ID, type Entrypoint, type SidebarRow } from "./host.js";
import { baseStyles } from "./styles.js";

// Mirrors the native sidebar's section header and rows.
const styles = `${baseStyles}
  html,body { background:transparent; } /* PlaneAI hosts older than content-sized sections paint the frame background */
  section { display:flex; flex-direction:column; gap:2px; padding:2px; }
  .section-header,.issue { width:100%; min-height:0; display:flex; align-items:center; gap:6px; border:0; border-radius:var(--planeai-radius); text-align:left; background:transparent; }
  .section-header { margin-bottom:2px; padding:4px 8px; font-size:11px; line-height:16px; font-weight:600; text-transform:uppercase; letter-spacing:.05em; color:var(--planeai-text-muted); }
  .issue { padding:6px 8px 6px 16px; font-size:13px; line-height:18px; color:var(--planeai-text); }
  .issue:hover:not(:disabled),.section-header:hover:not(:disabled) { background:var(--planeai-surface-raised); }
  .section-header:hover:not(:disabled) { color:var(--planeai-text-muted); }
  .section-header:focus,.issue:focus { outline:none; }
  .section-header.selected,.issue.selected,.section-header:focus-visible,.issue:focus-visible { box-shadow:inset 0 0 0 2px var(--planeai-accent); outline:2px solid transparent; outline-offset:-2px; }
  .chevron { width:12px; height:12px; flex:0 0 auto; color:var(--planeai-text-subtle); }
  .dot { width:6px; height:6px; border-radius:99px; background:var(--planeai-text-subtle); flex:0 0 auto; }
  .dot.active { background:var(--planeai-info); }
  .dot.done { background:var(--planeai-success); }
  .title { min-width:0; overflow:hidden; text-overflow:ellipsis; white-space:nowrap; font-weight:500; }
  .key { font-size:10px; }
  .count { margin-left:auto; font-weight:400; color:var(--planeai-text-subtle); }
`;

const chevron = (collapsed: boolean) => {
  const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  svg.setAttribute("class", "chevron");
  svg.setAttribute("viewBox", "0 0 24 24");
  svg.setAttribute("fill", "none");
  svg.setAttribute("stroke", "currentColor");
  svg.setAttribute("stroke-width", "2");
  svg.setAttribute("stroke-linecap", "round");
  svg.setAttribute("stroke-linejoin", "round");
  svg.setAttribute("aria-hidden", "true");
  const path = document.createElementNS("http://www.w3.org/2000/svg", "path");
  path.setAttribute("d", collapsed ? "m9 18 6-6-6-6" : "m6 9 6 6 6-6");
  svg.append(path);
  return svg;
};

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
      header.textContent = "Linear ";
      const count = document.createElement("span");
      count.className = "count";
      count.textContent = String(items.length);
      header.append(count, chevron(collapsed));
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
