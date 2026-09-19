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
      tile.className = "tile" + (t.open ? " open" : "");
      tile.title = t.path;
      tile.innerHTML =
        `<span class="badge">${idx <= 9 ? idx : ""}</span>` +
        `<span class="dot"></span>` +
        `<span class="label"><span class="name">${t.name}</span>` +
        (t.qualifier ? `<span class="qual">${t.qualifier}</span>` : "") +
        `</span>` +
        // One cross, two jobs: on an open vault it closes the window, on a
        // shut one it takes the tile off the board.
        (t.open
          ? `<span class="close" title="close vault (frees its RAM)">×</span>`
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
  invoke("launch", { id });
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
