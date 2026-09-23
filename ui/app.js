// The window's half of arxburn. It asks the CLI for facts and shows what comes back; every
// decision about what may be written to lives in the CLI, which is the part with the tests.
const invoke = window.__TAURI__.core.invoke;
const listen = window.__TAURI__.event.listen;

const $ = (id) => document.getElementById(id);
let picked = { image: null, device: null, allowInternal: false };
let sawProgress = false;
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
      showScheme(f.path);
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

// What will actually boot from this image. Asked of the CLI, which reads the sectors.
async function showScheme(path) {
  const el = $("image-scheme");
  if (!path) { el.textContent = ""; return; }
  el.textContent = "reading the first sectors…";
  try {
    const r = await invoke("inspect", { path });
    const cls = r.boots && r.boots.startsWith("hybrid") ? "yes" : "warn";
    el.innerHTML = `${r.summary} &middot; <span class="${cls}">${r.boots}</span>`;
  } catch (e) {
    el.innerHTML = `<span class="warn">${String(e)}</span>`;
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
      const meta = li.querySelector(".meta");
      meta.textContent = `downloading into ${out}…`;
      log(`downloading ${i.id} into ${out}`, "step");
      showProgress("download");
      try { await invoke("start_download", { id: i.id, out }); }
      catch (err) { meta.innerHTML = `<span class="warn">${String(err)}</span>`; log(String(err), "err"); hideProgress(); }
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
  document.querySelector(".stage").classList.add("has-progress");
  $("phase").textContent = phase;
  $("bar-fill").style.width = "0%";
  $("bytes").textContent = "0";
  $("rate").textContent = "";
  $("eta").textContent = "";
}
function hideProgress() {
  $("progress").classList.add("hidden");
  document.querySelector(".stage").classList.remove("has-progress");
}

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
    case "target": log(`target ${m.device}, ${human(m.size)}`); break;
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
          <span class="sha">computed ${short}</span><span class="sha">you gave ${m.expected}</span>`;
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
  list.innerHTML = "";
  const files = await invoke("local_images");
  for (const f of files) {
    const li = document.createElement("li");
    li.innerHTML = `<div><div class="name">${f.name}</div><div class="meta">${f.path}</div></div>
      <span class="side meta">${human(f.size)}</span>`;
    li.onclick = () => {
      $("verify-path").value = f.path;
      [...list.children].forEach((x) => x.classList.remove("picked"));
      li.classList.add("picked");
    };
    list.appendChild(li);
  }
  if (!files.length) {
    const li = document.createElement("li");
    li.className = "empty";
    li.textContent = "No .iso or .img found. Type a path above.";
    list.appendChild(li);
  }
}

$("verify-btn").onclick = async () => {
  const path = $("verify-path").value.trim();
  if (!path) { $("verify-out").className = "verify-out bad";
               $("verify-out").textContent = "Pick a file first."; return; }
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
  ready();
  clearTimeout(schemeTimer);
  schemeTimer = setTimeout(() => showScheme($("image-path").value.trim()), 500);
};
$("search").oninput = renderImages;
$("family").onchange = renderImages;

document.querySelectorAll(".nav-item").forEach((tab) => {
  tab.onclick = () => {
    document.querySelectorAll(".nav-item").forEach((t) => t.classList.remove("active"));
    document.querySelectorAll(".panel").forEach((p) => p.classList.remove("active"));
    tab.classList.add("active");
    $(tab.dataset.tab).classList.add("active");
    if (tab.dataset.tab === "images" && !images.length) loadImages();
    if (tab.dataset.tab === "verify") loadVerifyList();
  };
});

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
  await loadDevices();
  await loadLocal();
})();
