// hvelf board UI: plain JS, no bundler.
const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const filterEl = document.getElementById("filter");
const groupsEl = document.getElementById("groups");
const keepbarEl = document.getElementById("keepbar");
const hintEl = document.getElementById("hint");

const HINT_BOARD = "1–9 launch · Enter first match · Ctrl+Q quit";
const HINT_KEEP = "Enter or the chord keeps into the lit vault · 1–9 pick another · Esc parks it unrouted";

let tiles = [];   // full list from backend
let visible = []; // filtered, in render order (for number keys / Enter)
let pending = null; // a keep capture waiting for a vault, or null

async function refresh() {
  try {
    pending = await invoke("keep_pending");
    if (pending) {
      // In keep mode the tiles are the vault folders that take keeps, with
      // the preselected one first and lit.
      tiles = pending.vaults.map((n) => ({
        name: n, path: "", open: false, pick: n === pending.vault, group: "keep into",
      }));
    } else {
      tiles = await invoke("list_vaults");
    }
  } catch (e) {
    groupsEl.innerHTML = `<p class="err">${e}</p>`;
    return;
  }
  render();
}

function render() {
  const q = filterEl.value.trim().toLowerCase();
  // The qualifier is filterable too: typing the parent folder is how you
  // pick between two vaults of the same name.
  visible = tiles.filter(
    (t) => !q || `${t.name} ${t.qualifier || ""}`.toLowerCase().includes(q),
  );

  if (pending) {
    keepbarEl.innerHTML =
      `<span class="kword">keep</span>` +
      `<span class="kprev">${esc(pending.preview) || "(whitespace)"}</span>` +
      `<span class="kmeta">${pending.chars} chars · ${esc(pending.app)} → <b>${esc(pending.vault)}</b></span>`;
    keepbarEl.style.display = "flex";
    hintEl.textContent = HINT_KEEP;
  } else {
    keepbarEl.style.display = "none";
    hintEl.textContent = HINT_BOARD;
  }

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
      tile.className = "tile" + (t.open ? " open" : "") + (t.pick ? " pick" : "");
      tile.title = t.path;
      tile.innerHTML =
        `<span class="badge">${idx <= 9 ? idx : ""}</span>` +
        `<span class="dot"></span>` +
        `<span class="label"><span class="name">${esc(t.name)}</span>` +
        (t.qualifier ? `<span class="qual">${esc(t.qualifier)}</span>` : "") +
        `</span>` +
        // One cross, two jobs: on an open vault it closes the window, on a
        // shut one it takes the tile off the board. A keep tile is a folder
        // under the exocortex root and has neither.
        (t.open
          ? `<span class="close" title="close vault (frees its RAM)">×</span>`
          : pending
            ? ""
            : `<span class="close" title="remove from the board">×</span>`);
      tile.addEventListener("click", (e) => {
        if (e.target.classList.contains("close")) {
          e.stopPropagation();
          if (t.open) {
            invoke("close_vault", { id: t.id });
            setTimeout(refresh, 700); // give the window a moment to die
          } else if (tile.classList.contains("armed")) {
            invoke("hide_vault", { id: t.id }).then(refresh, (err) => {
              groupsEl.innerHTML = `<p class="err">${err}</p>`;
            });
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

function esc(s) {
  return String(s).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]));
}

// A tile: launch the vault, or in keep mode file the capture into it. A
// board tile travels by Obsidian's vault id, since two vaults can share a
// name and a label may carry a qualifier no vault answers to; a keep tile is
// a folder under the exocortex root and travels by name.
function pick(t) {
  filterEl.value = "";
  if (pending) {
    pending = null;
    invoke("keep_file", { vault: t.name });
  } else {
    invoke("launch", { id: t.id });
  }
}

// Esc in keep mode is "decide later", never "discard".
async function dismiss() {
  filterEl.value = "";
  if (pending) {
    pending = null;
    await invoke("keep_file", { vault: null });
  } else {
    invoke("hide_window");
  }
}

document.addEventListener("keydown", (e) => {
  if (e.key === "Escape") {
    dismiss();
    return;
  }
  if (e.ctrlKey && (e.key === "q" || e.key === "Q")) {
    dismiss().then(() => invoke("quit"));
    return;
  }
  if (e.key === "Enter" && visible.length > 0) {
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
});

// The keep chord may land while the board is already up.
listen("keep", () => {
  filterEl.value = "";
  refresh();
});

refresh();
filterEl.focus();
