//! arxburn: write an image to a USB stick, and prove the stick got it.
//!
//! The three tools people actually use each get one thing right. Etcher refuses to touch your
//! system disk and verifies afterwards, but ships a browser to do it. dd is exact and always
//! present, and will happily erase the wrong disk without a word. Rufus gives you real control
//! over the target and a list of images to fetch. arxburn keeps all of that as one small native
//! binary with no dependencies.
//!
//!   arxburn list                                  what is plugged in, and what is refused
//!   arxburn iso [term]                            images it can fetch, newest resolved live
//!   arxburn get <id> [--to <device>]              download, check, and optionally burn
//!   arxburn write <image> --to <device|UUID>      burn a file you already have
//!   arxburn verify <image> --to <device|UUID>     re-check a stick burned earlier
//!
//! Safety, in order of importance:
//!   - the disk carrying "/" is never writable, with any flag combination
//!   - internal disks need --allow-internal; removable media is the default
//!   - the image must fit, and mounted partitions are unmounted deliberately, never silently
//!   - without --yes you type the device name to proceed; a y/n prompt is too easy to fat-finger
//!   - after writing, the bytes are read BACK off the device and hashed against the image

mod catalog;
mod dev;
mod net;
mod sha256;

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{exit, Command};
use std::time::Instant;

const BUF: usize = 4 * 1024 * 1024;

const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const RED: &str = "\x1b[31m";
const GRN: &str = "\x1b[32m";
const YEL: &str = "\x1b[33m";
const RST: &str = "\x1b[0m";

fn ok(msg: &str) { println!("  {GRN}ok{RST} {msg}"); }
fn no(msg: &str) { eprintln!("  {RED}!!{RST} {msg}"); }
fn step(msg: &str) { println!("{BOLD}>>{RST} {msg}"); }

fn die(msg: &str) -> ! { no(msg); exit(1) }

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str).unwrap_or("help") {
        "list" | "ls" | "devices" => cmd_list(&args[2..]),
        "iso" | "images" | "catalog" => cmd_iso(&args[2..]),
        "get" | "fetch" | "download" => cmd_get(&args[2..]),
        "write" | "burn" | "w" => cmd_write(&args[2..]),
        "verify" | "check" => cmd_verify(&args[2..]),
        "--version" | "-V" => println!("arxburn {}", env!("CARGO_PKG_VERSION")),
        _ => usage(),
    }
}

fn usage() {
    println!("{BOLD}arxburn{RST} {} - write an image to a USB stick, and prove it landed", env!("CARGO_PKG_VERSION"));
    println!();
    println!("  arxburn list [--loop]                     block devices, and which are refused");
    println!("  arxburn iso [term] [--windows|--linux]    images it can fetch");
    println!("  arxburn get <id> [--to <device>]          download the newest, check it, burn it");
    println!("  arxburn write <image> --to <device|UUID>  burn a file you already have");
    println!("  arxburn verify <image> --to <device|UUID> re-check a stick burned earlier");
    println!();
    println!("{BOLD}options{RST}");
    println!("  --to <dev|UUID>     target: sdc, /dev/sdc, or a partition UUID (UUID survives replugging)");
    println!("  --expect <sha256>   check the IMAGE against a known hash before writing anything");
    println!("  --out <dir>         where downloads land (default: the current directory)");
    println!("  --yes               skip the typed confirmation (scripts)");
    println!("  --no-verify         skip the read-back check (not advised; it is the point)");
    println!("  --allow-internal    permit a non-removable disk. The running system stays refused.");
    println!("  --loop              allow /dev/loopN targets, for testing against an image file");
    println!();
    println!("{DIM}  The disk carrying / is never a target. Mounted partitions are unmounted first,");
    println!("  and you are told which. Without --yes you confirm by typing the device name.{RST}");
}

fn flag(args: &[String], name: &str) -> bool { args.iter().any(|a| a == name) }

fn opt(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

fn cmd_list(args: &[String]) {
    let devices = dev::list(flag(args, "--loop"));
    if devices.is_empty() { no("no block devices found"); return; }
    println!("{BOLD}  device      size        type       model                     status{RST}");
    for d in &devices {
        let kind = if d.holds_root { "SYSTEM" } else if d.removable { "removable" } else { "internal" };
        let status = match d.refusal(false) {
            Some(r) => format!("{RED}{r}{RST}"),
            None => {
                if d.mounts.is_empty() { format!("{GRN}writable{RST}") }
                else { format!("{YEL}writable, mounted at {}{RST}", d.mounts.join(", ")) }
            }
        };
        println!("  {:<11} {:<11} {:<10} {:<25} {}", d.name, dev::human(d.size), kind,
            &d.model.chars().take(25).collect::<String>(), status);
    }
}

// ---- catalog ------------------------------------------------------------------------------

fn cmd_iso(args: &[String]) {
    let term = args.iter().find(|a| !a.starts_with("--")).cloned();
    let only_win = flag(args, "--windows");
    let only_lin = flag(args, "--linux");
    let only_tool = flag(args, "--tools");
    let list = catalog::all();

    // A single exact id means "tell me about this one", which is worth a live lookup.
    if let Some(t) = &term {
        if let Some(iso) = list.iter().find(|i| &i.id == t) {
            step(&format!("{} ({})", iso.name, iso.id));
            println!("  {DIM}{}{RST}", iso.note);
            match catalog::resolve(iso) {
                Ok(r) => {
                    ok(&format!("newest: {}", r.filename));
                    println!("     {}", r.url);
                    if let Some(s) = net::remote_size(&r.url) { println!("     size   {}", dev::human(s)); }
                    match &r.sha256 {
                        Some(h) => println!("     sha256 {h} {DIM}(published by the project){RST}"),
                        None => println!("     {DIM}no published sha256; arxburn still verifies the stick against the file it downloaded{RST}"),
                    }
                    println!();
                    println!("  {BOLD}arxburn get {} --to <device>{RST}", iso.id);
                }
                Err(e) => no(&e),
            }
            return;
        }
    }

    println!("{BOLD}  id                name                           what{RST}");
    let mut shown = 0;
    for i in &list {
        if only_win && i.family != catalog::Family::Windows { continue; }
        if only_lin && i.family != catalog::Family::Linux { continue; }
        if only_tool && i.family != catalog::Family::Tool { continue; }
        if let Some(t) = &term {
            let t = t.to_lowercase();
            if !i.id.to_lowercase().contains(&t) && !i.name.to_lowercase().contains(&t)
               && !i.note.to_lowercase().contains(&t) { continue; }
        }
        let tag = match i.family {
            catalog::Family::Windows => format!("{YEL}win{RST}"),
            catalog::Family::Tool => format!("{DIM}tool{RST}"),
            catalog::Family::Linux => format!("{GRN}linux{RST}"),
        };
        println!("  {:<17} {:<30} {} {DIM}{}{RST}", i.id, i.name, tag, i.note);
        shown += 1;
    }
    if shown == 0 { no("nothing matches"); return; }
    println!();
    println!("{DIM}  arxburn iso <id>        resolve the newest build and show its link and hash");
    println!("  arxburn get <id> --to sdc   download it, check it, and burn it{RST}");
}

fn cmd_get(args: &[String]) {
    let id = match args.first() {
        Some(a) if !a.starts_with("--") => a.clone(),
        _ => die("usage: arxburn get <id> [--to <device>]   (see: arxburn iso)"),
    };
    let list = catalog::all();
    let iso = list.iter().find(|i| i.id == id)
        .or_else(|| {
            let m: Vec<_> = list.iter().filter(|i| i.id.contains(&id)).collect();
            if m.len() == 1 { Some(m[0]) } else { None }
        })
        .unwrap_or_else(|| die(&format!("no image called '{id}' (see: arxburn iso)")));

    step(&format!("{} ({})", iso.name, iso.id));
    let r = catalog::resolve(iso).unwrap_or_else(|e| die(&e));
    ok(&format!("newest is {}", r.filename));
    println!("     {}", r.url);
    let remote = net::remote_size(&r.url);
    if let Some(s) = remote { println!("     {}", dev::human(s)); }

    let out_dir = PathBuf::from(opt(args, "--out").unwrap_or_else(|| ".".into()));
    if !out_dir.is_dir() { die(&format!("no such directory: {}", out_dir.display())); }
    let dest = out_dir.join(&r.filename);

    // An image already here and already correct is not downloaded again.
    let mut have = false;
    if dest.is_file() {
        if let (Some(expect), Ok((got, len))) = (r.sha256.as_ref(), hash_file(&dest)) {
            if &got == expect { ok(&format!("already downloaded and verified: {} ({})", dest.display(), dev::human(len))); have = true; }
            else { no("a file of that name is here but its hash is wrong; downloading again"); }
        } else if remote.map(|s| dest.metadata().map(|m| m.len() == s).unwrap_or(false)).unwrap_or(false) {
            ok(&format!("already downloaded (full length): {}", dest.display())); have = true;
        }
    }
    if !have {
        step(&format!("downloading to {}", dest.display()));
        net::download(&r.url, &dest).unwrap_or_else(|e| die(&e));
    }

    step("checking what landed on disk");
    let (got, len) = hash_file(&dest).unwrap_or_else(|e| die(&format!("cannot read the download: {e}")));
    println!("     {} {}", dev::human(len), dest.display());
    match &r.sha256 {
        Some(expect) if expect == &got => ok(&format!("sha256 matches the project's published hash\n     {got}")),
        Some(expect) => die(&format!("DOWNLOAD IS NOT WHAT THE PROJECT PUBLISHED\n     published {expect}\n     downloaded {got}\n     delete it and try another mirror")),
        None => {
            ok(&format!("sha256 {got}"));
            println!("  {DIM}this project publishes no checksum next to the image; compare it with their site if you can{RST}");
        }
    }

    match opt(args, "--to") {
        Some(target) => { println!(); burn(&dest, &target, args, Some((got, len))); }
        None => {
            println!();
            println!("  {BOLD}arxburn write {} --to <device>{RST}   (arxburn list shows the devices)", dest.display());
        }
    }
}

// ---- hashing ------------------------------------------------------------------------------

/// Hash a file, streaming. Returns (hash, size).
fn hash_file(p: &Path) -> std::io::Result<(String, u64)> {
    let mut f = File::open(p)?;
    let mut h = sha256::Sha256::new();
    let mut buf = vec![0u8; BUF];
    let mut total = 0u64;
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 { break; }
        h.update(&buf[..n]);
        total += n as u64;
    }
    Ok((sha256::hex(&h.finish()), total))
}

/// Hash the first `len` bytes of a device.
fn hash_device_prefix(path: &Path, len: u64) -> std::io::Result<String> {
    let mut f = File::open(path)?;
    f.seek(SeekFrom::Start(0))?;
    let mut h = sha256::Sha256::new();
    let mut buf = vec![0u8; BUF];
    let mut left = len;
    while left > 0 {
        let want = std::cmp::min(left as usize, buf.len());
        f.read_exact(&mut buf[..want])?;
        h.update(&buf[..want]);
        left -= want as u64;
    }
    Ok(sha256::hex(&h.finish()))
}

fn progress(done: u64, total: u64, started: &Instant) {
    let secs = started.elapsed().as_secs_f64().max(0.001);
    let rate = done as f64 / secs;
    let pct = if total > 0 { done * 100 / total } else { 0 };
    let eta = if rate > 0.0 { ((total - done) as f64 / rate) as u64 } else { 0 };
    print!("\r  {:>3}%  {:>9} / {:<9}  {:>7}/s  eta {:02}:{:02}   ",
        pct, dev::human(done), dev::human(total), dev::human(rate as u64), eta / 60, eta % 60);
    let _ = std::io::stdout().flush();
}

// ---- burning ------------------------------------------------------------------------------

fn cmd_write(args: &[String]) {
    let image = match args.first() {
        Some(a) if !a.starts_with("--") => PathBuf::from(a),
        _ => die("usage: arxburn write <image> --to <device|UUID>"),
    };
    if !image.is_file() { die(&format!("no such image: {}", image.display())); }
    let target = opt(args, "--to").unwrap_or_else(|| die("--to <device|UUID> is required (see: arxburn list)"));
    burn(&image, &target, args, None);
}

fn burn(image: &Path, target: &str, args: &[String], known: Option<(String, u64)>) {
    let allow_internal = flag(args, "--allow-internal");
    let d = dev::resolve(target, flag(args, "--loop")).unwrap_or_else(|e| die(&e));

    step(&format!("image  {} ({})", image.display(), dev::human(image.metadata().map(|m| m.len()).unwrap_or(0))));
    step(&format!("target {}, {} {}, model {}", d.path.display(), dev::human(d.size),
        if d.removable { "removable" } else { "internal" }, d.model));

    if let Some(reason) = d.refusal(allow_internal) { die(&format!("refusing {}: {reason}", d.path.display())); }

    // Hash the image first: it costs one read, and it catches a truncated download BEFORE the
    // stick is erased rather than after.
    let (img_hash, img_len) = match known {
        Some(k) => k,
        None => {
            step("checking the image");
            let k = hash_file(image).unwrap_or_else(|e| die(&format!("cannot read image: {e}")));
            ok(&format!("sha256 {}", k.0));
            k
        }
    };
    if let Some(expect) = opt(args, "--expect") {
        if expect.trim().eq_ignore_ascii_case(&img_hash) { ok("matches --expect"); }
        else { die(&format!("image does NOT match --expect\n     expected {expect}\n     actual   {img_hash}")); }
    }
    if img_len > d.size {
        die(&format!("image is {} but the device holds {}", dev::human(img_len), dev::human(d.size)));
    }

    if std::env::var_os("ARXBURN_EUID0_OVERRIDE").is_none() && unsafe { libc_geteuid() } != 0 {
        die("writing to a block device needs root: run with sudo");
    }

    if !d.mounts.is_empty() {
        step(&format!("unmounting {}", d.mounts.join(", ")));
        for m in &d.mounts {
            match Command::new("umount").arg(m).status() {
                Ok(s) if s.success() => ok(&format!("unmounted {m}")),
                _ => die(&format!("could not unmount {m}: close anything using it and retry")),
            }
        }
    }

    if !flag(args, "--yes") {
        println!();
        println!("  {YEL}This ERASES {} completely.{RST}", d.path.display());
        print!("  Type the device name ({}) to continue: ", d.name);
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line).is_err() || line.trim() != d.name {
            die("not confirmed, nothing was written");
        }
    }

    step(&format!("writing {} to {}", dev::human(img_len), d.path.display()));
    let mut src = File::open(image).unwrap_or_else(|e| die(&format!("open image: {e}")));
    let mut dst = OpenOptions::new().write(true).open(&d.path)
        .unwrap_or_else(|e| die(&format!("open {}: {e}", d.path.display())));
    let mut buf = vec![0u8; BUF];
    let mut done = 0u64;
    let started = Instant::now();
    loop {
        let n = src.read(&mut buf).unwrap_or_else(|e| die(&format!("read image: {e}")));
        if n == 0 { break; }
        dst.write_all(&buf[..n]).unwrap_or_else(|e| die(&format!("write device: {e}")));
        done += n as u64;
        progress(done, img_len, &started);
    }
    println!();
    step("flushing to the device (this is where a slow stick actually writes)");
    dst.sync_all().unwrap_or_else(|e| die(&format!("sync: {e}")));
    drop(dst);
    ok(&format!("wrote {} in {:.0}s", dev::human(done), started.elapsed().as_secs_f64()));

    if flag(args, "--no-verify") {
        no("verification skipped (--no-verify): nothing proves the stick matches the image");
        return;
    }
    step("verifying: reading the bytes back off the device");
    drop_caches();
    let back = hash_device_prefix(&d.path, img_len).unwrap_or_else(|e| die(&format!("read back: {e}")));
    if back == img_hash {
        ok(&format!("VERIFIED: {} carries the image, byte for byte", d.path.display()));
        println!("  {DIM}sha256 {back}{RST}");
    } else {
        no(&format!("MISMATCH: do not boot this\n     image  {img_hash}\n     device {back}"));
        exit(1);
    }
}

fn cmd_verify(args: &[String]) {
    let image = match args.first() {
        Some(a) if !a.starts_with("--") => PathBuf::from(a),
        _ => die("usage: arxburn verify <image> --to <device|UUID>"),
    };
    let target = opt(args, "--to").unwrap_or_else(|| die("--to <device|UUID> is required"));
    let d = dev::resolve(&target, flag(args, "--loop")).unwrap_or_else(|e| die(&e));
    let (img_hash, img_len) = hash_file(&image).unwrap_or_else(|e| die(&format!("cannot read image: {e}")));
    step(&format!("image  sha256 {img_hash} ({})", dev::human(img_len)));
    if img_len > d.size {
        die(&format!("{} holds {} and the image is {}: this stick was never big enough for it",
            d.path.display(), dev::human(d.size), dev::human(img_len)));
    }
    drop_caches();
    let back = hash_device_prefix(&d.path, img_len).unwrap_or_else(|e| die(&format!("read back: {e}")));
    step(&format!("device sha256 {back}"));
    if back == img_hash { ok(&format!("{} matches the image", d.path.display())); }
    else { no(&format!("{} does NOT match the image", d.path.display())); exit(1); }
}

/// Ask the kernel to drop clean page cache before reading back. Without this a verify can be
/// answered from RAM: it would confirm what we just wrote, not what the stick stored.
fn drop_caches() {
    if let Ok(mut f) = OpenOptions::new().write(true).open("/proc/sys/vm/drop_caches") {
        let _ = f.write_all(b"1");
    }
}

extern "C" { fn geteuid() -> u32; }
unsafe fn libc_geteuid() -> u32 { geteuid() }
