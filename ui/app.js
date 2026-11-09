// The window's half of arxburn. It asks the CLI for facts and shows what comes back; every
// decision about what may be written to lives in the CLI, which is the part with the tests.
const invoke = window.__TAURI__.core.invoke;
const listen = window.__TAURI__.event.listen;

const $ = (id) => document.getElementById(id);
let picked = { image: null, device: null, allowInternal: false, refused: "" };
let sawProgress = false;
let images = [];
let place = { home: "", downloads: "" };

// Anything that came from outside this program is text, never markup: a file name in Downloads,
// a USB stick's model string (a stick names itself), a file name scraped from a mirror's listing.
// Put into innerHTML raw, any of them could run script in a window that can start a burn.
const esc = (s) => String(s ?? "").replace(/[&<>"']/g,
  (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));

// "~/" in a typed path means the home folder, as it does in a terminal
const expand = (p) => (p.startsWith("~/") && place.home ? place.home + p.slice(1) : p);

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
        ? `<span class="tag no">not offered</span><span class="why">${esc(refused)}</span>`
        : (d.mounts && d.mounts.length)
          ? `<span class="tag warn">mounted</span><span class="why">${esc(d.mounts.join(", "))}, it will be unmounted first</span>`
          : `<span class="tag ok">ready</span>`;
    li.innerHTML = `<div>
        <div class="name">${esc(d.name)}<span class="model">${esc(d.model)}</span></div>
        <div class="meta">${esc(d.size_human)} &middot; ${d.removable ? "removable" : "internal"}</div>
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

// A compressed image is also described by what lands on the stick, because that is what has to fit.
function sizeCell(f) {
  const unpacked = f.write_size ? `${human(f.write_size)} unpacked` : "unpacked size not recorded";
  return esc(human(f.size)) + (f.compression ? `<span class="sub">${esc(f.compression)}, ${esc(unpacked)}</span>` : "");
}

// shown with ~ for the home folder, as a terminal would; the full path stays in the tooltip
const tilde = (p) => (place.home && p.startsWith(place.home + "/") ? "~" + p.slice(place.home.length) : p);

// The images the CLI found on this machine, read by content. One list feeds both panels.
async function localImages() {
  try { return await invoke("local_images"); }
  catch (e) { log(String(e), "err"); return []; }
}

// One row of a found image: name, what it is, where it is, how big.
function imageRow(f, onPick) {
  const li = document.createElement("li");
  li.dataset.path = f.path;
  li.innerHTML = `<div><div class="name">${esc(f.name)}<span class="model">${esc(f.kind)}</span>` +
    (f.external ? `<span class="tag ext">USB drive</span>` : "") + `</div>
    <div class="meta">${esc(tilde(f.path))}</div></div><span class="side meta size">${sizeCell(f)}</span>`;
  li.title = `${f.path}\n${f.label || ""}`;
  li.onclick = () => onPick(f.path);
  return li;
}

function emptyRow(list, text) {
  const li = document.createElement("li");
  li.className = "empty";
  li.textContent = text;
  list.appendChild(li);
}

const NONE_FOUND = "No disk images found in Desktop, Downloads, ~/iso, your home folder or a mounted " +
  "USB drive. Browse for one, drop it on this window, or fetch one from Images.";

async function loadLocal() {
  const list = $("local-list");
  const files = await localImages();
  list.innerHTML = "";
  for (const f of files) list.appendChild(imageRow(f, pickImage));
  if (!files.length) emptyRow(list, NONE_FOUND);
  mark(list, picked.image);
}

// highlight the row for this path, if the list has one
function mark(list, path) {
  [...list.children].forEach((x) => x.classList.toggle("picked", !!path && x.dataset.path === path));
}

// Every way of choosing an image (the list, Browse, a dropped file, typing) ends here, so they
// cannot behave differently.
function pickImage(path) {
  $("image-path").value = path;
  mark($("local-list"), path);
  showScheme(path);
}

// What this file is and what will boot from it. Asked of the CLI, which reads the content: the
// format, the compression and the size once unpacked, or why it cannot be written at all.
async function showScheme(raw) {
  const el = $("image-scheme");
  const path = expand(raw.trim());
  picked.refused = "";
  ready();
  if (!path) { el.textContent = ""; return; }
  if (!path.startsWith("/")) {
    el.innerHTML = `<span class="caution">Give the full path, starting with / (or ~/ for your home folder).</span>`;
    return;
  }
  el.textContent = "reading the first sectors…";
  const stale = () => expand($("image-path").value.trim()) !== path; // a newer pick replaced it
  try {
    const r = await invoke("inspect", { path });
    if (stale()) return;
    if (r.refused) {
      // said in full, and Write stays off: the CLI would refuse it anyway, after a password
      picked.refused = r.refused;
      el.innerHTML = `<span class="bad">This cannot be written to a disk: ${esc(r.refused)}</span>`;
    } else {
      const unpacked = r.write_size ? `${human(r.write_size)} once unpacked` : "unpacked size not recorded in the file";
      const what = r.compression
        ? `${esc(r.format_label)} &middot; ${esc(r.compression)}, ${unpacked}`
        : `${esc(r.format_label)} &middot; ${human(r.write_size || r.file_size)}`;
      const cls = r.boots && r.boots.startsWith("hybrid") ? "yes" : "caution";
      el.innerHTML = `<span class="what">${what}</span><br>${esc(r.summary)} &middot; <span class="${cls}">${esc(r.boots)}</span>`;
    }
  } catch (e) {
    if (stale()) return;
    el.innerHTML = `<span class="caution">${esc(e)}</span>`;
  }
  ready();
}

function ready() {
  const p = expand($("image-path").value.trim());
  picked.image = p.startsWith("/") ? p : null;
  $("burn-btn").disabled = !(picked.image && picked.device && !picked.refused);
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
    li.innerHTML = `<div><div class="name">${esc(i.name)}<span class="model">${esc(i.id)}</span></div>
        <div class="meta">${esc(i.note)}</div></div>
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
        // the file name comes from the mirror's own listing: text, never markup
        meta.innerHTML = `<span style="color:var(--ok)">${esc(r.filename)}</span>` +
          (r.size ? ` &middot; ${human(r.size)}` : "") +
          (r.sha256 ? `<br><span style="font-size:.66rem">sha256 ${esc(r.sha256)}</span>` : "");
        log(`${i.id}: ${r.filename}${r.size ? " (" + human(r.size) + ")" : ""}`, "ok");
      } catch (err) {
        meta.textContent = was;
        log(String(err), "err");
      }
    };
    li.querySelector('[data-act="get"]').onclick = async (e) => {
      e.stopPropagation();
      const out = place.downloads || ".";
      const meta = li.querySelector(".meta");
      meta.textContent = `downloading into ${out}…`;
      log(`downloading ${i.id} into ${out}`, "step");
      showProgress("download");
      try { await invoke("start_download", { id: i.id, out }); }
      catch (err) { meta.innerHTML = `<span class="caution">${esc(err)}</span>`; log(String(err), "err"); hideProgress(); }
    };
    list.appendChild(li);
  }
}

// ---- running a job ---------------------------------------------------------------------

function showProgress(phase) {
  $("progress").classList.remove("hidden");
  $("phase").textContent = phase;
  $("bar-fill").style.width = "0%";
  $("bytes").textContent = "0";
  $("rate").textContent = "";
  $("eta").textContent = "";
  $("warn").classList.add("hidden");
  // bring it into view rather than pinning it over the page
  $("progress").scrollIntoView({ block: "nearest" });
}
function hideProgress() { $("progress").classList.add("hidden"); }

// ---- the ending -------------------------------------------------------------------------
// A burn is watched for minutes and then walked away from. The sound is so somebody in the next
// room knows it finished; the dialog is so they know WHICH way it finished.
function ping(good) {
  try {
    const AC = window.AudioContext || window.webkitAudioContext;
    if (!AC) return;
    const ctx = new AC();
    // two notes up for success, two down for a mismatch: recognisable without looking
    const notes = good ? [880, 1318.5] : [440, 311.1];
    notes.forEach((hz, i) => {
      const osc = ctx.createOscillator(), gain = ctx.createGain();
      osc.type = "sine";
      osc.frequency.value = hz;
      const t = ctx.currentTime + i * 0.13;
      // a short envelope, because an abrupt stop on a sine is an audible click
      gain.gain.setValueAtTime(0.0001, t);
      gain.gain.exponentialRampToValueAtTime(0.22, t + 0.015);
      gain.gain.exponentialRampToValueAtTime(0.0001, t + 0.30);
      osc.connect(gain).connect(ctx.destination);
      osc.start(t);
      osc.stop(t + 0.32);
    });
    setTimeout(() => ctx.close().catch(() => {}), 900);
  } catch (_) { /* no audio device is not a failure worth reporting */ }
}

function dialog(good, title, body, hash) {
  $("dialog-mark").textContent = good ? "\u2713" : "!";
  $("dialog-mark").className = "dialog-mark" + (good ? "" : " bad");
  $("dialog-title").textContent = title;
  $("dialog-body").textContent = body;
  $("dialog-hash").textContent = hash || "";
  $("dialog").classList.remove("hidden");
  ping(good);
  $("dialog-close").focus();
}
$("dialog-close").onclick = () => $("dialog").classList.add("hidden");
document.addEventListener("keydown", (e) => {
  if (e.key === "Escape") $("dialog").classList.add("hidden");
});

listen("arxburn", (e) => {
  const m = e.payload || {};
  switch (m.event) {
    case "progress": {
      sawProgress = true;
      const known = m.total > 0;
      const pct = known ? Math.floor((m.done * 100) / m.total) : 0;
      // a server that never said how big the file is must not look like a bar that is stuck
      $("bar-fill").style.width = known ? pct + "%" : "100%";
      $("bar-fill").style.opacity = known ? "1" : "0.35";
      $("phase").textContent = known ? `${m.phase || "working"} ${pct}%` : `${m.phase || "working"}`;
      $("bytes").textContent = known
        ? `${commas(m.done)} / ${commas(m.total)} bytes`
        : `${commas(m.done)} bytes (total unknown)`;
      // 100% is not done. The image has been handed to the kernel; the stick is still taking it.
      const w = $("warn");
      if (m.phase === "write" || m.phase === "flush") {
        w.classList.remove("hidden");
        w.innerHTML = m.phase === "flush"
          ? `<strong>Still writing.</strong> The image is in the kernel's cache and the stick is
             taking it now, which on a slow stick is most of the wait. Do not unplug it.`
          : `<strong>Do not unplug the stick.</strong> Reaching 100% here means the image has been
             handed to the kernel, not that the stick has it yet.`;
      } else {
        w.classList.add("hidden");
      }
      $("rate").textContent = `${human(m.bytes_per_second)}/s`;
      const eta = m.eta_seconds || 0;
      $("eta").textContent = eta ? `eta ${String(Math.floor(eta / 60)).padStart(2, "0")}:${String(eta % 60).padStart(2, "0")}` : "";
      break;
    }
    case "started":
      sawProgress = false;
      log(m.command, "step");
      break;
    case "step": log(m.message, "step"); break;
    case "ok": log(m.message, "ok"); break;
    case "error": log(m.message, "err"); break;
    case "warning": log(m.message, "warn"); break;
    case "target": log(`target ${m.device}, ${human(m.size)}`); break;
    case "format":
      log(m.compression
        ? `${m.label}, ${m.compression}: unpacked while it is written` +
          (m.write_size ? ` (${human(m.write_size)})` : "")
        : m.label);
      break;

    // a file held over the window, then let go
    case "drag":
      $("drop-title").textContent = activePanel() === "verify" ? "Drop it to check it" : "Drop it to write it";
      $("drop").classList.toggle("hidden", !m.over);
      break;
    case "drop": {
      $("drop").classList.add("hidden");
      const paths = m.paths || [];
      if (!paths.length) break;
      if (paths.length > 1) log(`${paths.length} files dropped; using the first, ${paths[0]}`, "warn");
      if (activePanel() === "verify") pickVerify(paths[0]);
      else { showPanel("burn"); pickImage(paths[0]); }
      break;
    }
    case "done":
      hideProgress();
      log(m.verified ? `verified: the disk carries the image, byte for byte (${m.sha256})`
                     : `MISMATCH: do not boot this disk`, m.verified ? "ok" : "err");
      dialog(m.verified,
        m.verified ? "Written and verified" : "It did not match",
        m.verified
          ? `The disk was read back and every byte matches the image. It is safe to boot.`
          : `What was read back off the disk is not what the image contains. Do not boot it; write it again, and if it fails twice the stick is probably failing.`,
        m.sha256 ? `sha256 ${m.sha256}` : "");
      loadLocal();
      break;

    // a hash run, from the Verify panel
    case "hash": {
      hideProgress();
      const out = $("verify-out");
      const short = `${m.sha256}`;
      if (m.matches === true) {
        out.className = "verify-out good";
        out.innerHTML = `This is the file the project published.<span class="sha">${short}</span>`;
        dialog(true, "It matches",
          "The sha256 you pasted and the one computed from this file are the same, so the download is intact and unaltered.",
          `sha256 ${short}`);
      } else if (m.matches === false) {
        out.className = "verify-out bad";
        out.innerHTML = `This is NOT the file that hash describes.
          <span class="sha">computed ${short}</span><span class="sha">you gave ${esc(m.expected)}</span>`;
        dialog(false, "It does not match",
          "The file on disk hashes to something else. Either the download was corrupted or truncated, or it is not the file that hash belongs to. Download it again before writing it to anything.",
          `computed ${short}`);
      } else {
        out.className = "verify-out";
        out.innerHTML = `${human(m.size)}, read with the ${m.engine} engine.<span class="sha">${short}</span>`;
        dialog(true, "Hashed",
          "Compare this against the sha256 on the project's download page. If they are the same, the file is intact.",
          `sha256 ${short}`);
      }
      break;
    }

    // a download that finished without being written anywhere
    case "ready":
      hideProgress();
      dialog(true, "Downloaded",
        `It is on disk and its hash was checked. Pick it in Burn to write it to a stick.`,
        m.path || "");
      loadLocal();
      break;
    case "finished":
      hideProgress();
      if (updating) {
        // the running window is the binary that was just replaced, so it cannot become the new
        // one by itself: say so, rather than leaving a button reading "updating" forever
        updating = false;
        const b = $("update-btn");
        b.classList.remove("ready");
        if (m.code === 0) {
          updateReady = false;
          b.textContent = "Check for updates";
          dialog(true, "Updated",
            "Both binaries were replaced, each checked against its published sha256. Close this window and open it again to run the new one.", "");
        } else {
          b.textContent = "Update failed";
          dialog(false, "The update did not go through",
            m.detail || "Nothing was replaced: arxburn only overwrites a file once the download matches its published hash.", "");
        }
      }
      if (m.code !== 0 && m.detail) log(m.detail, "err");
      // a run that ended without a single progress line never started: say so, rather than
      // leaving a bar that simply never moved
      if (!sawProgress && m.code !== 0) {
        log(`nothing ran (exit ${m.code}). If a password dialog did not appear, polkit may be missing; ` +
            `try it in a terminal with sudo.`, "err");
      }
      loadDevices();
      break;
  }
});

$("burn-btn").onclick = async () => {
  ready();
  if (!picked.image || !picked.device) return;
  const scheme = $("scheme").value;
  const schemeNote = scheme === "auto" ? ""
    : `\n\nAfter verifying, the stick will be left looking like ${scheme.toUpperCase()}.`;
  const sure = confirm(
    `This erases /dev/${picked.device} completely and cannot be undone.\n\n` +
    `Write ${picked.image}?` + schemeNote);
  if (!sure) return;
  $("log").innerHTML = "";
  showProgress("starting");
  log(`writing ${picked.image} to /dev/${picked.device}`, "step");
  try {
    await invoke("start_burn", {
      image: picked.image, device: picked.device,
      allowInternal: picked.allowInternal, verify: $("verify-readback").checked,
      scheme: $("scheme").value,
    });
  } catch (e) { log(String(e), "err"); hideProgress(); }
};

$("cancel").onclick = async () => {
  try { await invoke("cancel"); log("stopped", "err"); } catch (e) { log(String(e), "err"); }
};
$("refresh").onclick = () => { loadDevices(); loadLocal(); };

// ---- verify ---------------------------------------------------------------------------------
async function loadVerifyList() {
  const list = $("verify-list");
  const files = await localImages();
  list.innerHTML = "";
  for (const f of files) list.appendChild(imageRow(f, pickVerify));
  if (!files.length) emptyRow(list, NONE_FOUND);
  mark(list, expand($("verify-path").value.trim()));
}

function pickVerify(path) {
  $("verify-path").value = path;
  mark($("verify-list"), path);
  $("verify-out").className = "verify-out";
  $("verify-out").textContent = "";
}

$("verify-btn").onclick = async () => {
  const path = expand($("verify-path").value.trim());
  if (!path) { $("verify-out").className = "verify-out bad";
               $("verify-out").textContent = "Pick a file first."; return; }
  if (!path.startsWith("/")) { $("verify-out").className = "verify-out bad";
               $("verify-out").textContent = "Give the full path, starting with / (or ~/ for your home folder)."; return; }
  const expect = $("verify-expect").value.trim();
  $("verify-out").className = "verify-out";
  $("verify-out").textContent = "reading the whole file…";
  $("log").innerHTML = "";
  showProgress("hash");
  try {
    await invoke("start_hash", { path, expect });
  } catch (e) {
    hideProgress();
    $("verify-out").className = "verify-out bad";
    $("verify-out").textContent = String(e);
  }
};
let schemeTimer = null;
$("image-path").oninput = () => {
  // what was refused was the previous path, not this one
  picked.refused = "";
  ready();
  mark($("local-list"), expand($("image-path").value.trim()));
  clearTimeout(schemeTimer);
  schemeTimer = setTimeout(() => showScheme($("image-path").value), 500);
};
$("verify-path").oninput = () => mark($("verify-list"), expand($("verify-path").value.trim()));
$("search").oninput = renderImages;
$("family").onchange = renderImages;

// The desktop's own file picker. Closing it without choosing changes nothing.
async function browse(purpose) {
  let path = null;
  try { path = await invoke("browse", { purpose }); }
  catch (e) { log(String(e), "err"); return; }
  if (!path) return;
  if (purpose === "verify") pickVerify(path); else pickImage(path);
}
$("browse").onclick = () => browse("burn");
$("verify-browse").onclick = () => browse("verify");

const activePanel = () => document.querySelector(".panel.active")?.id || "burn";
function showPanel(id) {
  document.querySelectorAll(".nav-item").forEach((t) => t.classList.toggle("active", t.dataset.tab === id));
  document.querySelectorAll(".panel").forEach((p) => p.classList.toggle("active", p.id === id));
  if (id === "images" && !images.length) loadImages();
  if (id === "verify") loadVerifyList();
}
document.querySelectorAll(".nav-item").forEach((tab) => { tab.onclick = () => showPanel(tab.dataset.tab); });

// Updates: the CLI owns the version comparison and the hash check, this only asks and reports.
let updateReady = false;
let updating = false;
$("update-btn").onclick = async () => {
  const b = $("update-btn");
  if (updateReady) {
    if (!confirm("Replace the installed arxburn with the newest release?")) return;
    b.textContent = "updating…";
    updating = true;
    log("downloading the newest release", "step");
    try { await invoke("start_update"); }
    catch (e) { updating = false; log(String(e), "err"); b.textContent = "Check for updates"; }
    return;
  }
  b.textContent = "checking…";
  try {
    const r = await invoke("update_check");
    if (r.error) { log(r.error, "err"); b.textContent = "Check for updates"; return; }
    if (r.available) {
      updateReady = true;
      b.textContent = `Update to ${r.latest}`;
      b.classList.add("ready");
      log(`${r.latest} is available (you have ${r.current})`, "ok");
    } else {
      b.textContent = "Up to date";
      log(`${r.current} is the newest release`, "ok");
      setTimeout(() => { b.textContent = "Check for updates"; }, 4000);
    }
  } catch (e) {
    log(String(e), "err");
    b.textContent = "Check for updates";
  }
};

(async () => {
  // the deck foot carries what the engine says about itself: version, hash engine, block size
  const v = await invoke("version");
  const m = v.match(/^(\S+\s+\S+)\s*\((.*)\)$/);
  $("version").textContent = m ? m[1] : v;
  $("engine").textContent = m ? m[2] : "";
  try { place = await invoke("places"); } catch (_) { /* typed ~/ paths just stay unexpanded */ }
  await loadDevices();
  await loadLocal();
})();
