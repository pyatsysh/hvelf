// hvelf board UI: plain JS, no bundler.
const { invoke } = window.__TAURI__.core;

const filterEl = document.getElementById("filter");
const groupsEl = document.getElementById("groups");
const noticeEl = document.getElementById("notice");
const hintEl = document.getElementById("hint");

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
}

// Platform limits (Linux: Wayland, no window manager, missing handler) are
// listed under the board so a disabled action says why.
invoke("capabilities").then((caps) => {
  const limits = caps.filter((c) => c.state !== "available" && c.capability !== "session");
  if (limits.length === 0) return;
  const span = document.createElement("span");
  span.className = "limits";
  span.textContent = ` · limits: ${limits.map((c) => c.capability).join(", ")}`;
  span.title = limits.map((c) => `${c.capability} (${c.state}): ${c.reason}`).join("\n");
  hintEl.appendChild(span);
}, () => {});

let tiles = [];   // full list from backend
let visible = []; // filtered, in render order (for number keys / Enter)

async function refresh() {
  try {
    tiles = await invoke("list_vaults");
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

function render() {
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
      tile.className = "tile" + (t.open ? " open" : "") + (reported ? " reported" : "");
      tile.title = reported
        ? `${t.path}\nObsidian reports this vault open; not observed live, may be stale`
        : t.path;
      // Built as text nodes: a folder name may hold <, > or quotes.
      const el = (cls, text) => {
        const s = document.createElement("span");
        s.className = cls;
        if (text !== undefined) s.textContent = text;
        return s;
      };
      const label = el("label");
      label.appendChild(el("name", t.name));
      if (t.qualifier) label.appendChild(el("qual", t.qualifier));
      // One cross, two jobs: on an open vault it closes the window, on a
      // shut one it takes the tile off the board. Where this desktop gives
      // hvelf no window control, it only removes, and says so.
      const canClose = t.open && !t.close_reason;
      const cross = el("close", "×");
      cross.title = canClose
        ? "close vault (frees its RAM)"
        : t.open
          ? `remove from the board (closing is unavailable here: ${t.close_reason})`
          : "remove from the board";
      tile.append(el("badge", idx <= 9 ? String(idx) : ""), el("dot"), label, cross);
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
        launch(t.id);
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

// Tiles travel by Obsidian's vault id: two vaults can share a name, and
// the label on a tile may carry a qualifier that no vault answers to.
function launch(id) {
  clearNotice();
  invoke("launch", { id }).catch(showFailure);
  filterEl.value = "";
}

document.addEventListener("keydown", (e) => {
  if (e.key === "Escape") {
    filterEl.value = "";
    clearNotice();
    invoke("hide_window");
    return;
  }
  if (e.ctrlKey && (e.key === "q" || e.key === "Q")) {
    invoke("quit");
    return;
  }
  if (e.key === "Enter" && visible.length > 0) {
    launch(visible[0].id);
    return;
  }
  // Digits are quick picks while the filter is empty; once the user has
  // started typing they belong to the filter (vault names contain digits).
  if (/^[1-9]$/.test(e.key) && filterEl.value === "") {
    e.preventDefault();
    const i = parseInt(e.key, 10) - 1;
    if (visible[i]) launch(visible[i].id);
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

refresh();
filterEl.focus();
