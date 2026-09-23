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
mod fetch;
mod json;
mod dev;
mod net;
mod parts;
mod sha256;

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{exit, Command};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::sync_channel;
use std::time::{Duration, Instant};

/// Read and write in big blocks. A USB stick writes far faster in megabyte-sized chunks than in
/// the 512 byte ones dd defaults to, and the cost is only a few megabytes of RAM.
fn block_size() -> usize {
    std::env::var("ARXBURN_BLOCK").ok().and_then(|v| v.parse::<usize>().ok())
        .map(|m| m.clamp(64 * 1024, 64 * 1024 * 1024))
        .unwrap_or(8 * 1024 * 1024)
}

/// Machine readable output, for the GUI. Set once from the arguments.
static JSON: AtomicBool = AtomicBool::new(false);
fn json_mode() -> bool { JSON.load(Ordering::Relaxed) }

const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const RED: &str = "\x1b[31m";
const GRN: &str = "\x1b[32m";
const YEL: &str = "\x1b[33m";
const RST: &str = "\x1b[0m";

fn ok(msg: &str) { if json_mode() { json::line(&[("event", json::s("ok")), ("message", json::s(msg))]); } else { println!("  {GRN}ok{RST} {msg}"); } }
fn no(msg: &str) { if json_mode() { json::line(&[("event", json::s("error")), ("message", json::s(msg))]); } else { eprintln!("  {RED}!!{RST} {msg}"); } }
fn step(msg: &str) { if json_mode() { json::line(&[("event", json::s("step")), ("message", json::s(msg))]); } else { println!("{BOLD}>>{RST} {msg}"); } }

fn die(msg: &str) -> ! { no(msg); exit(1) }

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--json") { JSON.store(true, Ordering::Relaxed); }
    match args.get(1).map(String::as_str).unwrap_or("help") {
        "hash" => cmd_hash(&args[2..]),
        "inspect" | "scheme" => cmd_inspect(&args[2..]),
        "update" | "self-update" => cmd_update(&args[2..]),
        "net" | "networks" => {
            let n = fetch::networks();
            if json_mode() { json::line(&[("event", json::s("networks")), ("networks", json::V::Strs(n))]); }
            else if n.is_empty() { no("no network interface is up"); }
            else {
                println!("{BOLD}  downloads will use{RST}");
                for i in &n { println!("    {i}"); }
                println!("  {DIM}{}{RST}", fetch::describe(4));
            }
        }
        "list" | "ls" | "devices" => cmd_list(&args[2..]),
        "iso" | "images" | "catalog" => cmd_iso(&args[2..]),
        "get" | "fetch" | "download" => cmd_get(&args[2..]),
        "write" | "burn" | "w" => cmd_write(&args[2..]),
        "verify" | "check" => cmd_verify(&args[2..]),
        "--version" | "-V" => println!("arxburn {} ({} sha256, {} blocks)", env!("CARGO_PKG_VERSION"),
            sha256::engine(), dev::human(block_size() as u64)),
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
    println!("  arxburn inspect <image|device>            MBR, GPT, and what will boot it");
    println!("  arxburn update [--check]                  fetch the newest release and replace this");
    println!();
    println!("{BOLD}options{RST}");
    println!("  --to <dev|UUID>     target: sdc, /dev/sdc, or a partition UUID (UUID survives replugging)");
    println!("  --expect <sha256>   check the IMAGE against a known hash before writing anything");
    println!("  --out <dir>         where downloads land (default: the current directory)");
    println!("  --yes               skip the typed confirmation (scripts)");
    println!("  --no-verify         skip the read-back check (not advised; it is the point)");
    println!("  --allow-internal    permit a non-removable disk. The running system stays refused.");
    println!("  --scheme gpt|mbr    after verifying, leave the stick looking like one scheme");
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
    if json_mode() {
        let rows: Vec<Vec<(&'static str, json::V)>> = devices.iter().map(|d| vec![
            ("name", json::s(&d.name)),
            ("path", json::s(d.path.to_string_lossy())),
            ("size", json::V::N(d.size)),
            ("size_human", json::s(dev::human(d.size))),
            ("model", json::s(&d.model)),
            ("removable", json::V::B(d.removable)),
            ("holds_root", json::V::B(d.holds_root)),
            ("mounts", json::V::Strs(d.mounts.clone())),
            ("refusal", match d.refusal(false) { Some(r) => json::s(r), None => json::V::Null }),
            ("refusal_internal_allowed", match d.refusal(true) { Some(r) => json::s(r), None => json::V::Null }),
        ]).collect();
        json::line(&[("event", json::s("devices")), ("devices", json::V::Arr(rows))]);
        return;
    }
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
            if json_mode() {
                match catalog::resolve(iso) {
                    Ok(r) => json::line(&[
                        ("event", json::s("image")), ("id", json::s(&iso.id)), ("name", json::s(&iso.name)),
                        ("filename", json::s(&r.filename)), ("url", json::s(&r.url)),
                        ("size", match net::remote_size(&r.url) { Some(n) => json::V::N(n), None => json::V::Null }),
                        ("sha256", match &r.sha256 { Some(h) => json::s(h), None => json::V::Null }),
                    ]),
                    Err(e) => { no(&e); exit(1); }
                }
                return;
            }
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

    if json_mode() {
        let rows: Vec<Vec<(&'static str, json::V)>> = list.iter()
            .filter(|i| !(only_win && i.family != catalog::Family::Windows))
            .filter(|i| !(only_lin && i.family != catalog::Family::Linux))
            .filter(|i| !(only_tool && i.family != catalog::Family::Tool))
            .map(|i| vec![
                ("id", json::s(&i.id)), ("name", json::s(&i.name)),
                ("family", json::s(i.family.label())), ("note", json::s(&i.note)),
            ]).collect();
        json::line(&[("event", json::s("images")), ("images", json::V::Arr(rows))]);
        return;
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
    if !json_mode() { println!("     {}", r.url); }
    // A HEAD against a mirror can take seconds, and saying nothing through it is exactly what a
    // dead progress bar looks like from the outside.
    step("asking the mirror how big it is");
    // One probe, reused for the size, the already-downloaded check and the decision about
    // parallel connections. It used to be two requests to the same slow redirect.
    let probe = fetch::probe(&r.url);
    let remote = if probe.size > 0 { Some(probe.size) } else { None };
    match remote {
        Some(n) if !json_mode() => println!("     {}", dev::human(n)),
        Some(n) => json::line(&[("event", json::s("size")), ("bytes", json::V::N(n))]),
        // Not fatal, and not the end of the progress bar either: the download reports its own
        // size once it starts.
        None => step("the mirror did not say; the download will report its own size"),
    }

    let out_dir = PathBuf::from(opt(args, "--out").unwrap_or_else(|| ".".into()));
    if !out_dir.is_dir() { die(&format!("no such directory: {}", out_dir.display())); }
    let dest = out_dir.join(&r.filename);

    // An image already here and already correct is not downloaded again.
    let mut have = false;
    if dest.is_file() {
        if let (Some(expect), Ok((got, len))) = (r.sha256.as_ref(), hash_file(&dest, None)) {
            if &got == expect { ok(&format!("already downloaded and verified: {} ({})", dest.display(), dev::human(len))); have = true; }
            else { no("a file of that name is here but its hash is wrong; downloading again"); }
        } else if remote.map(|s| dest.metadata().map(|m| m.len() == s).unwrap_or(false)).unwrap_or(false) {
            ok(&format!("already downloaded (full length): {}", dest.display())); have = true;
        }
    }
    if !have {
        step(&format!("downloading to {}", dest.display()));
        if json_mode() {
            json::line(&[("event", json::s("download")), ("url", json::s(&r.url)),
                         ("dest", json::s(dest.to_string_lossy())),
                         ("size", match remote { Some(n) => json::V::N(n), None => json::V::Null })]);
        }
        download_parallel_or_single(&r.url, &dest, &probe, args);
    }

    step("checking what landed on disk");
    let (got, len) = hash_file(&dest, Some("hash")).unwrap_or_else(|e| die(&format!("cannot read the download: {e}")));
    if !json_mode() { println!("     {} {}", dev::human(len), dest.display()); }
    match &r.sha256 {
        Some(expect) if expect == &got => ok(&format!("sha256 matches the project's published hash\n     {got}")),
        Some(expect) => die(&format!("DOWNLOAD IS NOT WHAT THE PROJECT PUBLISHED\n     published {expect}\n     downloaded {got}\n     delete it and try another mirror")),
        None => {
            ok(&format!("sha256 {got}"));
            if !json_mode() {
                println!("  {DIM}this project publishes no checksum next to the image; compare it with their site if you can{RST}");
            }
        }
    }

    match opt(args, "--to") {
        Some(target) => { if !json_mode() { println!(); } burn(&dest, &target, args, Some((got, len))); }
        None if json_mode() => json::line(&[("event", json::s("ready")),
                                            ("path", json::s(dest.to_string_lossy())),
                                            ("bytes", json::V::N(len))]),
        None => {
            println!();
            println!("  {BOLD}arxburn write {} --to <device>{RST}   (arxburn list shows the devices)", dest.display());
        }
    }
}

/// Download with a live counter. curl draws its own bar on a terminal, but the GUI needs bytes
/// on the pipe, and either way the file on disk is the honest counter: it is what a resume would
/// pick up from.
/// Pull the file over as many connections as the machine and the mirror allow, falling back to
/// one connection when ranges are not on offer.
fn download_parallel_or_single(url: &str, dest: &Path, p: &fetch::Probe, args: &[String]) {
    let streams = opt(args, "--streams").and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(4).clamp(1, 16);
    if !flag(args, "--single") {
        let size = p.size;
        // below a few tens of megabytes the extra connections cost more than they save
        if p.ranges && size > 32 * 1024 * 1024 {
            step(&format!("fetching over {}", fetch::describe(streams)));
            if json_mode() {
                json::line(&[("event", json::s("engine")), ("connections", json::V::N((streams * fetch::networks().len().max(1)) as u64)),
                             ("networks", json::V::Strs(fetch::networks()))]);
            }
            let mut meter = Meter::new("download", size);
            let opts = fetch::Opts { streams_per_network: streams, ..Default::default() };
            match fetch::parallel(url, dest, size, &opts, |done| meter.tick(done)) {
                Ok(()) => { meter.finish(size); return; }
                Err(e) => no(&format!("{e}\n     falling back to a single connection")),
            }
        }
    }
    download_single(url, dest, p.size);
}

fn download_single(url: &str, dest: &Path, total: u64) {
    // curl writes the real response headers here, so that even when the size probe was refused or
    // timed out the bar gets a denominator a second into the transfer instead of never.
    let hdr = dest.with_extension("arxburn-headers");
    let _ = std::fs::remove_file(&hdr);
    let (u, d, h) = (url.to_string(), dest.to_path_buf(), hdr.clone());
    let handle = std::thread::spawn(move || net::download(&u, &d, Some(&h)));
    let mut meter = Meter::new("download", total);
    // a resumed download's headers describe the REMAINING bytes, so what is already on disk
    // counts towards the total
    let already = if total == 0 { dest.metadata().map(|m| m.len()).unwrap_or(0) } else { 0 };
    while !handle.is_finished() {
        if meter.total == 0 {
            if let Ok(head) = std::fs::read_to_string(&hdr) {
                if let Some(n) = net::size_in_headers(&head) { meter.set_total(n + already); }
            }
        }
        let n = dest.metadata().map(|m| m.len()).unwrap_or(0);
        meter.tick(n);
        std::thread::sleep(Duration::from_millis(150));
    }
    let _ = std::fs::remove_file(&hdr);
    match handle.join() {
        Ok(Ok(())) => {}
        Ok(Err(e)) => die(&e),
        Err(_) => die("the download thread stopped unexpectedly"),
    }
    meter.finish(dest.metadata().map(|m| m.len()).unwrap_or(0));
}

// ---- hashing and progress -----------------------------------------------------------------

/// Tell the kernel we are going to read this straight through, so it reads ahead properly
/// instead of treating a 4GB sequential pass as random access.
fn sequential(f: &File) {
    use std::os::unix::io::AsRawFd;
    const POSIX_FADV_SEQUENTIAL: i32 = 2;
    // SAFETY: a plain advisory call on a file descriptor we own; it cannot fail destructively.
    unsafe { posix_fadvise(f.as_raw_fd(), 0, 0, POSIX_FADV_SEQUENTIAL) };
}
extern "C" { fn posix_fadvise(fd: i32, offset: i64, len: i64, advice: i32) -> i32; }

/// The live byte counter. Redraws at most twelve times a second, because a terminal that is
/// repainting is not writing to your stick, and reports exact byte counts rather than a
/// rounded percentage: on a 4GB image a percent is 42 megabytes wide.
struct Meter {
    phase: &'static str,
    total: u64,
    started: Instant,
    last_draw: Instant,
    last_done: u64,
    rate: f64,
}

impl Meter {
    fn new(phase: &'static str, total: u64) -> Meter {
        let now = Instant::now();
        Meter { phase, total, started: now, last_draw: now, last_done: 0, rate: 0.0 }
    }

    /// The size arrived late, from the transfer itself rather than from a probe.
    fn set_total(&mut self, total: u64) { self.total = total; }

    fn tick(&mut self, done: u64) {
        let now = Instant::now();
        if now.duration_since(self.last_draw) < Duration::from_millis(80) && done < self.total { return; }
        let dt = now.duration_since(self.last_draw).as_secs_f64();
        if dt > 0.0 {
            let instant = (done.saturating_sub(self.last_done)) as f64 / dt;
            // a little smoothing, or the number is unreadable when the stick's cache fills
            self.rate = if self.rate == 0.0 { instant } else { self.rate * 0.6 + instant * 0.4 };
        }
        self.last_draw = now;
        self.last_done = done;
        self.draw(done);
    }

    fn draw(&self, done: u64) {
        let eta = if self.rate > 1.0 { ((self.total.saturating_sub(done)) as f64 / self.rate) as u64 } else { 0 };
        if json_mode() {
            json::line(&[
                ("event", json::s("progress")), ("phase", json::s(self.phase)),
                ("done", json::V::N(done)), ("total", json::V::N(self.total)),
                ("bytes_per_second", json::V::N(self.rate as u64)), ("eta_seconds", json::V::N(eta)),
                ("elapsed_ms", json::V::N(self.started.elapsed().as_millis() as u64)),
            ]);
            return;
        }
        const SEGS: usize = 24;
        if self.total == 0 {
            // the server never said how big it is: show what has arrived, not a bar that cannot move
            let spin = ['\u{25B0}', '\u{25B1}'];
            let phase = (self.started.elapsed().as_millis() / 120) as usize;
            let bar: String = (0..SEGS).map(|i| spin[(i + phase) % 2]).collect();
            print!("\r  {YEL}{bar}{RST}  {:>13} bytes  {:>9}/s  (total unknown)   ",
                with_commas(done), dev::human(self.rate as u64));
            let _ = std::io::stdout().flush();
            return;
        }
        let filled = (done as u128 * SEGS as u128 / self.total as u128) as usize;
        let bar: String = (0..SEGS).map(|i| if i < filled { '\u{25B0}' } else { '\u{25B1}' }).collect();
        let pct = done * 100 / self.total;
        print!("\r  {YEL}{bar}{RST} {pct:>3}%  {:>13} / {:<13} {:>9}/s  eta {:02}:{:02}  ",
            with_commas(done), with_commas(self.total), dev::human(self.rate as u64), eta / 60, eta % 60);
        let _ = std::io::stdout().flush();
    }

    fn finish(&mut self, done: u64) {
        self.last_draw = Instant::now() - Duration::from_secs(1);
        self.tick(done);
        if !json_mode() { println!(); }
    }
}

/// Exact bytes, grouped, because "3.9 GB" hides the last 40 megabytes and this is the number
/// someone watches to know the thing is actually moving.
fn with_commas(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 { out.push(','); }
        out.push(c);
    }
    out
}

/// Hash a file, streaming, with a live counter when a phase is named.
fn hash_file(p: &Path, phase: Option<&'static str>) -> std::io::Result<(String, u64)> {
    let mut f = File::open(p)?;
    sequential(&f);
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let mut meter = phase.map(|ph| Meter::new(ph, len));
    let mut h = sha256::Sha256::new();
    let mut buf = vec![0u8; block_size()];
    let mut total = 0u64;
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 { break; }
        h.update(&buf[..n]);
        total += n as u64;
        if let Some(m) = meter.as_mut() { m.tick(total); }
    }
    if let Some(m) = meter.as_mut() { m.finish(total); }
    Ok((sha256::hex(&h.finish()), total))
}

/// Hash the first `len` bytes of a device.
fn hash_device_prefix(path: &Path, len: u64, phase: Option<&'static str>) -> std::io::Result<String> {
    let mut f = File::open(path)?;
    sequential(&f);
    f.seek(SeekFrom::Start(0))?;
    let mut meter = phase.map(|ph| Meter::new(ph, len));
    let mut h = sha256::Sha256::new();
    let mut buf = vec![0u8; block_size()];
    let mut left = len;
    while left > 0 {
        let want = std::cmp::min(left as usize, buf.len());
        f.read_exact(&mut buf[..want])?;
        h.update(&buf[..want]);
        left -= want as u64;
        if let Some(m) = meter.as_mut() { m.tick(len - left); }
    }
    if let Some(m) = meter.as_mut() { m.finish(len); }
    Ok(sha256::hex(&h.finish()))
}

/// Read the first sectors of an image or a device and say what a firmware will make of it.
fn head_of(path: &Path) -> std::io::Result<Vec<u8>> {
    let mut f = File::open(path)?;
    let mut buf = vec![0u8; 36 * 1024];
    let mut filled = 0;
    while filled < buf.len() {
        match f.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) => return Err(e),
        }
    }
    buf.truncate(filled);
    Ok(buf)
}

fn cmd_inspect(args: &[String]) {
    let target = match args.first() {
        Some(a) if !a.starts_with("--") => a.clone(),
        _ => die("usage: arxburn inspect <image|device>"),
    };
    // a bare device name means the stick, not a file in the current directory
    let path = if Path::new(&target).is_file() {
        PathBuf::from(&target)
    } else {
        dev::resolve(&target, flag(args, "--loop")).map(|d| d.path)
            .unwrap_or_else(|_| PathBuf::from(&target))
    };
    let head = head_of(&path).unwrap_or_else(|e| die(&format!("cannot read {}: {e}", path.display())));
    let s = parts::read(&head);
    if json_mode() {
        let rows: Vec<Vec<(&'static str, json::V)>> = s.entries.iter().map(|e| vec![
            ("type", json::V::N(e.kind as u64)),
            ("type_name", json::s(e.kind_name())),
            ("bootable", json::V::B(e.bootable)),
            ("start_lba", json::V::N(e.start_lba as u64)),
            ("sectors", json::V::N(e.sectors as u64)),
        ]).collect();
        json::line(&[("event", json::s("scheme")), ("path", json::s(path.to_string_lossy())),
                     ("mbr", json::V::B(s.mbr)), ("gpt", json::V::B(s.gpt)),
                     ("el_torito", json::V::B(s.el_torito)),
                     ("efi_partition", json::V::B(s.efi_partition)),
                     ("summary", json::s(s.summary())), ("boots", json::s(s.boots())),
                     ("partitions", json::V::Arr(rows))]);
        return;
    }
    step(&format!("{}", path.display()));
    ok(&format!("{}", s.summary()));
    println!("     {}", s.boots());
    for (i, e) in s.entries.iter().enumerate() {
        println!("  {DIM}{}{RST} {:<16} {:>12} sectors at LBA {:<10}{}", i + 1, e.kind_name(),
            with_commas(e.sectors as u64), e.start_lba,
            if e.bootable { format!(" {YEL}bootable{RST}") } else { String::new() });
    }
}

const REPO: &str = "0xb0rn3/arxburn";

/// Newest published version, or None when the release cannot be read.
fn latest_release() -> Option<String> {
    let body = net::text(&format!("https://api.github.com/repos/{REPO}/releases/latest")).ok()?;
    let tag = body.split("\"tag_name\":").nth(1)?.split('"').nth(1)?.to_string();
    Some(tag.trim_start_matches('v').to_string())
}

/// Compare two dotted versions numerically, so 0.10.0 is newer than 0.9.0.
fn newer(a: &str, b: &str) -> bool {
    let parts = |v: &str| v.split('.').map(|x| x.parse::<u64>().unwrap_or(0)).collect::<Vec<_>>();
    let (x, y) = (parts(a), parts(b));
    for i in 0..x.len().max(y.len()) {
        let (l, r) = (*x.get(i).unwrap_or(&0), *y.get(i).unwrap_or(&0));
        if l != r { return l > r; }
    }
    false
}

fn cmd_update(args: &[String]) {
    let here = env!("CARGO_PKG_VERSION");
    let latest = match latest_release() {
        Some(v) => v,
        None => {
            if json_mode() {
                json::line(&[("event", json::s("update")), ("error", json::s("cannot reach the release feed"))]);
                return;
            }
            die("cannot reach the release feed");
        }
    };
    let available = newer(&latest, here);
    if json_mode() {
        json::line(&[("event", json::s("update")), ("current", json::s(here)),
                     ("latest", json::s(&latest)), ("available", json::V::B(available))]);
    } else if available {
        ok(&format!("{latest} is available (you have {here})"));
    } else {
        ok(&format!("{here} is the newest release"));
    }
    if flag(args, "--check") || !available { return; }

    // replace the binaries in place, but only after each download matches its published hash
    let exe = std::env::current_exe().unwrap_or_else(|e| die(&format!("cannot find myself: {e}")));
    let dir = exe.parent().unwrap_or(Path::new("/usr/bin")).to_path_buf();
    // What replacing a binary actually needs is write permission on its DIRECTORY, because the
    // swap is a rename, not a write. Testing it by opening the binary itself could never work:
    // the kernel answers ETXTBSY, "text file busy", for a write-open of a running executable, and
    // it answers that to root as well. So the old check failed for everybody and then blamed
    // permissions, which is why running it under sudo changed nothing.
    let probe = dir.join(".arxburn-update-probe");
    match std::fs::File::create(&probe) {
        Ok(_) => { let _ = std::fs::remove_file(&probe); }
        Err(e) => die(&format!("cannot write to {}: {e}\n     run it with sudo", dir.display())),
    }
    let base = format!("https://github.com/{REPO}/releases/latest/download");
    let sums = net::text(&format!("{base}/SHA256SUMS")).unwrap_or_else(|e| die(&e));

    for (asset, dest) in [("arxburn-x86_64-linux", exe.clone()),
                          ("arxburn-gui-x86_64-linux", dir.join("arxburn-gui"))] {
        if !dest.exists() { continue; }          // only replace what is actually installed
        let want = sums.lines().find(|l| l.ends_with(asset))
            .and_then(|l| l.split_whitespace().next()).map(str::to_string);
        let Some(want) = want else { no(&format!("no published hash for {asset}, skipped")); continue };
        let tmp = dest.with_extension("new");
        step(&format!("downloading {asset}"));
        if let Err(e) = net::download(&format!("{base}/{asset}"), &tmp, None) { let _ = std::fs::remove_file(&tmp); die(&e); }
        let (got, _) = hash_file(&tmp, None).unwrap_or_else(|e| die(&format!("cannot read the download: {e}")));
        if got != want {
            let _ = std::fs::remove_file(&tmp);
            die(&format!("{asset} does not match its published sha256; nothing was replaced"));
        }
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755));
        // rename over the old one: atomic on the same filesystem, so there is never a moment
        // where the tool is half replaced
        std::fs::rename(&tmp, &dest).unwrap_or_else(|e| die(&format!("cannot replace {}: {e}", dest.display())));
        ok(&format!("{} updated", dest.display()));
    }
}

fn cmd_hash(args: &[String]) {
    let p = match args.first() {
        Some(a) if !a.starts_with("--") => PathBuf::from(a),
        _ => die("usage: arxburn hash <file> [--expect <sha256>]"),
    };
    // Always metered, including under --json. Hashing a 4GB image takes the better part of a
    // minute, and a window told nothing for a minute is a window that looks broken.
    let (h, n) = hash_file(&p, Some("hash"))
        .unwrap_or_else(|e| die(&format!("cannot read {}: {e}", p.display())));

    // What somebody actually wants to know: does this file match the hash the project published?
    let expect = opt(args, "--expect").map(|e| e.trim().to_ascii_lowercase());
    let verdict = expect.as_ref().map(|e| e == &h);

    if json_mode() {
        let mut f: Vec<(&str, json::V)> = vec![
            ("event", json::s("hash")), ("sha256", json::s(&h)), ("size", json::V::N(n)),
            ("engine", json::s(sha256::engine())), ("path", json::s(p.to_string_lossy())),
        ];
        if let Some(e) = &expect { f.push(("expected", json::s(e))); }
        if let Some(v) = verdict { f.push(("matches", json::V::B(v))); }
        json::line(&f);
    } else {
        println!("  {h}  {}  ({} bytes, {})", p.display(), with_commas(n), sha256::engine());
    }
    match verdict {
        Some(true) => ok("matches the hash you gave"),
        Some(false) => {
            no(&format!("DOES NOT MATCH\n     you gave  {}\n     this file {h}", expect.unwrap_or_default()));
            exit(1);
        }
        None => {}
    }
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
    let img_len_meta = image.metadata().map(|m| m.len()).unwrap_or(0);

    step(&format!("image  {} ({})", image.display(), dev::human(img_len_meta)));
    step(&format!("target {}, {} {}, model {}", d.path.display(), dev::human(d.size),
        if d.removable { "removable" } else { "internal" }, d.model));
    if json_mode() {
        json::line(&[("event", json::s("target")), ("device", json::s(d.path.to_string_lossy())),
                     ("size", json::V::N(d.size)), ("image_size", json::V::N(img_len_meta))]);
    }

    if let Ok(head) = head_of(image) {
        let sc = parts::read(&head);
        step(&format!("image is {}: {}", sc.summary(), sc.boots()));
        if json_mode() {
            json::line(&[("event", json::s("image_scheme")), ("summary", json::s(sc.summary())),
                         ("boots", json::s(sc.boots())), ("mbr", json::V::B(sc.mbr)),
                         ("gpt", json::V::B(sc.gpt))]);
        }
    }

    if let Some(reason) = d.refusal(allow_internal) { die(&format!("refusing {}: {reason}", d.path.display())); }
    if img_len_meta > d.size {
        die(&format!("image is {} but the device holds {}", dev::human(img_len_meta), dev::human(d.size)));
    }

    // Hashing the image up front costs a whole extra pass over it. It buys one thing: catching a
    // truncated download BEFORE the stick is erased. So it happens when the answer can actually
    // change the decision (--expect), and otherwise the hash is taken during the write itself,
    // where it is free. A hash already known from `arxburn get` is reused either way.
    let expect = opt(args, "--expect");
    let pre_hash = match (&known, &expect) {
        (Some(k), _) => Some(k.clone()),
        (None, Some(_)) => {
            step("checking the image before anything is erased");
            let k = hash_file(image, Some("hash")).unwrap_or_else(|e| die(&format!("cannot read image: {e}")));
            ok(&format!("sha256 {}", k.0));
            Some(k)
        }
        _ => None,
    };
    if let (Some(e), Some((got, _))) = (&expect, &pre_hash) {
        if e.trim().eq_ignore_ascii_case(got) { ok("matches --expect"); }
        else { die(&format!("image does NOT match --expect\n     expected {e}\n     actual   {got}")); }
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

    step(&format!("writing {} to {}", dev::human(img_len_meta), d.path.display()));
    let (img_hash, written, elapsed) = write_pipelined(image, &d.path, img_len_meta);
    if let Some((k, _)) = &pre_hash {
        if k != &img_hash {
            die("the image changed while it was being written; nothing about this stick can be trusted");
        }
    }
    let secs = elapsed.as_secs_f64().max(0.001);
    ok(&format!("wrote {} in {:.1}s ({}/s)", with_commas(written), secs, dev::human((written as f64 / secs) as u64)));

    if flag(args, "--no-verify") {
        no("verification skipped (--no-verify): nothing proves the stick matches the image");
        return;
    }
    let scheme_choice = opt(args, "--scheme");
    step("verifying: reading the bytes back off the device");
    drop_caches();
    let back = hash_device_prefix(&d.path, written, Some("verify"))
        .unwrap_or_else(|e| die(&format!("read back: {e}")));
    if back == img_hash {
        ok(&format!("VERIFIED: {} carries the image, byte for byte", d.path.display()));
        if json_mode() {
            json::line(&[("event", json::s("done")), ("verified", json::V::B(true)),
                         ("sha256", json::s(&back)), ("bytes", json::V::N(written))]);
        } else {
            println!("  {DIM}sha256 {back}{RST}");
        }
        // only now, with the copy proved, is the partition table deliberately changed
        if let Some(want) = scheme_choice {
            apply_scheme(&d, &want);
        }
    } else {
        no(&format!("MISMATCH: do not boot this\n     image  {img_hash}\n     device {back}"));
        if json_mode() {
            json::line(&[("event", json::s("done")), ("verified", json::V::B(false)),
                         ("sha256", json::s(&back)), ("expected", json::s(&img_hash))]);
        }
        exit(1);
    }
}

/// Read, hash and write at the same time.
///
/// Reading the image, hashing it and writing it to the stick are three jobs that each wait on
/// something different, so doing them in sequence means two of the three are always idle. The
/// writer runs on its own thread and the read plus hash feed it through a small queue, so the
/// stick is kept busy continuously and the hash comes out of the same pass. The progress the
/// display shows is the writer's own counter: bytes actually handed to the device, never the
/// bytes we have merely queued.
/// Bytes the kernel has accepted but not yet handed to any device, from /proc/meminfo.
///
/// Dirty is waiting to be written back, Writeback is in flight. Together they are what is still
/// owed to the hardware, and during a flush the number only falls, which is what makes it usable
/// as a countdown. The values are in kB and the suffix is on the line, so it is parsed by
/// position rather than trusted.
fn unflushed(meminfo: &str) -> u64 {
    meminfo.lines()
        .filter(|l| l.starts_with("Dirty:") || l.starts_with("Writeback:"))
        .filter_map(|l| l.split_whitespace().nth(1))
        .filter_map(|v| v.parse::<u64>().ok())
        .map(|kb| kb * 1024)
        .sum()
}

fn write_pipelined(image: &Path, device: &Path, total: u64) -> (String, u64, Duration) {
    use std::sync::atomic::AtomicU64;
    use std::sync::Arc;

    let mut src = File::open(image).unwrap_or_else(|e| die(&format!("open image: {e}")));
    sequential(&src);
    let mut dst = OpenOptions::new().write(true).open(device)
        .unwrap_or_else(|e| die(&format!("open {}: {e}", device.display())));

    let written = Arc::new(AtomicU64::new(0));
    let counter = Arc::clone(&written);
    // a shallow queue: deep enough that the reader never stalls the writer, shallow enough that
    // the progress shown is close to what the device has really taken
    let (tx, rx) = sync_channel::<Vec<u8>>(3);
    let started = Instant::now();

    let writer = std::thread::spawn(move || -> std::io::Result<()> {
        for buf in rx {
            dst.write_all(&buf)?;
            counter.fetch_add(buf.len() as u64, Ordering::Relaxed);
        }
        dst.sync_all()?;
        Ok(())
    });

    let mut hasher = sha256::Sha256::new();
    let mut meter = Meter::new("write", total);
    let bs = block_size();
    let mut read_total = 0u64;
    loop {
        let mut buf = vec![0u8; bs];
        let mut filled = 0;
        while filled < bs {
            match src.read(&mut buf[filled..]) {
                Ok(0) => break,
                Ok(n) => filled += n,
                Err(e) => die(&format!("read image: {e}")),
            }
        }
        if filled == 0 { break; }
        buf.truncate(filled);
        hasher.update(&buf);
        read_total += filled as u64;
        if tx.send(buf).is_err() { break; } // the writer died; its error is reported below
        meter.tick(written.load(Ordering::Relaxed));
    }
    drop(tx);
    // the queue drains and the device is flushed here: on a slow stick this is most of the wait,
    // so the counter keeps moving instead of freezing at 100%
    loop {
        let w = written.load(Ordering::Relaxed);
        meter.tick(w);
        if w >= read_total { break; }
        std::thread::sleep(Duration::from_millis(60));
    }
    // Everything the image contains has now been handed to the kernel, so the counter reads 100%.
    // The stick has NOT got it yet: most of it is sitting in the page cache, and sync_all below is
    // where a slow stick really writes, for minutes. A bar frozen at 100% with nothing else moving
    // is read as "finished", and a stick pulled at that moment is a corrupt stick. So the flush
    // reports its own progress, counted down from what the kernel still owes the device.
    step("flushing to the device: this is where a slow stick actually writes, and it is STILL WRITING");
    step("do not unplug it until this says verified");
    let flushing = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let watch = Arc::clone(&flushing);
    let watcher = std::thread::spawn(move || {
        // what the kernel has not yet handed to any device: it only falls, so it is an honest
        // measure of how much of this wait is left
        let outstanding = || -> u64 {
            std::fs::read_to_string("/proc/meminfo").map(|mi| unflushed(&mi)).unwrap_or(0)
        };
        let peak = outstanding().max(1);
        let mut m = Meter::new("flush", peak);
        while watch.load(Ordering::Relaxed) {
            let left = outstanding();
            m.tick(peak.saturating_sub(left.min(peak)));
            std::thread::sleep(Duration::from_millis(250));
        }
        m.finish(peak);
    });
    let joined = writer.join();
    flushing.store(false, Ordering::Relaxed);
    let _ = watcher.join();
    match joined {
        Ok(Ok(())) => {}
        Ok(Err(e)) => die(&format!("write device: {e}")),
        Err(_) => die("the writing thread stopped unexpectedly; the stick is not complete"),
    }
    meter.finish(written.load(Ordering::Relaxed));
    (sha256::hex(&hasher.finish()), read_total, started.elapsed())
}

/// Leave the stick looking like one scheme or the other.
///
/// This runs AFTER the read back verification, never before: the point of the verification is
/// that the stick matches the image byte for byte, and this changes bytes on purpose. Doing it
/// in the other order would make the proof meaningless.
fn apply_scheme(d: &dev::Device, want: &str) {
    let head = head_of(&d.path).unwrap_or_else(|e| die(&format!("cannot read back {}: {e}", d.path.display())));
    let s = parts::read(&head);
    let sectors = d.size / parts::SECTOR as u64;
    let mut f = OpenOptions::new().write(true).open(&d.path)
        .unwrap_or_else(|e| die(&format!("open {}: {e}", d.path.display())));
    match want {
        "mbr" => {
            if !s.gpt {
                ok("already MBR only; nothing to change");
                return;
            }
            // clear the primary GPT header and its backup at the last sector: the MBR that the
            // image wrote stays exactly as it is, so BIOS booting is untouched
            let zero = vec![0u8; parts::SECTOR];
            f.seek(SeekFrom::Start(parts::SECTOR as u64)).and_then(|_| f.write_all(&zero))
                .unwrap_or_else(|e| die(&format!("cannot clear the GPT header: {e}")));
            if sectors > 1 {
                let last = (sectors - 1) * parts::SECTOR as u64;
                let _ = f.seek(SeekFrom::Start(last)).and_then(|_| f.write_all(&zero));
            }
            f.sync_all().ok();
            ok("scheme: MBR (the GPT headers were cleared; BIOS boot is untouched)");
        }
        "gpt" => {
            if !s.gpt {
                die("this image has no GPT, so there is nothing to keep: writing a protective MBR \
                     would leave a stick that no firmware can boot. Leave it as it is, or write an \
                     image that carries a GPT.");
            }
            let pm = parts::protective_mbr(sectors);
            f.seek(SeekFrom::Start(0)).and_then(|_| f.write_all(&pm))
                .unwrap_or_else(|e| die(&format!("cannot write the protective MBR: {e}")));
            f.sync_all().ok();
            ok("scheme: GPT (sector 0 is now a protective MBR; UEFI boot is untouched)");
        }
        "auto" | "hybrid" => ok(&format!("scheme: left as the image wrote it ({})", s.summary())),
        other => die(&format!("unknown scheme '{other}': use gpt, mbr or auto")),
    }
}

fn cmd_verify(args: &[String]) {
    let image = match args.first() {
        Some(a) if !a.starts_with("--") => PathBuf::from(a),
        _ => die("usage: arxburn verify <image> --to <device|UUID>"),
    };
    let target = opt(args, "--to").unwrap_or_else(|| die("--to <device|UUID> is required"));
    let d = dev::resolve(&target, flag(args, "--loop")).unwrap_or_else(|e| die(&e));
    let (img_hash, img_len) = hash_file(&image, Some("hash")).unwrap_or_else(|e| die(&format!("cannot read image: {e}")));
    step(&format!("image  sha256 {img_hash} ({})", dev::human(img_len)));
    if img_len > d.size {
        die(&format!("{} holds {} and the image is {}: this stick was never big enough for it",
            d.path.display(), dev::human(d.size), dev::human(img_len)));
    }
    drop_caches();
    let back = hash_device_prefix(&d.path, img_len, Some("verify")).unwrap_or_else(|e| die(&format!("read back: {e}")));
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

#[cfg(test)]
mod flush_tests {
    use super::unflushed;

    const SAMPLE: &str = "MemTotal:       16316772 kB\n\
                          Cached:          4212344 kB\n\
                          Dirty:           1048576 kB\n\
                          Writeback:         65536 kB\n\
                          WritebackTmp:          0 kB\n";

    #[test]
    fn what_the_kernel_still_owes_the_device_is_dirty_plus_writeback() {
        // 1048576 kB + 65536 kB, in bytes
        assert_eq!(unflushed(SAMPLE), (1_048_576 + 65_536) * 1024);
    }

    #[test]
    fn writeback_tmp_is_not_counted_as_outstanding() {
        // it starts with "Writeback" but it is tmpfs accounting, not bytes owed to a disk;
        // counting it would make the flush countdown stall short of zero
        let one = "Dirty:  0 kB\nWritebackTmp:  999999 kB\n";
        assert_eq!(unflushed(one), 0);
    }

    #[test]
    fn a_clean_system_owes_nothing_and_unreadable_input_is_not_a_panic() {
        assert_eq!(unflushed("Dirty:   0 kB\nWriteback:   0 kB\n"), 0);
        assert_eq!(unflushed(""), 0);
        assert_eq!(unflushed("Dirty: notanumber kB\n"), 0);
    }
}
