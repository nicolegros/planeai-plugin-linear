import {
  TASK_STATUSES,
  errorMessage,
  sidecar,
  statusLabel,
  type Settings,
  type Source,
  type Status,
  type SyncTotals,
  type TaskStatus,
  type Writeback,
} from "./api.js";
import type { Entrypoint } from "./host.js";
import { baseStyles } from "./styles.js";

/** Open issues assigned to the connected user, a useful and valid first source. */
export const DEFAULT_FILTER = {
  or: [{ assignee: { isMe: { eq: true } } }, { assignee: { null: true } }],
  state: { type: { nin: ["completed", "canceled"] } },
};

const styles = `${baseStyles}
  .page { max-width:600px; padding:var(--planeai-space-5); display:grid; gap:28px; }
  .card { display:grid; gap:12px; }
  .card > h2 { margin:0; color:var(--planeai-text-subtle); font-size:11px; font-weight:600; letter-spacing:.05em; text-transform:uppercase; }
  .field { display:grid; gap:4px; }
  .prefix-offer { display:grid; gap:8px; }
  .prefix-offer .row { align-items:flex-end; }
  .prefix-offer .field { flex:0 0 160px; }
  .key-prefixes { display:grid; gap:12px; padding:14px; border:1px solid var(--planeai-border); border-radius:8px; background:var(--planeai-surface-raised); }
  .field > label, .mapping > .label { color:var(--planeai-text-muted); font-size:12px; }
  .source { display:grid; gap:12px; padding:14px; border:1px solid var(--planeai-border); border-radius:8px; background:var(--planeai-surface-raised); }
  .source > .row { align-items:end; }
  .mapping { display:grid; gap:8px; }
  .mapping .row button { width:32px; padding:0; }
  .check { display:flex; gap:8px; align-items:center; color:var(--planeai-text-muted); font-size:12px; }
  .add { justify-self:start; }
  ul.error { padding-left:18px; }
`;

const nextName = (taken: Record<string, unknown>, base: string, separator: string) => {
  let name = base;
  for (let i = 2; name in taken; i++) name = `${base}${separator}${i}`;
  return name;
};

let fieldIds = 0;
function field(label: string, control: HTMLElement): HTMLElement {
  const wrap = document.createElement("div");
  wrap.className = "field grow";
  const heading = document.createElement("label");
  control.id ||= `linear-field-${++fieldIds}`;
  heading.htmlFor = control.id;
  heading.textContent = label;
  wrap.append(heading, control);
  return wrap;
}

function textInput(
  value: string,
  changed?: (value: string) => void,
  options: { placeholder?: string; ariaLabel?: string; type?: string } = {},
) {
  const input = document.createElement("input");
  input.type = options.type ?? "text";
  input.value = value;
  input.placeholder = options.placeholder ?? "";
  if (options.ariaLabel) input.setAttribute("aria-label", options.ariaLabel);
  if (changed) input.addEventListener("change", () => changed(input.value));
  return input;
}

function button(label: string, onClick: () => void, className = "") {
  const element = document.createElement("button");
  element.type = "button";
  element.textContent = label;
  element.className = className;
  element.onclick = onClick;
  return element;
}

/** The Linear team key of an identifier, such as `TRA` for `TRA-13`. */
const teamOf = (identifier: string) => identifier.slice(0, identifier.lastIndexOf("-"));

/** A free PlaneAI prefix to offer for a team whose keys a PlaneAI project already uses. */
function suggestedPrefix(team: string, prefixes: Record<string, string>): string {
  const taken = new Set([...Object.keys(prefixes), ...Object.values(prefixes)]);
  return [`L${team}`, `${team}L`, `L${team}2`].find((prefix) => !taken.has(prefix)) ?? "";
}

const normalizedWriteback = (writeback: Writeback): Writeback | null =>
  writeback.on_start || writeback.on_complete || writeback.comment ? writeback : null;

/** Linear preferences, also mounted as the dashboard: connection, Sync Now, and sources. */
const entrypoint: Entrypoint = {
  mount(root, context) {
    const { host } = context;
    const api = sidecar(host);
    const style = document.createElement("style");
    style.textContent = styles;
    const page = document.createElement("main");
    page.className = "page";
    root.replaceChildren(style, page);

    let settings: Settings = { sync_interval_ms: 60000, sources: {} };
    let status: Status | null = null;
    let lastError: string | null = null;
    let result: SyncTotals | null = null;
    let syncing = false;
    let connecting = false;
    let attempt: string | null = null;
    let apiKey = "";
    let disposed = false;
    /** Query kinds picked in the editor but not saved yet, and local query errors, by source. */
    const kindDrafts: Record<string, "view" | "filter"> = {};
    const queryErrors: Record<string, string> = {};
    let generation = 0;
    let chain: Promise<boolean> = Promise.resolve(true);

    const save = (next: Settings) => {
      settings = next;
      const current = ++generation;
      render();
      const operation = chain.then(async () => {
        try {
          const saved = await api.updateSettings(next);
          if (current === generation) settings = saved;
          lastError = null;
          await host.data.changed();
          return true;
        } catch (error) {
          lastError = errorMessage(error);
          return false;
        } finally {
          if (!disposed) render();
        }
      });
      chain = operation;
      return operation;
    };
    const saveSource = (name: string, source: Source) =>
      save({ ...settings, sources: { ...settings.sources, [name]: source } });

    const refresh = async () => {
      const [nextSettings, nextStatus] = await Promise.all([api.settings(), api.status()]);
      settings = nextSettings;
      status = nextStatus;
      lastError = nextStatus.last_error;
      if (!disposed) render();
    };
    const run = async (work: () => Promise<unknown>) => {
      try {
        await work();
      } catch (error) {
        lastError = errorMessage(error);
      }
      if (!disposed) render();
    };

    const connectApiKey = () =>
      run(async () => {
        connecting = true;
        render();
        try {
          await api.connectApiKey(apiKey);
          apiKey = "";
          await host.data.changed();
          await refresh();
        } finally {
          connecting = false;
        }
      });

    const connectOAuth = async () => {
      if (connecting || attempt) return;
      const id = crypto.randomUUID();
      connecting = true;
      render();
      try {
        attempt = id;
        const started = await api.connectStart(id);
        await api.openBrowser(started.authorization_url);
        await api.connectComplete(id);
        const poll = async () => {
          if (disposed) return;
          status = await api.status();
          lastError = status.last_error;
          if (!status.authorizing && attempt === id) {
            attempt = null;
            if (status.connected) await host.data.changed();
          }
          render();
          if (status.authorizing) setTimeout(() => void poll(), 500);
        };
        await poll();
      } catch (error) {
        try {
          await api.connectCancel(id);
        } catch {
          // Keep the error that stopped the attempt; cancelling only releases the callback port.
        }
        if (attempt === id) attempt = null;
        lastError = errorMessage(error);
      } finally {
        connecting = false;
        if (!disposed) render();
      }
    };

    const sync = () =>
      run(async () => {
        syncing = true;
        render();
        try {
          result = await api.syncNow();
          await host.data.changed();
        } catch (error) {
          result = {
            created: 0,
            updated: 0,
            departed: 0,
            errors: 1,
            error_messages: [errorMessage(error)],
          };
        } finally {
          syncing = false;
        }
      });

    /** Offers to sync a team whose keys a PlaneAI project already uses under another prefix. */
    const prefixOffer = (team: string, identifiers: string[]) => {
      const prefixes = settings.key_prefixes ?? {};
      const offer = document.createElement("div");
      offer.className = "prefix-offer";
      const message = document.createElement("p");
      message.className = "warning";
      const mapped = prefixes[team];
      const keys = identifiers.map((identifier) =>
        mapped ? `${identifier} as ${mapped}${identifier.slice(team.length)}` : identifier,
      );
      message.textContent = `Not synced, because PlaneAI tasks already use their keys: ${keys.join(", ")}.`;
      const input = textInput(suggestedPrefix(team, prefixes), undefined, {
        ariaLabel: `PlaneAI key prefix for ${team} issues`,
      });
      input.addEventListener("input", () => {
        input.value = input.value.toUpperCase();
      });
      const apply = button(
        `Sync ${team} issues under this prefix`,
        () => {
          const prefix = input.value.trim();
          void (async () => {
            if (await save({ ...settings, key_prefixes: { ...prefixes, [team]: prefix } }))
              await sync();
          })();
        },
        "primary",
      );
      const row = document.createElement("div");
      row.className = "row";
      const prefixField = field(`Key prefix for ${team}`, input);
      prefixField.classList.remove("grow");
      row.append(prefixField, apply);
      offer.append(message, row);
      return offer;
    };

    const connectionCard = () => {
      const card = document.createElement("section");
      card.className = "card";
      const heading = document.createElement("h2");
      heading.textContent = "Linear connection";
      const summary = document.createElement("p");
      summary.className = "muted";
      card.append(heading, summary);
      const actions = document.createElement("div");
      actions.className = "row";
      if (status?.connected && status.organization) {
        const how = status.auth_kind === "oauth" ? "OAuth" : "an API key";
        summary.textContent = `Connected to ${status.organization.name} (${status.organization.url_key}) with ${how}`;
        const syncButton = button(syncing ? "Syncing…" : "Sync Now", () => void sync(), "primary");
        syncButton.disabled = syncing;
        actions.append(
          button(
            "Disconnect",
            () =>
              void run(async () => {
                await api.disconnect();
                result = null;
                await host.data.changed();
                await refresh();
              }),
          ),
          syncButton,
        );
        card.append(actions);
      } else {
        summary.textContent = status?.authorizing
          ? "Waiting for Linear in your browser…"
          : "Not connected";
        const key = textInput(apiKey, (value) => (apiKey = value), {
          type: "password",
          placeholder: "lin_api_…",
          ariaLabel: "Linear API key",
        });
        key.addEventListener("input", () => {
          apiKey = key.value;
          connectKey.disabled = connecting || !apiKey.trim();
        });
        const connectKey = button("Connect", () => void connectApiKey(), "primary");
        connectKey.disabled = connecting || !apiKey.trim();
        const keyRow = document.createElement("div");
        keyRow.className = "row";
        keyRow.append(field("Personal API key", key), connectKey);
        const hint = document.createElement("p");
        hint.className = "muted";
        hint.textContent =
          "Create one in Linear under Settings, Security & access, Personal API keys.";
        card.append(keyRow, hint);
        if (status?.oauth_available) {
          const oauth = button(
            status.authorizing ? "Authorizing…" : "Connect with Linear",
            () => void connectOAuth(),
          );
          oauth.disabled = connecting || status.authorizing;
          actions.append(oauth);
          if (status.authorizing) {
            actions.append(
              button(
                "Cancel authorization",
                () =>
                  void run(async () => {
                    await api.connectCancel(attempt);
                    attempt = null;
                    await refresh();
                  }),
              ),
            );
          }
          card.append(actions);
        }
      }
      if (result) {
        const totals = document.createElement("p");
        totals.className = result.errors ? "warning" : "muted";
        totals.textContent = `${result.created} created · ${result.updated} updated · ${result.departed} departed · ${result.errors} error${result.errors === 1 ? "" : "s"}`;
        card.append(totals);
        if (result.error_messages?.length) {
          const list = document.createElement("ul");
          list.className = "error";
          for (const message of result.error_messages) {
            const item = document.createElement("li");
            item.textContent = message;
            list.append(item);
          }
          card.append(list);
        }
        const skippedByTeam = new Map<string, string[]>();
        for (const identifier of result.skipped ?? []) {
          const team = teamOf(identifier);
          skippedByTeam.set(team, [...(skippedByTeam.get(team) ?? []), identifier]);
        }
        for (const [team, identifiers] of skippedByTeam)
          card.append(prefixOffer(team, identifiers));
      }
      const writeback = status?.last_writeback;
      if (writeback) {
        const line = document.createElement("p");
        line.className = writeback.ok ? "muted" : "warning";
        line.textContent = `Last writeback: ${writeback.identifier} ${writeback.action} ${writeback.ok ? "succeeded" : `failed: ${writeback.error}`}`;
        card.append(line);
      }
      if (lastError) {
        const error = document.createElement("p");
        error.className = "error";
        error.setAttribute("role", "alert");
        error.textContent = lastError;
        card.append(error);
      }
      return card;
    };

    const queryEditor = (name: string, source: Source) => {
      const kind = kindDrafts[name] ?? source.query.kind;
      const wrap = document.createElement("div");
      wrap.className = "field";
      const select = document.createElement("select");
      select.setAttribute("aria-label", `Query kind for ${name}`);
      select.append(
        new Option("Custom view", "view", false, kind === "view"),
        new Option("Issue filter (JSON)", "filter", false, kind === "filter"),
      );
      select.onchange = () => {
        const next = select.value as "view" | "filter";
        if (next === source.query.kind) delete kindDrafts[name];
        else if (next === "filter") {
          delete kindDrafts[name];
          void saveSource(name, { ...source, query: { kind: "filter", filter: DEFAULT_FILTER } });
          return;
        } else kindDrafts[name] = next;
        delete queryErrors[name];
        render();
      };
      wrap.append(field("Query", select));
      if (kind === "view") {
        const view = source.query.kind === "view" ? source.query.view : "";
        wrap.append(
          field(
            "Custom view URL",
            textInput(
              view,
              (value) => {
                if (!value.trim()) return;
                delete kindDrafts[name];
                void saveSource(name, { ...source, query: { kind: "view", view: value.trim() } });
              },
              { placeholder: "https://linear.app/acme/view/my-issues-0a1b2c3d4e5f" },
            ),
          ),
        );
      } else {
        const filter = document.createElement("textarea");
        filter.spellcheck = false;
        filter.value =
          source.query.kind === "filter" ? JSON.stringify(source.query.filter, null, 2) : "";
        filter.addEventListener("change", () => {
          let parsed: unknown;
          try {
            parsed = JSON.parse(filter.value);
          } catch (error) {
            queryErrors[name] = `IssueFilter is not valid JSON: ${errorMessage(error)}`;
            render();
            return;
          }
          if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
            queryErrors[name] = "IssueFilter must be a JSON object";
            render();
            return;
          }
          delete queryErrors[name];
          void saveSource(name, {
            ...source,
            query: { kind: "filter", filter: parsed as Record<string, unknown> },
          });
        });
        wrap.append(field("IssueFilter JSON", filter));
      }
      if (queryErrors[name]) {
        const error = document.createElement("p");
        error.className = "error";
        error.setAttribute("role", "alert");
        error.textContent = queryErrors[name];
        wrap.append(error);
      }
      return wrap;
    };

    const statusMapEditor = (name: string, source: Source) => {
      const map = source.status_map;
      const wrap = document.createElement("div");
      wrap.className = "mapping";
      const label = document.createElement("span");
      label.className = "label";
      label.textContent = "Status mapping (Linear state → PlaneAI status)";
      wrap.append(label);
      const saveMap = (status_map: Record<string, TaskStatus>) =>
        void saveSource(name, { ...source, status_map });
      for (const [state, mapped] of Object.entries(map)) {
        const row = document.createElement("div");
        row.className = "row";
        const stateInput = textInput(
          state,
          (value) => {
            const next = { ...map };
            delete next[state];
            if (value.trim()) next[value.trim()] = mapped;
            saveMap(next);
          },
          { ariaLabel: "Linear state" },
        );
        stateInput.classList.add("grow");
        const select = document.createElement("select");
        select.setAttribute("aria-label", `PlaneAI status for ${state}`);
        for (const value of TASK_STATUSES)
          select.append(new Option(statusLabel(value), value, false, value === mapped));
        select.onchange = () => saveMap({ ...map, [state]: select.value as TaskStatus });
        const remove = button("×", () => {
          const next = { ...map };
          delete next[state];
          saveMap(next);
        });
        remove.setAttribute("aria-label", `Remove status mapping for ${state}`);
        row.append(stateInput, select, remove);
        wrap.append(row);
      }
      wrap.append(
        button(
          "Add status mapping",
          () => saveMap({ ...map, [nextName(map, "Linear state", " ")]: "todo" }),
          "add",
        ),
      );
      return wrap;
    };

    const writebackEditor = (name: string, source: Source) => {
      const current: Writeback = source.writeback ?? {
        on_start: null,
        on_complete: null,
        comment: false,
      };
      const update = (patch: Partial<Writeback>) =>
        void saveSource(name, {
          ...source,
          writeback: normalizedWriteback({ ...current, ...patch }),
        });
      const wrap = document.createElement("div");
      wrap.className = "mapping";
      const label = document.createElement("span");
      label.className = "label";
      label.textContent = "Write back to Linear";
      const row = document.createElement("div");
      row.className = "row";
      row.append(
        field(
          "When work starts, move to",
          textInput(current.on_start ?? "", (value) => update({ on_start: value.trim() || null }), {
            placeholder: "In Progress",
          }),
        ),
        field(
          "When the task is done, move to",
          textInput(
            current.on_complete ?? "",
            (value) => update({ on_complete: value.trim() || null }),
            { placeholder: "Done" },
          ),
        ),
      );
      const check = document.createElement("label");
      check.className = "check";
      const comment = document.createElement("input");
      comment.type = "checkbox";
      comment.checked = current.comment;
      comment.onchange = () => update({ comment: comment.checked });
      check.append(comment, "Comment on the issue when PlaneAI moves it");
      wrap.append(label, row, check);
      return wrap;
    };

    const configurationCard = () => {
      const card = document.createElement("section");
      card.className = "card";
      const heading = document.createElement("h2");
      heading.textContent = "Configuration";
      card.append(heading);
      const interval = textInput(
        String(Math.round(settings.sync_interval_ms / 1000)),
        (value) => {
          const seconds = Number(value);
          if (Number.isInteger(seconds) && seconds >= 10)
            void save({ ...settings, sync_interval_ms: seconds * 1000 });
          else {
            lastError = "Sync interval must be a whole number of seconds, at least 10.";
            render();
          }
        },
        { type: "number" },
      );
      interval.min = "10";
      card.append(field("Sync interval (seconds)", interval));
      const sources = settings.sources;
      for (const [name, source] of Object.entries(sources)) {
        const article = document.createElement("article");
        article.className = "source";
        const head = document.createElement("div");
        head.className = "row";
        head.append(
          field(
            "Source name",
            textInput(name, (value) => {
              const renamed = value.trim();
              if (!renamed || renamed === name || renamed in sources) return;
              const next = Object.fromEntries(
                Object.entries(sources).map(([key, body]) => [key === name ? renamed : key, body]),
              );
              void save({ ...settings, sources: next });
            }),
          ),
          button(
            "Remove",
            () => {
              const next = { ...sources };
              delete next[name];
              void save({ ...settings, sources: next });
            },
            "danger",
          ),
        );
        article.append(
          head,
          queryEditor(name, source),
          statusMapEditor(name, source),
          writebackEditor(name, source),
        );
        card.append(article);
      }
      card.append(
        button(
          "Add source",
          () =>
            void save({
              ...settings,
              sources: {
                ...sources,
                [nextName(sources, "source", "-")]: {
                  query: { kind: "filter", filter: DEFAULT_FILTER },
                  status_map: {},
                  writeback: null,
                },
              },
            }),
          "add",
        ),
      );
      const prefixes = Object.entries(settings.key_prefixes ?? {}).sort(([a], [b]) =>
        a.localeCompare(b),
      );
      if (prefixes.length) {
        const section = document.createElement("article");
        section.className = "key-prefixes";
        const title = document.createElement("h3");
        title.textContent = "Key prefixes";
        const note = document.createElement("p");
        note.className = "muted";
        note.textContent =
          "Issues of these teams sync under another prefix, because a PlaneAI project uses their team key. Tasks already synced keep their keys.";
        section.append(title, note);
        for (const [team, prefix] of prefixes) {
          const row = document.createElement("div");
          row.className = "row";
          const label = document.createElement("span");
          label.className = "grow";
          label.textContent = `${team}-N syncs as ${prefix}-N`;
          row.append(
            label,
            button(
              "Remove",
              () => {
                const next = { ...settings.key_prefixes };
                delete next[team];
                void save({ ...settings, key_prefixes: next });
              },
              "danger",
            ),
          );
          section.append(row);
        }
        card.append(section);
      }
      return card;
    };

    const render = () => {
      if (disposed) return;
      page.replaceChildren(connectionCard(), configurationCard());
    };

    render();
    void refresh().catch((error) => {
      lastError = errorMessage(error);
      render();
    });
    return () => {
      disposed = true;
      if (attempt) void api.connectCancel(attempt).catch(() => {});
      root.replaceChildren();
    };
  },
};

export default entrypoint;
