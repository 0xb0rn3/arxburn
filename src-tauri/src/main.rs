// arxburn GUI: a window over the same binary the terminal uses.
//
// Nothing here knows how to write to a disk. Every action runs the `arxburn` CLI with --json and
// forwards what it says, which means the GUI cannot drift from the tool: the safety rules, the
// refusals and the verification are all decided in one place, by the code that has the tests.
// The same pattern as arxctl and droidB.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::Mutex;

use tauri::{AppHandle, Emitter, Manager, State};

#[derive(Default)]
struct Running(Mutex<Option<std::process::Child>>);

/// Prefer an installed arxburn, fall back to one sitting next to this binary (a dev build), so
/// the GUI works both from /usr/bin and straight out of target/release.
fn cli() -> String {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let beside = dir.join("arxburn");
            if beside.is_file() { return beside.to_string_lossy().into_owned(); }
        }
    }
    "arxburn".into()
}

/// One-shot commands: run it, hand back the single JSON line it printed.
fn one_shot(args: &[&str]) -> Result<serde_json::Value, String> {
    let out = Command::new(cli()).args(args).arg("--json").output()
        .map_err(|e| format!("cannot run arxburn: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    let last = text.lines().filter(|l| l.starts_with('{')).next_back()
        .ok_or_else(|| {
            let err = String::from_utf8_lossy(&out.stderr);
            if err.trim().is_empty() { "arxburn said nothing".to_string() } else { err.trim().to_string() }
        })?;
    serde_json::from_str(last).map_err(|e| format!("cannot read arxburn's answer: {e}"))
}

#[tauri::command]
fn devices() -> Result<serde_json::Value, String> { one_shot(&["list"]) }

#[tauri::command]
fn images() -> Result<serde_json::Value, String> { one_shot(&["iso"]) }

#[tauri::command]
fn resolve(id: String) -> Result<serde_json::Value, String> { one_shot(&["iso", &id]) }

#[tauri::command]
fn networks() -> Result<serde_json::Value, String> { one_shot(&["net"]) }

/// Images already on disk, so the common case (a file you downloaded yesterday) needs no typing.
#[tauri::command]
fn local_images() -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    let home = std::env::var("HOME").unwrap_or_default();
    let dirs = [format!("{home}/Downloads"), format!("{home}/iso"), format!("{home}"),
                std::env::current_dir().map(|d| d.to_string_lossy().into_owned()).unwrap_or_default()];
    for dir in dirs.iter().filter(|d| !d.is_empty()) {
        let Ok(entries) = std::fs::read_dir(dir) else { continue };
        for e in entries.flatten() {
            let path = e.path();
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let lower = name.to_lowercase();
            if !(lower.ends_with(".iso") || lower.ends_with(".img")) { continue; }
            let size = e.metadata().map(|m| m.len()).unwrap_or(0);
            if out.len() < 60 {
                out.push(serde_json::json!({
                    "name": name, "path": path.to_string_lossy(), "size": size,
                }));
            }
        }
    }
    out.sort_by_key(|v| v["name"].as_str().unwrap_or("").to_lowercase());
    out.dedup_by_key(|v| v["path"].as_str().unwrap_or("").to_string());
    out
}

/// Start a long job (download or burn) and stream its JSON lines to the window as they arrive.
fn stream(app: AppHandle, state: State<'_, Running>, mut args: Vec<String>, privileged: bool) -> Result<(), String> {
    {
        let mut slot = state.0.lock().unwrap();
        if slot.is_some() { return Err("something is already running".into()); }
        args.push("--json".into());
        let mut cmd = if privileged {
            // A burn needs root. pkexec asks through the desktop's own dialog, so no password
            // is ever typed into this window, and the GUI never holds one.
            let mut c = Command::new("pkexec");
            c.arg(cli());
            c
        } else {
            Command::new(cli())
        };
        cmd.args(&args).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = cmd.spawn().map_err(|e| {
            if privileged { format!("cannot start pkexec: {e}. Install polkit, or burn from a terminal with sudo.") }
            else { format!("cannot run arxburn: {e}") }
        })?;
        let stdout = child.stdout.take().ok_or("no output from arxburn")?;
        let stderr = child.stderr.take();
        *slot = Some(child);

        let a = app.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) {
                    let _ = a.emit("arxburn", v);
                }
            }
            // whatever the exit status, the window is told the run is over
            let app2 = a.clone();
            let state: State<'_, Running> = app2.state();
            let code = {
                let mut slot = state.0.lock().unwrap();
                match slot.take() { Some(mut c) => c.wait().ok().and_then(|s| s.code()).unwrap_or(-1), None => -1 }
            };
            let mut detail = String::new();
            if let Some(e) = stderr {
                for line in BufReader::new(e).lines().map_while(Result::ok) {
                    if !line.trim().is_empty() { detail = line; }
                }
            }
            let _ = a.emit("arxburn", serde_json::json!({
                "event": "finished", "code": code, "detail": detail
            }));
        });
    }
    Ok(())
}

#[tauri::command]
fn start_burn(app: AppHandle, state: State<'_, Running>, image: String, device: String,
              allow_internal: bool, verify: bool) -> Result<(), String> {
    let mut args = vec!["write".to_string(), image, "--to".into(), device, "--yes".into()];
    if allow_internal { args.push("--allow-internal".into()); }
    if !verify { args.push("--no-verify".into()); }
    stream(app, state, args, true)
}

#[tauri::command]
fn start_download(app: AppHandle, state: State<'_, Running>, id: String, out: String) -> Result<(), String> {
    stream(app, state, vec!["get".into(), id, "--out".into(), out], false)
}

#[tauri::command]
fn cancel(state: State<'_, Running>) -> Result<(), String> {
    let mut slot = state.0.lock().unwrap();
    if let Some(child) = slot.as_mut() {
        // Only ever this child, by pid. A pattern kill would be a way to take out the rest of
        // the desktop, and a half-killed burn is exactly the state nobody wants.
        child.kill().map_err(|e| format!("cannot stop it: {e}"))?;
    }
    Ok(())
}

#[tauri::command]
fn version() -> String {
    Command::new(cli()).arg("--version").output().ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "arxburn (not installed)".into())
}

fn main() {
    // WebKitGTK picks a renderer at startup and gets it wrong often enough that a blank white
    // window is the single most common way this app "does not work": DMA-BUF buffers fail on
    // plenty of drivers, in virtual machines, and over remote sessions, and WebKit does not fall
    // back on its own. Turning that off costs nothing here (this UI is text and boxes) and it is
    // only set when the user has not chosen for themselves.
    if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
        std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
    }
    if std::env::var_os("WEBKIT_DISABLE_COMPOSITING_MODE").is_none() {
        std::env::set_var("WEBKIT_DISABLE_COMPOSITING_MODE", "1");
    }

    tauri::Builder::default()
        .manage(Running::default())
        .invoke_handler(tauri::generate_handler![
            devices, images, resolve, networks, local_images,
            start_burn, start_download, cancel, version
        ])
        .run(tauri::generate_context!())
        .expect("arxburn GUI failed to start");
}
