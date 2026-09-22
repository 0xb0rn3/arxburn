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
    println!("     {}", r.url);
    let remote = net::remote_size(&r.url);
    if let Some(s) = remote { println!("     {}", dev::human(s)); }

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
        download_parallel_or_single(&r.url, &dest, remote.unwrap_or(0), args);
    }

    step("checking what landed on disk");
    let (got, len) = hash_file(&dest, Some("hash")).unwrap_or_else(|e| die(&format!("cannot read the download: {e}")));
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

/// Download with a live counter. curl draws its own bar on a terminal, but the GUI needs bytes
/// on the pipe, and either way the file on disk is the honest counter: it is what a resume would
/// pick up from.
/// Pull the file over as many connections as the machine and the mirror allow, falling back to
/// one connection when ranges are not on offer.
fn download_parallel_or_single(url: &str, dest: &Path, total: u64, args: &[String]) {
    let streams = opt(args, "--streams").and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(4).clamp(1, 16);
    if !flag(args, "--single") {
        let p = fetch::probe(url);
        let size = if p.size > 0 { p.size } else { total };
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
    download_single(url, dest, total);
}

fn download_single(url: &str, dest: &Path, total: u64) {
    let (u, d) = (url.to_string(), dest.to_path_buf());
    let handle = std::thread::spawn(move || net::download(&u, &d));
    let mut meter = Meter::new("download", total);
    while !handle.is_finished() {
        let n = dest.metadata().map(|m| m.len()).unwrap_or(0);
        meter.tick(n);
        std::thread::sleep(Duration::from_millis(150));
    }
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
        let filled = if self.total > 0 { (done as u128 * SEGS as u128 / self.total as u128) as usize } else { 0 };
        let bar: String = (0..SEGS).map(|i| if i < filled { '\u{25B0}' } else { '\u{25B1}' }).collect();
        let pct = if self.total > 0 { done * 100 / self.total } else { 0 };
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

fn cmd_hash(args: &[String]) {
    let p = match args.first() {
        Some(a) if !a.starts_with("--") => PathBuf::from(a),
        _ => die("usage: arxburn hash <file>"),
    };
    let quiet = json_mode();
    let (h, n) = hash_file(&p, if quiet { None } else { Some("hash") })
        .unwrap_or_else(|e| die(&format!("cannot read {}: {e}", p.display())));
    if json_mode() {
        json::line(&[("event", json::s("hash")), ("sha256", json::s(h)), ("size", json::V::N(n)),
                     ("engine", json::s(sha256::engine()))]);
    } else {
        println!("  {h}  {}  ({} bytes, {})", p.display(), with_commas(n), sha256::engine());
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
    step("flushing to the device (this is where a slow stick actually writes)");
    match writer.join() {
        Ok(Ok(())) => {}
        Ok(Err(e)) => die(&format!("write device: {e}")),
        Err(_) => die("the writing thread stopped unexpectedly; the stick is not complete"),
    }
    meter.finish(written.load(Ordering::Relaxed));
    (sha256::hex(&hasher.finish()), read_total, started.elapsed())
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
