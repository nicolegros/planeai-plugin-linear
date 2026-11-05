export type ComboboxItem = { value: string; label: string };

type ComboboxOptions = {
  ariaLabel: string;
  items: ComboboxItem[];
  value: string;
  disabled?: boolean;
  placeholder: string;
  emptyText: string;
  onValueChange(value: string): void;
};

export const comboboxStyles = `.combobox { position:relative; } .combobox-input { width:100%; } .combobox-list { margin-top:4px; max-height:192px; overflow-y:auto; border:1px solid var(--planeai-border); border-radius:var(--planeai-radius); background:var(--planeai-surface-raised); } .combobox-option { display:flex; width:100%; border:0; border-radius:0; background:transparent; color:var(--planeai-text-muted); padding:8px 12px; text-align:left; font:inherit; } .combobox-option:hover,.combobox-option[data-highlighted="true"] { background:var(--planeai-accent-subtle); color:var(--planeai-text); } .combobox-empty { display:block; padding:8px 12px; color:var(--planeai-text-subtle); }`;

/** A searchable listbox: type to filter, arrows to move, Enter to choose. */
export function createCombobox(options: ComboboxOptions): HTMLElement {
  const wrap = document.createElement("div");
  wrap.className = "combobox";
  const input = document.createElement("input");
  input.className = "combobox-input";
  input.type = "text";
  input.dataset.projectPicker = "";
  input.setAttribute("aria-label", options.ariaLabel);
  input.setAttribute("role", "combobox");
  input.setAttribute("aria-autocomplete", "list");
  input.autocomplete = "off";
  input.spellcheck = false;
  input.disabled = options.disabled ?? false;
  const list = document.createElement("div");
  list.className = "combobox-list";
  list.setAttribute("role", "listbox");
  list.hidden = true;
  list.id = `combobox-${crypto.randomUUID()}`;
  input.setAttribute("aria-controls", list.id);

  let open = false;
  let query = "";
  let highlighted = 0;
  const selectedLabel = () => options.items.find((item) => item.value === options.value)?.label ?? "";
  const matches = () => {
    const normalized = query.trim().toLowerCase();
    return normalized
      ? options.items.filter((item) => item.label.toLowerCase().includes(normalized))
      : options.items;
  };
  const renderOptions = () => {
    const found = matches();
    highlighted = Math.min(highlighted, Math.max(found.length - 1, 0));
    list.replaceChildren();
    if (found.length === 0) {
      input.removeAttribute("aria-activedescendant");
      const empty = document.createElement("span");
      empty.className = "combobox-empty";
      empty.textContent = options.emptyText;
      list.append(empty);
      return;
    }
    found.forEach((item, index) => {
      const option = document.createElement("button");
      option.type = "button";
      option.tabIndex = -1;
      option.id = `${list.id}-option-${index}`;
      option.className = "combobox-option";
      option.setAttribute("role", "option");
      option.setAttribute("aria-selected", String(item.value === options.value));
      option.dataset.highlighted = String(index === highlighted);
      option.textContent = item.label;
      option.addEventListener("mousedown", (event) => event.preventDefault());
      option.addEventListener("click", () => choose(item.value));
      list.append(option);
      if (index === highlighted) input.setAttribute("aria-activedescendant", option.id);
    });
  };
  const setOpen = (next: boolean) => {
    open = next && !input.disabled;
    list.hidden = !open;
    input.setAttribute("aria-expanded", String(open));
    if (open) renderOptions();
    else input.removeAttribute("aria-activedescendant");
  };
  const choose = (value: string) => {
    query = "";
    setOpen(false);
    input.value = options.items.find((item) => item.value === value)?.label ?? "";
    options.onValueChange(value);
  };

  input.value = selectedLabel();
  input.placeholder = options.placeholder;
  input.setAttribute("aria-expanded", "false");
  input.addEventListener("focus", () => {
    query = "";
    highlighted = 0;
    input.value = "";
    setOpen(true);
  });
  input.addEventListener("input", () => {
    query = input.value;
    highlighted = 0;
    setOpen(true);
  });
  input.addEventListener("blur", () => {
    queueMicrotask(() => {
      if (wrap.contains(document.activeElement)) return;
      query = "";
      setOpen(false);
      input.value = selectedLabel();
    });
  });
  input.addEventListener("keydown", (event) => {
    const found = matches();
    if (event.key === "Escape" && open) {
      event.preventDefault();
      event.stopPropagation();
      query = "";
      setOpen(false);
      input.value = selectedLabel();
    } else if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      highlighted =
        event.key === "ArrowDown"
          ? Math.min(highlighted + 1, Math.max(found.length - 1, 0))
          : Math.max(highlighted - 1, 0);
      setOpen(true);
      list.querySelector<HTMLElement>("[data-highlighted='true']")?.scrollIntoView?.({ block: "nearest" });
    } else if (event.key === "Enter" && open && found[highlighted]) {
      event.preventDefault();
      event.stopPropagation();
      choose(found[highlighted].value);
    }
  });
  wrap.append(input, list);
  return wrap;
}
