// hvelf board UI: plain JS, no bundler.
const { invoke } = window.__TAURI__.core;

const filterEl = document.getElementById("filter");
const groupsEl = document.getElementById("groups");

let tiles = [];   // full list from backend
let visible = []; // filtered, in render order (for number keys / Enter)

async function refresh() {
  try {
    tiles = await invoke("list_vaults");
  } catch (e) {
    groupsEl.innerHTML = `<p class="err">${e}</p>`;
    return;
  }
  render();
}

function render() {
  const q = filterEl.value.trim().toLowerCase();
  visible = tiles.filter((t) => !q || t.name.toLowerCase().includes(q));

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
      tile.className = "tile" + (t.open ? " open" : "");
      tile.title = t.path;
      tile.innerHTML =
        `<span class="badge">${idx <= 9 ? idx : ""}</span>` +
        `<span class="dot"></span>` +
        `<span class="name">${t.name}</span>` +
        (t.open ? `<span class="close" title="close vault (frees its RAM)">×</span>` : "");
      tile.addEventListener("click", (e) => {
        if (e.target.classList.contains("close")) {
          e.stopPropagation();
          invoke("close_vault", { name: t.name });
          setTimeout(refresh, 700); // give the window a moment to die
          return;
        }
        launch(t.name);
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

function launch(name) {
  invoke("launch", { name });
  filterEl.value = "";
}

document.addEventListener("keydown", (e) => {
  if (e.key === "Escape") {
    filterEl.value = "";
    invoke("hide_window");
    return;
  }
  if (e.ctrlKey && (e.key === "q" || e.key === "Q")) {
    invoke("quit");
    return;
  }
  if (e.key === "Enter" && visible.length > 0) {
    launch(visible[0].name);
    return;
  }
  if (/^[1-9]$/.test(e.key) && document.activeElement !== filterEl) {
    const i = parseInt(e.key, 10) - 1;
    if (visible[i]) launch(visible[i].name);
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
