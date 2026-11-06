export type FormMode = "normal" | "insert";

type Binding = { key: string; run(): void };

const isEditable = (target: EventTarget | null) =>
  target instanceof HTMLElement &&
  target.matches("input, textarea, select, [contenteditable='true']");

/**
 * PlaneAI's form keyboard model: in normal mode a letter acts (focus a field, run an action)
 * and Escape closes; typing in a field is insert mode, and Escape returns to normal mode.
 */
export function attachFormKeyboard(options: {
  wrapper: HTMLElement;
  bindings: Binding[];
  onDismiss(): void;
  onModeChange(mode: FormMode): void;
}): { mode(): FormMode } {
  let mode: FormMode = "normal";
  const setMode = (next: FormMode) => {
    if (mode === next) return;
    mode = next;
    options.onModeChange(mode);
  };
  options.wrapper.addEventListener("focusin", (event) =>
    setMode(isEditable(event.target) ? "insert" : "normal"),
  );
  options.wrapper.addEventListener("keydown", (event) => {
    if (event.metaKey || event.ctrlKey || event.altKey || event.key === "Tab") return;
    if (mode === "insert") {
      if (event.key !== "Escape") return;
      event.preventDefault();
      event.stopPropagation();
      (event.target as HTMLElement).blur?.();
      options.wrapper.focus();
      setMode("normal");
      return;
    }
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      options.onDismiss();
      return;
    }
    const binding = options.bindings.find((candidate) => candidate.key === event.key.toLowerCase());
    if (binding) {
      event.preventDefault();
      event.stopPropagation();
      binding.run();
      return;
    }
    // Enter and Space still activate the focused button.
    if (event.key !== "Enter" && event.key !== " ") {
      event.preventDefault();
      event.stopPropagation();
    }
  });
  return { mode: () => mode };
}
