//! The download engine: one file, many connections, every network at once.
//!
//! A single HTTP connection to a mirror is almost never as fast as the machine's link, because
//! the mirror shapes per connection and TCP takes a while to find the ceiling. Pulling byte
//! ranges in parallel fixes that, and if the machine has more than one way onto the internet
//! (wifi and ethernet, or a tethered phone) the ranges can go out over all of them at once and
//! the speeds add up.
//!
//! The scheduling rules here are ported from Plexo (https://github.com/anmolkapil/plexo, MIT,
//! Copyright (c) 2026 Anmol Kapil), whose engine states them clearly and whose reasoning is
//! worth keeping verbatim:
//!
//!   - a free stream takes the first waiting block, except one that its own network already
//!     failed to deliver, which is left to another network for as long as a stream there is
//!     free. Otherwise a network that is not answering gets handed the same block again and
//!     again while healthy ones sit idle.
//!   - a second attempt at a block someone else holds (a hedge) is only handed out once NO
//!     block is left waiting, so it can never take bandwidth from work that still needs doing.
//!     What it costs is some bytes fetched twice at the very end; what it buys is that one slow
//!     connection can no longer hold up the whole download.
//!   - a block is hedged only once its holder has been at it long enough to have a measured
//!     speed and still needs at least as long again.
//!
//! The implementation is ours and shares no code with Plexo: that is TypeScript on Node's http
//! module, this is std-only Rust driving curl, one process per range, writing straight into the
//! destination file at the right offset.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::Read;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::net;

/// A block is hedged once its holder has been at it this long and still needs as long again.
const HEDGE_AFTER: Duration = Duration::from_secs(8);
/// Bounds the duplicate work on a block whose hedges keep failing.
const MAX_HEDGES_PER_BLOCK: u32 = 2;
/// A block whose attempts all come back empty this many times stops the download: something is
/// wrong with the link or the mirror, and retrying for ever just hides it.
const MAX_ATTEMPTS_PER_BLOCK: u32 = 5;
/// An attempt that has delivered nothing for this long is abandoned and the block requeued.
const STALL_AFTER: Duration = Duration::from_secs(20);

#[derive(Clone, Copy, PartialEq)]
enum Status { Pending, Running, Done }

struct Block {
    index: usize,
    start: u64,
    end: u64, // inclusive
    status: Status,
    got: u64,
    /// A network whose last attempt on this block delivered nothing.
    avoid: Option<String>,
    hedges: u32,
    /// Attempts that delivered nothing. A block that cannot be fetched at all must stop the
    /// download rather than be retried until the end of time.
    fails: u32,
    /// In-flight attempts: (stream id, network, when it started).
    attempts: Vec<(usize, String, Instant)>,
}

impl Block {
    fn len(&self) -> u64 { self.end - self.start + 1 }
}

struct Plan {
    blocks: Vec<Block>,
    /// Streams waiting for work, by network, so the scheduler can tell whether another network
    /// could take a block instead of the one that just failed at it.
    idle_by_net: HashMap<String, usize>,
    speed: HashMap<usize, f64>, // stream id -> bytes per second
}

enum Work { Primary(usize), Hedge(usize), Nothing }

impl Plan {
    /// The first waiting block, skipping one this network already failed to deliver while some
    /// other network has a stream free to take it. With no other network free it is taken
    /// anyway, so a block can never be stranded.
    fn next_waiting(&self, net_id: &str) -> Option<usize> {
        let other_free = self.idle_by_net.iter().any(|(n, c)| n != net_id && *c > 0);
        self.blocks.iter()
            .find(|b| b.status == Status::Pending
                   && !(other_free && b.avoid.as_deref() == Some(net_id)))
            .map(|b| b.index)
    }

    /// The block most worth a second attempt: the one whose holder will be longest yet.
    fn next_hedge(&self, stream: usize, net_id: &str, now: Instant) -> Option<usize> {
        if self.blocks.iter().any(|b| b.status == Status::Pending) { return None; }
        let mut target = None;
        let mut latest = 0f64;
        for b in &self.blocks {
            if b.status != Status::Running || b.attempts.len() != 1 { continue; }
            let (holder_id, holder_net, started) = &b.attempts[0];
            if *holder_id == stream { continue; }
            if b.hedges >= MAX_HEDGES_PER_BLOCK { continue; }
            if b.avoid.as_deref() == Some(net_id) { continue; }
            if now.duration_since(*started) < HEDGE_AFTER { continue; }
            let remaining = b.len().saturating_sub(b.got) as f64;
            if remaining <= 0.0 { continue; }
            // a holder that has gone quiet has no finish time at all
            let eta = match self.speed.get(holder_id) {
                Some(s) if *s > 1.0 => remaining / s,
                _ => f64::INFINITY,
            };
            if eta < HEDGE_AFTER.as_secs_f64() { continue; }
            // the holder's own network may be what is slow, so another network goes first
            let other_free = self.idle_by_net.iter()
                .any(|(n, c)| n != holder_net.as_str() && *c > 0 && b.avoid.as_deref() != Some(n));
            if holder_net == net_id && other_free { continue; }
            if eta > latest { latest = eta; target = Some(b.index); }
        }
        target
    }

    fn pick(&self, stream: usize, net_id: &str, now: Instant) -> Work {
        if let Some(i) = self.next_waiting(net_id) { return Work::Primary(i); }
        if let Some(i) = self.next_hedge(stream, net_id, now) { return Work::Hedge(i); }
        Work::Nothing
    }

    fn finished(&self) -> bool { self.blocks.iter().all(|b| b.status == Status::Done) }
}

/// What a one byte ranged GET tells us. Cheaper than fetching the body and, unlike HEAD, the
/// 206 proves range requests actually work rather than merely being advertised.
pub struct Probe { pub size: u64, pub ranges: bool }

pub fn probe(url: &str) -> Probe {
    let out = Command::new("curl")
        .args(["-sS", "-L", "-D", "-", "-o", "/dev/null", "--max-time", "30",
               "-r", "0-0", "-A", net::UA, url])
        .output();
    let head = match out { Ok(o) => String::from_utf8_lossy(&o.stdout).into_owned(), Err(_) => String::new() };
    let mut size = 0u64;
    let mut ranges = false;
    for line in head.lines() {
        let l = line.to_ascii_lowercase();
        if l.starts_with("content-range:") {
            // Content-Range: bytes 0-0/4223172608
            if let Some(total) = line.rsplit('/').next() {
                if let Ok(n) = total.trim().parse::<u64>() { size = n; ranges = true; }
            }
        } else if l.starts_with("content-length:") && size == 0 {
            if let Some(v) = line.split(':').nth(1) { size = v.trim().parse().unwrap_or(0); }
        }
    }
    Probe { size, ranges }
}

/// Usable ways onto the internet: interfaces that are up, have a route, and are not loopback.
/// Each one can carry its own streams, which is where the speeds add up.
pub fn networks() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if let Ok(routes) = std::fs::read_to_string("/proc/net/route") {
        for line in routes.lines().skip(1) {
            if let Some(iface) = line.split_whitespace().next() {
                if iface != "lo" && !out.iter().any(|x| x == iface) { out.push(iface.to_string()); }
            }
        }
    }
    // keep only what is actually up, and drop virtual bridges that route nowhere useful
    out.retain(|i| {
        if i.starts_with("virbr") || i.starts_with("docker") || i.starts_with("br-") { return false; }
        std::fs::read_to_string(format!("/sys/class/net/{i}/operstate"))
            .map(|s| s.trim() == "up" || s.trim() == "unknown").unwrap_or(false)
    });
    out
}

pub struct Opts {
    /// How many connections to open on each interface. More than a handful stops helping and
    /// starts looking like abuse to the mirror.
    pub streams_per_network: usize,
    /// Empty means "work it out from the routing table".
    pub networks: Vec<String>,
    /// Zero means "about four blocks per stream".
    pub block_size: u64,
}

impl Default for Opts {
    fn default() -> Self {
        Opts { streams_per_network: 4, networks: Vec::new(), block_size: 0 }
    }
}

fn state_path(dest: &Path) -> PathBuf {
    let mut p = dest.to_path_buf();
    let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    p.set_file_name(format!(".{name}.arxburn"));
    p
}

/// Read back which blocks a previous run finished, so an interrupted download continues instead
/// of starting over. The record is only trusted when it describes this exact url and size.
fn load_done(dest: &Path, url: &str, size: u64) -> Vec<usize> {
    let body = match std::fs::read_to_string(state_path(dest)) { Ok(b) => b, Err(_) => return vec![] };
    let mut lines = body.lines();
    if lines.next() != Some(url) { return vec![]; }
    if lines.next().and_then(|l| l.parse::<u64>().ok()) != Some(size) { return vec![]; }
    lines.next().unwrap_or("").split(',').filter_map(|x| x.parse().ok()).collect()
}

fn save_done(dest: &Path, url: &str, size: u64, done: &[usize]) {
    let list: Vec<String> = done.iter().map(|i| i.to_string()).collect();
    let _ = std::fs::write(state_path(dest), format!("{url}\n{size}\n{}\n", list.join(",")));
}

/// Download `url` to `dest` over many connections at once. Returns Err with a reason when the
/// parallel path cannot be used, so the caller can fall back to a plain single stream.
pub fn parallel<F>(url: &str, dest: &Path, size: u64, opts: &Opts, mut on_progress: F) -> Result<(), String>
where F: FnMut(u64) + Send,
{
    let nets = if opts.networks.is_empty() { networks() } else { opts.networks.clone() };
    let nets = if nets.is_empty() { vec![String::new()] } else { nets }; // empty name = let the OS choose
    let streams: Vec<(usize, String)> = nets.iter()
        .flat_map(|n| (0..opts.streams_per_network).map(move |_| n.clone()))
        .enumerate().collect();
    if streams.is_empty() { return Err("no usable network".into()); }

    let block = if opts.block_size > 0 { opts.block_size } else {
        // about four blocks per stream, so a slow stream cannot hold a large tail
        (size / (streams.len() as u64 * 4)).clamp(8 << 20, 64 << 20)
    };

    let file = OpenOptions::new().create(true).write(true).read(true).open(dest)
        .map_err(|e| format!("cannot open {}: {e}", dest.display()))?;
    file.set_len(size).map_err(|e| format!("cannot size {}: {e}", dest.display()))?;
    let file = Arc::new(file);

    let already = load_done(dest, url, size);
    let mut blocks = Vec::new();
    let mut start = 0u64;
    let mut index = 0usize;
    while start < size {
        let end = std::cmp::min(start + block - 1, size - 1);
        let done = already.contains(&index);
        blocks.push(Block { index, start, end, got: if done { end - start + 1 } else { 0 },
                            status: if done { Status::Done } else { Status::Pending },
                            avoid: None, hedges: 0, fails: 0, attempts: Vec::new() });
        start = end + 1;
        index += 1;
    }
    let resumed: u64 = blocks.iter().filter(|b| b.status == Status::Done).map(|b| b.len()).sum();

    let mut idle_by_net: HashMap<String, usize> = HashMap::new();
    for (_, n) in &streams { *idle_by_net.entry(n.clone()).or_insert(0) += 1; }

    let plan = Arc::new(Mutex::new(Plan { blocks, idle_by_net, speed: HashMap::new() }));
    let done_bytes = Arc::new(AtomicU64::new(resumed));
    let failed = Arc::new(Mutex::new(Option::<String>::None));
    let stop = Arc::new(AtomicBool::new(false));

    let mut handles = Vec::new();
    for (id, netname) in streams {
        let (plan, file, done_bytes, failed, stop) =
            (Arc::clone(&plan), Arc::clone(&file), Arc::clone(&done_bytes), Arc::clone(&failed), Arc::clone(&stop));
        let url = url.to_string();
        let dest_path = dest.to_path_buf();
        handles.push(std::thread::spawn(move || {
            loop {
                if stop.load(Ordering::Relaxed) { return; }
                let (work, range) = {
                    let mut p = plan.lock().unwrap();
                    if p.finished() { return; }
                    let w = p.pick(id, &netname, Instant::now());
                    match w {
                        Work::Nothing => { drop(p); std::thread::sleep(Duration::from_millis(200)); continue; }
                        Work::Primary(i) | Work::Hedge(i) => {
                            if let Work::Hedge(_) = w { p.blocks[i].hedges += 1; }
                            let b = &mut p.blocks[i];
                            b.status = Status::Running;
                            b.attempts.push((id, netname.clone(), Instant::now()));
                            let r = (b.start + b.got, b.end);
                            if let Some(c) = p.idle_by_net.get_mut(&netname) { *c = c.saturating_sub(1); }
                            (i, r)
                        }
                    }
                };

                let got = fetch_range(&url, &netname, range.0, range.1, &file, &done_bytes, &stop, |bps| {
                    plan.lock().unwrap().speed.insert(id, bps);
                });

                let mut p = plan.lock().unwrap();
                *p.idle_by_net.entry(netname.clone()).or_insert(0) += 1;
                let b = &mut p.blocks[work];
                b.attempts.retain(|(sid, _, _)| *sid != id);
                match got {
                    Ok(n) if b.status != Status::Done => {
                        b.got += n;
                        if b.got >= b.len() {
                            b.status = Status::Done;
                            b.attempts.clear();
                            let list: Vec<usize> = p.blocks.iter()
                                .filter(|x| x.status == Status::Done).map(|x| x.index).collect();
                            drop(p);
                            save_done(&dest_path, &url, size, &list);
                            continue;
                        }
                        // a partial answer: leave it pending, the rest of it is still owed
                        if b.attempts.is_empty() { b.status = Status::Pending; }
                    }
                    Ok(_) => {} // a hedge lost the race; its bytes were identical anyway
                    Err(e) => {
                        if b.status != Status::Done {
                            if b.got == 0 { b.avoid = Some(netname.clone()); b.fails += 1; }
                            if b.attempts.is_empty() { b.status = Status::Pending; }
                        }
                        let give_up = b.fails > MAX_ATTEMPTS_PER_BLOCK;
                        let mut f = failed.lock().unwrap();
                        if f.is_none() { *f = Some(e); }
                        drop(f);
                        if give_up { stop.store(true, Ordering::Relaxed); return; }
                    }
                }
            }
        }));
    }

    // the parent watches: progress out, and a stop when everything is done
    let total = size;
    loop {
        let n = done_bytes.load(Ordering::Relaxed);
        on_progress(n);
        if plan.lock().unwrap().finished() { break; }
        if handles.iter().all(|h| h.is_finished()) { break; }
        std::thread::sleep(Duration::from_millis(120));
    }
    stop.store(true, Ordering::Relaxed);
    for h in handles { let _ = h.join(); }
    on_progress(done_bytes.load(Ordering::Relaxed));

    let complete = plan.lock().unwrap().finished();
    if complete {
        let _ = std::fs::remove_file(state_path(dest));
        Ok(())
    } else {
        let why = failed.lock().unwrap().clone().unwrap_or_else(|| "the download did not finish".into());
        Err(format!("{why} ({} of {} bytes fetched; run it again to resume)",
            done_bytes.load(Ordering::Relaxed), total))
    }
}

/// One range, one curl, straight into the file at the right offset.
fn fetch_range<S>(url: &str, netname: &str, start: u64, end: u64, file: &Arc<File>,
                  done: &Arc<AtomicU64>, stop: &Arc<AtomicBool>, mut speed: S) -> Result<u64, String>
where S: FnMut(f64),
{
    let mut cmd = Command::new("curl");
    cmd.args(["-sS", "-L", "--max-time", "0", "--connect-timeout", "20",
              "-A", net::UA, "-r", &format!("{start}-{end}")]);
    if !netname.is_empty() { cmd.args(["--interface", netname]); }
    cmd.arg(url).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| format!("curl: {e}"))?;
    let mut out = child.stdout.take().ok_or("curl produced no output")?;

    let mut buf = vec![0u8; 1 << 20];
    let mut at = start;
    let mut written = 0u64;
    let mut last_byte = Instant::now();
    let began = Instant::now();
    loop {
        if stop.load(Ordering::Relaxed) { let _ = child.kill(); break; }
        // checked before the read, not after, so a connection that never answers at all is
        // caught too, and so the initial value is actually used
        if last_byte.elapsed() > STALL_AFTER {
            let _ = child.kill();
            return Err(format!("a connection{} stalled",
                if netname.is_empty() { String::new() } else { format!(" on {netname}") }));
        }
        match out.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                file.write_at(&buf[..n], at).map_err(|e| format!("write: {e}"))?;
                at += n as u64;
                written += n as u64;
                done.fetch_add(n as u64, Ordering::Relaxed);
                last_byte = Instant::now();
                let secs = began.elapsed().as_secs_f64().max(0.001);
                speed(written as f64 / secs);
            }
            Err(e) => { let _ = child.kill(); return Err(format!("read: {e}")); }
        }
    }
    let status = child.wait().map_err(|e| format!("curl: {e}"))?;
    if !status.success() && written == 0 {
        let mut err = String::new();
        if let Some(mut e) = child.stderr.take() { let _ = e.read_to_string(&mut err); }
        return Err(format!("curl failed{}: {}",
            if netname.is_empty() { String::new() } else { format!(" on {netname}") },
            err.trim().lines().next().unwrap_or("no detail")));
    }
    Ok(written)
}

/// Report what the engine would do, for the GUI and for `arxburn net`.
pub fn describe(streams_per_network: usize) -> String {
    let nets = networks();
    if nets.is_empty() { return "no network interface is up".into(); }
    format!("{} connection{} over {}", nets.len() * streams_per_network,
        if nets.len() * streams_per_network == 1 { "" } else { "s" }, nets.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(states: &[(Status, Option<&str>)]) -> Plan {
        let blocks = states.iter().enumerate().map(|(i, (s, avoid))| Block {
            index: i, start: i as u64 * 100, end: i as u64 * 100 + 99, status: *s, got: 0,
            avoid: avoid.map(String::from), hedges: 0, fails: 0, attempts: Vec::new(),
        }).collect();
        Plan { blocks, idle_by_net: HashMap::new(), speed: HashMap::new() }
    }

    #[test]
    fn a_network_that_failed_a_block_leaves_it_to_another_one() {
        let mut p = plan(&[(Status::Pending, Some("wlan0")), (Status::Pending, None)]);
        p.idle_by_net.insert("eth0".into(), 1);
        // wlan0 skips the block it already failed, because eth0 is free to take it
        assert_eq!(p.next_waiting("wlan0"), Some(1));
        // eth0 has no such history, so it takes the first one
        assert_eq!(p.next_waiting("eth0"), Some(0));
    }

    #[test]
    fn a_block_is_never_stranded_when_no_other_network_is_free() {
        let mut p = plan(&[(Status::Pending, Some("wlan0"))]);
        p.idle_by_net.insert("wlan0".into(), 1);
        assert_eq!(p.next_waiting("wlan0"), Some(0), "with nobody else free it must be taken anyway");
    }

    #[test]
    fn hedging_never_steals_from_work_that_still_needs_doing() {
        let mut p = plan(&[(Status::Running, None), (Status::Pending, None)]);
        p.blocks[0].attempts.push((1, "wlan0".into(), Instant::now() - Duration::from_secs(60)));
        assert!(p.next_hedge(2, "eth0", Instant::now()).is_none(),
            "a block is still waiting, so a second attempt must not be started");
    }

    #[test]
    fn a_slow_holder_gets_hedged_once_nothing_is_waiting() {
        let mut p = plan(&[(Status::Running, None)]);
        p.blocks[0].attempts.push((1, "wlan0".into(), Instant::now() - Duration::from_secs(60)));
        p.speed.insert(1, 1.0); // one byte a second: it will never finish
        assert_eq!(p.next_hedge(2, "eth0", Instant::now()), Some(0));
        // but not past the cap
        p.blocks[0].hedges = MAX_HEDGES_PER_BLOCK;
        assert!(p.next_hedge(2, "eth0", Instant::now()).is_none());
    }

    #[test]
    fn a_holder_that_is_nearly_finished_is_left_alone() {
        let mut p = plan(&[(Status::Running, None)]);
        p.blocks[0].attempts.push((1, "wlan0".into(), Instant::now() - Duration::from_secs(60)));
        p.speed.insert(1, 1_000_000.0); // 100 bytes left: done in a blink
        assert!(p.next_hedge(2, "eth0", Instant::now()).is_none());
    }
}
