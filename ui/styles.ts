/** Shared styles on PlaneAI's semantic theme tokens. */
export const baseStyles = `
  body { margin:0; color:var(--planeai-text); font-family:var(--planeai-font-sans); font-size:13px; }
  button { cursor:pointer; font:inherit; min-height:30px; padding:4px 12px; border:1px solid var(--planeai-border); border-radius:var(--planeai-radius); background:transparent; color:var(--planeai-text-muted); }
  button:hover:not(:disabled) { background:var(--planeai-surface-raised); color:var(--planeai-text); }
  button:disabled { opacity:.55; cursor:default; }
  button.primary { border-color:var(--planeai-accent); background:var(--planeai-accent); color:var(--planeai-on-accent); }
  button.danger { border-color:transparent; color:var(--planeai-danger); }
  input, select, textarea { box-sizing:border-box; width:100%; min-height:32px; padding:6px 10px; border:1px solid var(--planeai-border); border-radius:var(--planeai-radius); background:var(--planeai-surface); color:var(--planeai-text); font:inherit; }
  textarea { font-family:var(--planeai-font-mono); font-size:12px; min-height:84px; resize:vertical; }
  input:focus, select:focus, textarea:focus { outline:none; border-color:var(--planeai-accent); }
  input[type="checkbox"] { width:auto; min-height:0; }
  .muted { color:var(--planeai-text-muted); font-size:12px; }
  .error { color:var(--planeai-danger); font-size:12px; margin:0; }
  .warning { color:var(--planeai-warning); font-size:12px; margin:0; }
  .row { display:flex; gap:8px; align-items:center; }
  .row > .grow { flex:1; }
  .key { color:var(--planeai-text-subtle); font-family:var(--planeai-font-mono); font-size:11px; flex:0 0 auto; }
`;
