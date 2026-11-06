import { errorMessage, sidecar, type LinkedIssue } from "./api.js";
import { comboboxStyles, createCombobox } from "./combobox.js";
import { attachFormKeyboard, type FormMode } from "./form-keyboard.js";
import type { Entrypoint } from "./host.js";
import { baseStyles } from "./styles.js";

type Project = { id: string; name: string; path: string };

const styles = `${baseStyles}${comboboxStyles}
  .assignment { padding:4px var(--planeai-space-5) var(--planeai-space-5); outline:none; }
  .assignment > form { display:grid; gap:14px; }
  .preview { display:grid; gap:4px; border:1px solid var(--planeai-border); border-radius:8px; padding:10px; }
  .preview h3 { margin:0; font-size:14px; }
  .preview p { white-space:pre-wrap; margin:0; color:var(--planeai-text-muted); font-size:12px; max-height:180px; overflow:auto; }
  .field { display:grid; gap:4px; }
  .field label { color:var(--planeai-text-muted); font-size:12px; }
  .actions { display:flex; justify-content:space-between; align-items:center; gap:8px; }
  .keyboard-mode { display:flex; align-items:center; gap:8px; font-size:10px; color:var(--planeai-text-subtle); }
  .keyboard-mode strong { font-family:var(--planeai-font-mono); font-size:10px; padding:2px 6px; border-radius:4px; background:var(--planeai-surface-raised); color:var(--planeai-text-muted); }
  .keyboard-mode.insert strong { background:var(--planeai-accent-subtle); color:var(--planeai-accent); }
  .submit-hint { margin-left:4px; font-family:var(--planeai-font-mono); font-size:10px; opacity:.6; }
`;

const submitHint = () => (/Mac|iPhone|iPad/.test(navigator.platform) ? "⌘↵" : "Ctrl+↵");

/** The assign dialog: makes the selected Linear issue's task the parent of a new task in a project. */
const entrypoint: Entrypoint = {
  mount(root, context) {
    const { host } = context;
    const api = sidecar(host);
    const style = document.createElement("style");
    style.textContent = styles;
    const body = document.createElement("div");
    body.className = "assignment";
    body.tabIndex = -1;
    root.replaceChildren(style, body);
    // One operation per dialog: a retried submit never creates a second child.
    const operation = crypto.randomUUID();
    let issue: LinkedIssue | null = null;
    let projects: Project[] = [];
    let projectId = "";
    let error = "";
    let loading = true;
    let submitting = false;
    let disposed = false;
    let mode: FormMode = "normal";

    const picker = () => body.querySelector<HTMLInputElement>("[data-project-picker]");
    const project = () => projects.find((candidate) => candidate.id === projectId) ?? null;
    const loadProjects = async () => {
      projects = (await host.rpc.call<{ projects: Project[] }>("projects.list")).projects;
      if (!project()) projectId = "";
    };

    const renderMode = () => {
      const status = body.querySelector<HTMLElement>("[data-keyboard-mode]");
      if (!status) return;
      status.classList.toggle("insert", mode === "insert");
      status.innerHTML =
        mode === "insert"
          ? "<strong>INSERT</strong><span>esc → normal mode</span>"
          : "<strong>NORMAL</strong><span>press a key to focus field</span>";
    };

    const render = () => {
      const form = document.createElement("form");
      form.setAttribute("aria-label", "Assign Linear issue to PlaneAI project");
      form.addEventListener("submit", (event) => {
        event.preventDefault();
        void submit();
      });
      if (loading || !issue) {
        const message = document.createElement("p");
        message.className = loading ? "muted" : "error";
        if (!loading) message.setAttribute("role", "alert");
        message.textContent = loading
          ? "Loading Linear issue…"
          : error || "This Linear issue is no longer available.";
        form.append(message);
        body.replaceChildren(form);
        return;
      }
      const preview = document.createElement("section");
      preview.className = "preview";
      const key = document.createElement("span");
      key.className = "key";
      key.textContent = issue.identifier;
      const title = document.createElement("h3");
      title.textContent = issue.title;
      const description = document.createElement("p");
      description.textContent = issue.description || "No description";
      preview.append(key, title, description);
      form.append(preview);

      if (projects.length === 0) {
        const empty = document.createElement("p");
        empty.className = "muted";
        empty.textContent = "No PlaneAI projects available. Press N to create one.";
        form.append(empty);
      } else {
        const field = document.createElement("div");
        field.className = "field";
        const label = document.createElement("label");
        label.textContent = "PlaneAI project (P)";
        const combobox = createCombobox({
          ariaLabel: "PlaneAI project",
          items: projects.map(({ id, name }) => ({ value: id, label: name })),
          value: projectId,
          disabled: submitting,
          placeholder: "Search projects…",
          emptyText: "No projects found",
          onValueChange: (value) => {
            projectId = value;
            error = "";
            render();
          },
        });
        label.htmlFor = combobox.querySelector("input")!.id = "linear-project";
        field.append(label, combobox);
        form.append(field);
      }
      if (error) {
        const message = document.createElement("p");
        message.className = "error";
        message.setAttribute("role", "alert");
        message.textContent = error;
        form.append(message);
      }
      const actions = document.createElement("div");
      actions.className = "actions";
      const status = document.createElement("div");
      status.className = "keyboard-mode";
      status.dataset.keyboardMode = "";
      const buttons = document.createElement("div");
      buttons.className = "row";
      const create = document.createElement("button");
      create.type = "button";
      create.textContent = "New Project (N)";
      create.disabled = submitting || !host.interaction;
      create.onclick = () => void createProject();
      const assign = document.createElement("button");
      assign.type = "submit";
      assign.className = "primary";
      assign.disabled = submitting || !project();
      assign.title = submitHint();
      if (submitting) assign.textContent = "Assigning…";
      else {
        const hint = document.createElement("span");
        hint.className = "submit-hint";
        hint.textContent = submitHint();
        assign.append("Assign ", hint);
      }
      buttons.append(create, assign);
      actions.append(status, buttons);
      form.append(actions);
      body.replaceChildren(form);
      renderMode();
    };

    const createProject = async () => {
      if (submitting || !host.interaction) return;
      const created = await host.interaction.openProjectForm();
      if (!created || disposed) return;
      try {
        await loadProjects();
        projectId = projects.some((candidate) => candidate.id === created.id) ? created.id : "";
        error = "";
      } catch (nextError) {
        error = errorMessage(nextError);
      }
      render();
      picker()?.blur();
    };

    const submit = async () => {
      if (submitting || !issue) return;
      const chosen = project();
      if (!chosen) {
        error = "Choose a PlaneAI project.";
        render();
        return;
      }
      submitting = true;
      error = "";
      render();
      try {
        await host.rpc.call("tasks.createChild", {
          project_path: chosen.path,
          parent_key: issue.task_key,
          title: issue.title,
          description: issue.description,
          operation_id: `linear-assign:${issue.issue_id}:${operation}`,
        });
        await host.data.changed();
        host.navigation.close();
      } catch (nextError) {
        error = errorMessage(nextError);
        submitting = false;
        if (!disposed) render();
      }
    };

    attachFormKeyboard({
      wrapper: body,
      bindings: [
        { key: "p", run: () => picker()?.focus() },
        { key: "n", run: () => void createProject() },
      ],
      onDismiss: () => host.navigation.close(),
      onModeChange: (next) => {
        mode = next;
        renderMode();
      },
    });
    body.addEventListener(
      "keydown",
      (event) => {
        if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) {
          event.preventDefault();
          event.stopPropagation();
          void submit();
        }
      },
      true,
    );

    render();
    void Promise.all([api.currentIssue(), loadProjects()])
      .then(([current]) => {
        issue = current;
      })
      .catch((nextError) => {
        error = errorMessage(nextError);
      })
      .finally(() => {
        loading = false;
        if (disposed) return;
        render();
        body.focus();
      });
    return () => {
      disposed = true;
      root.replaceChildren();
    };
  },
};

export default entrypoint;
