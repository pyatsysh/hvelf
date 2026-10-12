// hvelf board UI: plain JS, no bundler.
const { invoke } = window.__TAURI__.core;

const filterEl = document.getElementById("filter");
const groupsEl = document.getElementById("groups");
const keepbarEl = document.getElementById("keepbar");
const noticeEl = document.getElementById("notice");
const hintTextEl = document.getElementById("hinttext");
const hintEl = document.getElementById("hint");

const HINT_BOARD = "1–9 launch · Enter first match · Ctrl+Q quit";
// What Esc does comes from the board: it parks a capture unrouted, keeps it in
// the fallback vault, or, once keep has refused it, drops it.
const hintKeep = (esc) => `Enter or the chord keeps into the lit vault · 1–9 pick another · Esc ${esc || "parks it unrouted"}`;

// Failures come back as {ok:false, code, message, feature}; show the reason,
// never a silent no-op. Text only: names and paths are data, not markup.
function showFailure(err) {
  const msg = err && err.message ? err.message : String(err);
  noticeEl.textContent = msg;
  noticeEl.hidden = false;
}
function clearNotice() {
  noticeEl.textContent = "";
  noticeEl.hidden = true;
}
if (window.__TAURI__.event) {
  window.__TAURI__.event.listen("hvelf-notice", (e) => showFailure(e.payload));
  // The keep chord may land while the board is already up.
  window.__TAURI__.event.listen("keep", () => {
    filterEl.value = "";
    refresh();
  });
}

// Platform limits (Linux: Wayland, no window manager, missing handler) are
// listed under the board so a disabled action says why. They are read again
// each time the board is summoned, since the chord grab is recorded after the
// page has loaded. While some chord summons the board, through hvelf's own
// grab or a GNOME shortcut, the routes not in use are no limit: no
// GlobalShortcuts portal, no chord of hvelf's own, no GNOME shortcut. A chord
// that failed, or a GNOME shortcut that cannot run, always is.
function showLimits() {
  invoke("capabilities").then((caps) => {
    const works = (name) => caps.some((c) => c.capability === name
      && (c.state === "available" || c.state === "degraded"));
    const chords = works("global-hotkey-x11") || works("global-hotkey-desktop");
    const unused = (c) => c.capability === "global-hotkey-portal"
      || (c.state === "disabled"
        && (c.capability === "global-hotkey-x11" || c.capability === "global-hotkey-desktop"));
    const limits = caps.filter((c) => c.state !== "available" && c.capability !== "session"
      && !(chords && unused(c)));
    hintEl.querySelector(".limits")?.remove();
    if (limits.length === 0) return;
    const span = document.createElement("span");
    span.className = "limits";
    span.textContent = ` · limits: ${limits.map((c) => c.capability).join(", ")}`;
    span.title = limits.map((c) => `${c.capability} (${c.state}): ${c.reason}`).join("\n");
    hintEl.appendChild(span);
  }, () => {});
}
showLimits();

let tiles = [];   // full list from backend
let visible = []; // filtered, in render order (for number keys / Enter)
let pending = null; // a keep capture waiting for a vault, or null

async function refresh() {
  try {
    pending = await invoke("keep_pending");
    if (pending) {
      // In keep mode the tiles are the vaults that take keeps, by id, with
      // the preselected one first and lit.
      tiles = pending.vaults.map((v) => ({
        id: v.id, name: v.name, qualifier: v.qualifier, bounded: v.bounded,
        path: "", open: false, pick: v.id === pending.vault.id, group: "keep into",
      }));
    } else {
      tiles = await invoke("list_vaults");
    }
  } catch (e) {
    groupsEl.replaceChildren();
    const p = document.createElement("p");
    p.className = "err";
    p.textContent = e && e.message ? e.message : String(e);
    groupsEl.appendChild(p);
    return;
  }
  render();
}

// Built as text nodes: a folder name may hold <, > or quotes.
function el(cls, text) {
  const s = document.createElement("span");
  s.className = cls;
  if (text !== undefined) s.textContent = text;
  return s;
}

// The keep banner: what was captured, from where, and where it is going.
function renderKeepbar() {
  if (!pending) {
    keepbarEl.replaceChildren();
    keepbarEl.style.display = "none";
    hintTextEl.textContent = HINT_BOARD;
    return;
  }
  const meta = el("kmeta", `${pending.chars} chars · ${pending.app} → `);
  const target = document.createElement("b");
  target.textContent = pending.vault.id;
  meta.appendChild(target);
  if (pending.vault.bounded) meta.appendChild(el("bound", " bounded"));
  keepbarEl.replaceChildren(el("kword", "keep"), el("kprev", pending.preview || "(whitespace)"), meta);
  keepbarEl.style.display = "flex";
  hintTextEl.textContent = hintKeep(pending.esc);
}

function render() {
  renderKeepbar();
  const q = filterEl.value.trim().toLowerCase();
  // The qualifier is filterable too: typing the parent folder is how you
  // pick between two vaults of the same name.
  visible = tiles.filter(
    (t) => !q || `${t.name} ${t.qualifier || ""}`.toLowerCase().includes(q),
  );

  const byGroup = new Map();
  for (const t of visible) {
    const g = t.group || "vaults";
    if (!byGroup.has(g)) byGroup.set(g, []);
    byGroup.get(g).push(t);
  }

  groupsEl.innerHTML = "";
  let idx = 0;
  for (const [gname, gtiles] of byGroup) {
    const section = document.createElement("section");
    const h = document.createElement("h2");
    h.textContent = gname;
    section.appendChild(h);
    const grid = document.createElement("div");
    grid.className = "grid";
    for (const t of gtiles) {
      idx += 1;
      const tile = document.createElement("button");
      const reported = t.open && t.open_state === "reported";
      const unknown = t.registered === false;
      tile.className = "tile" + (t.open ? " open" : "") + (reported ? " reported" : "") + (t.pick ? " pick" : "")
        + (t.missing ? " missing" : "") + (unknown ? " unregistered" : "") + (t.bounded ? " bounded" : "");
      tile.title = t.missing
        ? `${t.path}\nnot on this machine: Obsidian's registry still lists it here`
        : unknown
          ? `${t.path}\nnot a vault Obsidian knows yet: open the folder once with Obsidian's "Open folder as vault"`
          : reported
            ? `${t.path}\nObsidian reports this vault open; not observed live, may be stale`
            : t.path;
      const label = el("label");
      label.appendChild(el("name", t.name));
      if (t.qualifier) label.appendChild(el("qual", t.qualifier));
      // Bounded vaults (Admin, Career, Personal) are searched only when named.
      if (t.bounded) label.appendChild(el("bound", "bounded"));
      tile.append(el("badge", idx <= 9 ? String(idx) : ""), el("dot"), label);
      // One cross, two jobs: on an open vault it closes the window, on a
      // shut one it takes the tile off the board. Where this desktop gives
      // hvelf no window control, it only removes, and says so. A keep tile
      // is a folder under the exocortex root and has neither.
      const canClose = t.open && !t.close_reason;
      if (!pending) {
        const cross = el("close", "×");
        cross.title = canClose
          ? "close vault (frees its RAM)"
          : t.open
            ? `remove from the board (closing is unavailable here: ${t.close_reason})`
            : "remove from the board";
        tile.appendChild(cross);
      }
      tile.addEventListener("click", (e) => {
        if (e.target.classList.contains("close")) {
          e.stopPropagation();
          if (canClose) {
            invoke("close_vault", { id: t.id }).catch(showFailure);
            setTimeout(refresh, 700); // give the window a moment to die
          } else if (tile.classList.contains("armed")) {
            invoke("hide_vault", { id: t.id }).then(refresh, showFailure);
          } else {
            // Removal asks twice. The grid closes up after a tile goes, so
            // a stray second click would land on the next vault's cross.
            tile.classList.add("armed");
            tile.querySelector(".name").textContent = "remove from board?";
          }
          return;
        }
        if (tile.classList.contains("armed")) {
          render(); // a click anywhere else on the tile is a no
          return;
        }
        pick(t);
      });
      tile.addEventListener("mouseleave", () => {
        if (tile.classList.contains("armed")) render();
      });
      grid.appendChild(tile);
    }
    section.appendChild(grid);
    groupsEl.appendChild(section);
  }
  if (visible.length === 0) {
    groupsEl.innerHTML = `<p class="err">no vaults match</p>`;
  }
}

// A tile: launch the vault, or in keep mode file the capture into it. A
// board tile travels by Obsidian's vault id, since two vaults can share a
// name and a label may carry a qualifier no vault answers to; a keep tile is
// a folder under the exocortex root and travels by name.
function pick(t) {
  clearNotice();
  filterEl.value = "";
  if (pending) {
    pending = null;
    invoke("keep_file", { vault: t.id }).catch(showFailure);
  } else {
    invoke("launch", { id: t.id }).catch(showFailure);
  }
}

// Esc in keep mode is "decide later", never "discard", until keep has refused
// the capture: then nothing may be written around the refusal, so it drops it.
async function dismiss() {
  filterEl.value = "";
  clearNotice();
  if (pending) {
    pending = null;
    await invoke("keep_file", { vault: null }).catch(showFailure);
  } else {
    invoke("hide_window");
  }
}

// A key that acts is marked handled. WebKitGTK gives a key the page leaves
// unhandled back to GTK, and marks the next key press for GTK, meaning that
// same key; when the key has hidden the board by then, GTK drops it, the mark
// stays, and the first key of the next summon never reaches the page, so Esc
// took two presses (DELIVERY-14).
document.addEventListener("keydown", (e) => {
  if (e.key === "Escape") {
    e.preventDefault();
    dismiss();
    return;
  }
  if (e.ctrlKey && (e.key === "q" || e.key === "Q")) {
    e.preventDefault();
    dismiss().then(() => invoke("quit"));
    return;
  }
  if (e.key === "Enter" && visible.length > 0) {
    e.preventDefault();
    pick(visible[0]);
    return;
  }
  // Digits are quick picks while the filter is empty; once the user has
  // started typing they belong to the filter (vault names contain digits).
  if (/^[1-9]$/.test(e.key) && filterEl.value === "") {
    e.preventDefault();
    const i = parseInt(e.key, 10) - 1;
    if (visible[i]) pick(visible[i]);
    return;
  }
  // Anything printable focuses the filter so you can just start typing.
  if (e.key.length === 1 && document.activeElement !== filterEl) {
    filterEl.focus();
  }
});

filterEl.addEventListener("input", render);

// Re-read state every time the board is summoned (window regains focus).
window.addEventListener("focus", () => {
  filterEl.value = "";
  filterEl.focus();
  refresh();
  showLimits();
});

refresh();
filterEl.focus();
