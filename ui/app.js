// The window's half of arxburn. It asks the CLI for facts and shows what comes back; every
// decision about what may be written to lives in the CLI, which is the part with the tests.
const invoke = window.__TAURI__.core.invoke;
const listen = window.__TAURI__.event.listen;

const $ = (id) => document.getElementById(id);
let picked = { image: null, device: null, allowInternal: false };
let images = [];

function human(n) {
  const u = ["B", "KB", "MB", "GB", "TB"];
  let v = Number(n) || 0, i = 0;
  while (v >= 1024 && i < u.length - 1) { v /= 1024; i++; }
  return i === 0 ? `${v} B` : `${v.toFixed(1)} ${u[i]}`;
}
const commas = (n) => (Number(n) || 0).toLocaleString("en-US");

function log(text, kind) {
  const li = document.createElement("li");
  li.textContent = text;
  if (kind) li.className = kind;
  $("log").prepend(li);
  while ($("log").children.length > 60) $("log").lastChild.remove();
}

// ---- devices ---------------------------------------------------------------------------

async function loadDevices() {
  const list = $("device-list");
  list.innerHTML = "";
  let answer;
  try { answer = await invoke("devices"); }
  catch (e) { log(String(e), "err"); return; }

  for (const d of answer.devices || []) {
    const li = document.createElement("li");
    const refused = d.refusal;
    // The one rule that is absolute is stated in full, not hidden behind a disabled button.
    const side = d.holds_root
      ? `<span class="tag sys">system disk</span><span class="why">this is what you are running; it can never be a target</span>`
      : refused
        ? `<span class="tag no">not offered</span><span class="why">${refused}</span>`
        : (d.mounts && d.mounts.length)
          ? `<span class="tag warn">mounted</span><span class="why">${d.mounts.join(", ")}, it will be unmounted first</span>`
          : `<span class="tag ok">ready</span>`;
    li.innerHTML = `<div>
        <div class="name">${d.name}<span class="model">${d.model || ""}</span></div>
        <div class="meta">${d.size_human} &middot; ${d.removable ? "removable" : "internal"}</div>
      </div><div class="side">${side}</div>`;

    const canPick = !d.holds_root && (!refused || d.refusal_internal_allowed === null);
    if (!canPick) li.classList.add("refused");
    else li.onclick = () => {
      picked.device = d.name;
      // an internal disk is allowed only after it was chosen deliberately, and it says so
      picked.allowInternal = !!refused && d.refusal_internal_allowed === null;
      [...list.children].forEach((x) => x.classList.remove("picked"));
      li.classList.add("picked");
      if (picked.allowInternal) log(`${d.name} is an internal disk; it will need --allow-internal`, "err");
      ready();
    };
    list.appendChild(li);
  }
  if (!(answer.devices || []).length) log("no block devices found", "err");
}

async function loadLocal() {
  const list = $("local-list");
  list.innerHTML = "";
  const files = await invoke("local_images");
  for (const f of files) {
    const li = document.createElement("li");
    li.innerHTML = `<div><div class="name">${f.name}</div>
      <div class="meta">${f.path}</div></div><span class="side meta">${human(f.size)}</span>`;
    li.onclick = () => {
      $("image-path").value = f.path;
      picked.image = f.path;
      [...list.children].forEach((x) => x.classList.remove("picked"));
      li.classList.add("picked");
      ready();
    };
    list.appendChild(li);
  }
  if (!files.length) {
    const li = document.createElement("li");
    li.className = "empty";
    li.textContent = "No .iso or .img found in Downloads or your home. Type a path above, or fetch one from Images.";
    list.appendChild(li);
  }
}

function ready() {
  picked.image = $("image-path").value.trim() || picked.image;
  $("burn-btn").disabled = !(picked.image && picked.device);
}

// ---- the catalog -----------------------------------------------------------------------

async function loadImages() {
  const answer = await invoke("images");
  images = answer.images || [];
  renderImages();
}

function renderImages() {
  const term = $("search").value.toLowerCase();
  const family = $("family").value;
  const list = $("image-list");
  list.innerHTML = "";
  for (const i of images) {
    if (family && i.family !== family) continue;
    if (term && !(`${i.id} ${i.name} ${i.note}`.toLowerCase().includes(term))) continue;
    const li = document.createElement("li");
    li.innerHTML = `<div><div class="name">${i.name}<span class="model">${i.id}</span></div>
        <div class="meta">${i.note}</div></div>
      <div class="acts"><button class="glass small" data-act="resolve">Latest</button>
           <button class="glass small" data-act="get">Download</button></div>`;
    li.querySelector('[data-act="resolve"]').onclick = async (e) => {
      e.stopPropagation();
      // the answer belongs next to the row that was asked, not only in the log on another
      // panel where nobody looking at this list would ever see it
      const meta = li.querySelector(".meta");
      const was = meta.textContent;
      meta.textContent = "asking the mirror…";
      log(`asking ${i.id} what the newest build is…`, "step");
      try {
        const r = await invoke("resolve", { id: i.id });
        meta.innerHTML = `<span style="color:var(--ok)">${r.filename}</span>` +
          (r.size ? ` &middot; ${human(r.size)}` : "") +
          (r.sha256 ? `<br><span style="font-size:.66rem">sha256 ${r.sha256}</span>` : "");
        log(`${i.id}: ${r.filename}${r.size ? " (" + human(r.size) + ")" : ""}`, "ok");
      } catch (err) {
        meta.textContent = was;
        log(String(err), "err");
      }
    };
    li.querySelector('[data-act="get"]').onclick = async (e) => {
      e.stopPropagation();
      const out = (await homeDir()) + "/Downloads";
      log(`downloading ${i.id} into ${out}`, "step");
      showProgress("download");
      try { await invoke("start_download", { id: i.id, out }); }
      catch (err) { log(String(err), "err"); hideProgress(); }
    };
    list.appendChild(li);
  }
}

async function homeDir() {
  try { const d = await invoke("local_images"); return (d[0]?.path || "").split("/").slice(0, 3).join("/") || "."; }
  catch { return "."; }
}

// ---- running a job ---------------------------------------------------------------------

function showProgress(phase) {
  $("progress").classList.remove("hidden");
  $("phase").textContent = phase;
  $("bar-fill").style.width = "0%";
  $("bytes").textContent = "0";
  $("rate").textContent = "";
  $("eta").textContent = "";
}
function hideProgress() { $("progress").classList.add("hidden"); }

listen("arxburn", (e) => {
  const m = e.payload || {};
  switch (m.event) {
    case "progress": {
      const pct = m.total ? Math.floor((m.done * 100) / m.total) : 0;
      $("bar-fill").style.width = pct + "%";
      $("phase").textContent = `${m.phase || "working"} ${pct}%`;
      // byte for byte, because that is the number that shows it is really moving
      $("bytes").textContent = `${commas(m.done)} / ${commas(m.total)} bytes`;
      $("rate").textContent = `${human(m.bytes_per_second)}/s`;
      const eta = m.eta_seconds || 0;
      $("eta").textContent = eta ? `eta ${String(Math.floor(eta / 60)).padStart(2, "0")}:${String(eta % 60).padStart(2, "0")}` : "";
      break;
    }
    case "step": log(m.message, "step"); break;
    case "ok": log(m.message, "ok"); break;
    case "error": log(m.message, "err"); break;
    case "target": log(`target ${m.device}, ${human(m.size)}`); break;
    case "done":
      hideProgress();
      log(m.verified ? `verified: the disk carries the image, byte for byte (${m.sha256})`
                     : `MISMATCH: do not boot this disk`, m.verified ? "ok" : "err");
      loadLocal();
      break;
    case "finished":
      hideProgress();
      if (m.code !== 0 && m.detail) log(m.detail, "err");
      loadDevices();
      break;
  }
});

$("burn-btn").onclick = async () => {
  ready();
  if (!picked.image || !picked.device) return;
  const sure = confirm(
    `This erases /dev/${picked.device} completely and cannot be undone.\n\n` +
    `Write ${picked.image}?`);
  if (!sure) return;
  $("log").innerHTML = "";
  showProgress("starting");
  log(`writing ${picked.image} to /dev/${picked.device}`, "step");
  try {
    await invoke("start_burn", {
      image: picked.image, device: picked.device,
      allowInternal: picked.allowInternal, verify: $("verify").checked,
    });
  } catch (e) { log(String(e), "err"); hideProgress(); }
};

$("cancel").onclick = async () => {
  try { await invoke("cancel"); log("stopped", "err"); } catch (e) { log(String(e), "err"); }
};
$("refresh").onclick = () => { loadDevices(); loadLocal(); };
$("image-path").oninput = ready;
$("search").oninput = renderImages;
$("family").onchange = renderImages;

document.querySelectorAll(".nav-item").forEach((tab) => {
  tab.onclick = () => {
    document.querySelectorAll(".nav-item").forEach((t) => t.classList.remove("active"));
    document.querySelectorAll(".panel").forEach((p) => p.classList.remove("active"));
    tab.classList.add("active");
    $(tab.dataset.tab).classList.add("active");
    if (tab.dataset.tab === "images" && !images.length) loadImages();
  };
});

(async () => {
  // the deck foot carries what the engine says about itself: version, hash engine, block size
  const v = await invoke("version");
  const m = v.match(/^(\S+\s+\S+)\s*\((.*)\)$/);
  $("version").textContent = m ? m[1] : v;
  $("engine").textContent = m ? m[2] : "";
  await loadDevices();
  await loadLocal();
})();
