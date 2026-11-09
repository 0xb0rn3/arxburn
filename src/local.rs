// Images already on this machine, found by what they are rather than by where someone might
// have typed a path: the Desktop, Downloads (the localised folder names too), ~/iso, the home
// folder, the current folder, and any mounted USB or external drive. A few levels into each,
// never following directory links, skipping hidden folders. Each candidate is classified by its
// bytes (format.rs), so a tarball or a VM disk with an .img name is not offered as burnable.
use crate::format::{self, Kind};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

pub struct Found {
    pub path: PathBuf,
    pub size: u64,
    pub write_size: Option<u64>,
    pub kind: Kind,
    pub compression: Option<format::Compression>,
    pub mtime: i64,
    /// on a mounted USB or external drive rather than this machine's own disk
    pub external: bool,
}

const DIRECT: [&str; 5] = ["iso", "img", "raw", "dd", "wic"];
const PACKED: [&str; 5] = ["xz", "zst", "zstd", "gz", "bz2"];
const MIN: u64 = 1024 * 1024;          // anything smaller is not a bootable image
const MAX_ENTRIES: usize = 40_000;     // a bound on the walk, so a huge home folder cannot stall it
const MAX_PACKED_LOOKS: usize = 60;    // each look inside a compressed file starts a decompressor

/// Does the name make it worth reading? Content decides the rest.
fn candidate(name: &str, size: u64) -> bool {
    let lower = name.to_ascii_lowercase();
    let mut parts = lower.rsplit('.');
    let ext = parts.next().unwrap_or("");
    if DIRECT.contains(&ext) { return size >= MIN; }
    if !PACKED.contains(&ext) { return false; }
    let inner = parts.next().unwrap_or("");
    if inner == "tar" || lower.ends_with(".tgz") || lower.ends_with(".tbz2") || lower.ends_with(".txz") { return false; }
    // foo.img.xz, foo.iso.zst: the name says disk image, so only a small floor (a mostly empty
    // image compresses very well; the size that matters is the decompressed one). A bare .xz with
    // no other hint is only worth a look when it is big enough to plausibly be a disk.
    if DIRECT.contains(&inner) { return size >= 64 * 1024; }
    size >= 64 * MIN
}

/// Where to look, most likely first. `home` is the invoking user's home even under sudo;
/// `mounts` are the external drives to include (the CLI passes external_mounts()).
pub fn roots(home: &Path, cwd: Option<&Path>, mounts: &[PathBuf]) -> Vec<(PathBuf, usize, bool)> {
    let mut r: Vec<(PathBuf, usize, bool)> = Vec::new();
    let mut push = |p: PathBuf, depth: usize, external: bool| {
        if p.is_dir() && !r.iter().any(|(q, _, _)| q == &p) { r.push((p, depth, external)); }
    };
    for p in xdg_dirs(home) { push(p, 3, false); }
    for p in ["Desktop", "Downloads", "iso", "ISO", "Images"] { push(home.join(p), 3, false); }
    push(home.to_path_buf(), 1, false);
    if let Some(c) = cwd { push(c.to_path_buf(), 2, false); }
    for m in mounts { push(m.clone(), 3, true); }
    r
}

/// The localised Desktop and Download folders from ~/.config/user-dirs.dirs, if set.
fn xdg_dirs(home: &Path) -> Vec<PathBuf> {
    let text = std::fs::read_to_string(home.join(".config/user-dirs.dirs")).unwrap_or_default();
    text.lines().filter_map(|l| {
        let (k, v) = l.split_once('=')?;
        if k != "XDG_DESKTOP_DIR" && k != "XDG_DOWNLOAD_DIR" { return None; }
        let v = v.trim().trim_matches('"');
        Some(if let Some(rest) = v.strip_prefix("$HOME/") { home.join(rest) } else { PathBuf::from(v) })
    }).collect()
}

/// Mount points of removable and external drives, from /proc/self/mounts.
pub fn external_mounts() -> Vec<PathBuf> {
    let text = std::fs::read_to_string("/proc/self/mounts").unwrap_or_default();
    text.lines().filter_map(|l| {
        let mut f = l.split_whitespace();
        let src = f.next()?;
        let dir = f.next()?.replace("\\040", " ");
        let ext = dir.starts_with("/run/media/") || dir.starts_with("/media/") || dir.starts_with("/mnt");
        (src.starts_with("/dev/") && ext).then(|| PathBuf::from(dir))
    }).collect()
}

pub fn scan(roots: &[(PathBuf, usize, bool)]) -> Vec<Found> {
    let mut out: Vec<Found> = Vec::new();
    let mut seen = 0usize;
    let mut packed_looks = 0usize;
    for (root, depth, external) in roots {
        walk(root, *depth, *external, &mut out, &mut seen, &mut packed_looks);
    }
    // the same file reached from two roots (Downloads and the home folder) is listed once
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out.dedup_by(|a, b| a.path == b.path);
    // newest first: the image someone just downloaded is the one they want
    out.sort_by(|a, b| b.mtime.cmp(&a.mtime).then_with(|| a.path.cmp(&b.path)));
    out
}

fn walk(dir: &Path, depth: usize, external: bool, out: &mut Vec<Found>, seen: &mut usize, packed_looks: &mut usize) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        *seen += 1;
        if *seen > MAX_ENTRIES { return; }
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') { continue; }
        // symlink_metadata: a directory link is never followed (no loops, no escaping the root)
        let Ok(ft) = e.file_type() else { continue };
        let path = e.path();
        if ft.is_dir() {
            if depth > 1 && name != "node_modules" && name != "target" { walk(&path, depth - 1, external, out, seen, packed_looks); }
            continue;
        }
        let Ok(md) = std::fs::metadata(&path) else { continue }; // a file link is fine to follow
        if !md.is_file() || !candidate(&name, md.len()) { continue; }
        let Ok((kind, _)) = format::sniff_file(&path) else { continue };
        let (inner, compression, write_size) = match kind {
            Kind::Compressed(c) => {
                if *packed_looks >= MAX_PACKED_LOOKS || !format::tool_present(c) { continue; }
                *packed_looks += 1;
                let Ok(h) = format::decompressed_head(c, &path) else { continue };
                (format::sniff(&h), Some(c), format::uncompressed_size(c, &path))
            }
            k => (k, None, Some(md.len())),
        };
        // only what can actually be burned: refused formats and archives are not offered
        if !matches!(inner, Kind::Optical { .. } | Kind::Disk { .. }) { continue; }
        out.push(Found { path, size: md.len(), write_size, kind: inner, compression, mtime: md.mtime(), external });
    }
}

/// The block device a file is stored on (e.g. "sdb1", or "dm-0" for an encrypted volume), if it
/// lives on one. dev::disks_behind turns that into the whole disk(s) underneath.
pub fn device_of(path: &Path) -> Option<String> {
    let dev = std::fs::metadata(path).ok()?.dev();
    // glibc's dev_t encoding (gnu_dev_major / gnu_dev_minor)
    let major = ((dev >> 8) & 0xfff) | ((dev >> 32) & !0xfff);
    let minor = (dev & 0xff) | ((dev >> 12) & !0xff);
    if major == 0 { return None; } // tmpfs, overlay, proc: not a block device
    let link = std::fs::canonicalize(format!("/sys/dev/block/{major}:{minor}")).ok()?;
    link.file_name().map(|n| n.to_string_lossy().into_owned())
}

/// The invoking user's home, even when this runs under sudo (HOME is then /root).
pub fn user_home() -> PathBuf {
    if let (Ok(u), Ok(h)) = (std::env::var("SUDO_USER"), std::env::var("HOME")) {
        if h == "/root" && !u.is_empty() {
            let pw = std::fs::read_to_string("/etc/passwd").unwrap_or_default();
            if let Some(dir) = pw.lines().find_map(|l| {
                let f: Vec<&str> = l.split(':').collect();
                (f.len() > 5 && f[0] == u).then(|| f[5].to_string())
            }) { return PathBuf::from(dir); }
        }
    }
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidates_by_name() {
        let big = 70 * MIN;
        for n in ["arxos-0.0.2-x86_64.iso", "raspios.img", "x.raw", "a.dd", "y.wic", "raspios.img.xz", "fw.raw.zst", "big.xz"] {
            assert!(candidate(n, big), "{n}");
        }
        for n in ["src.tar.gz", "src.tgz", "a.tar.xz", "notes.txt", "photo.jpg", "x.tbz2"] {
            assert!(!candidate(n, big), "{n}");
        }
        assert!(!candidate("tiny.iso", 1000), "below the minimum size");
        assert!(!candidate("small.xz", 2 * MIN), "a small bare .xz is not worth decompressing");
        assert!(candidate("small.img.xz", 2 * MIN), "but an .img.xz is");
        assert!(candidate("tiny.img.xz", 100 * 1024), "a well-compressed small image is still offered");
        assert!(!candidate("crumb.img.xz", 1000), "below the compressed floor");
    }

    #[test]
    fn scan_finds_by_content_newest_first_and_skips_impostors() {
        let root = std::env::temp_dir().join(format!("arxburn-local-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let deep = root.join("Downloads/a/b");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::create_dir_all(root.join(".hidden")).unwrap();
        std::fs::create_dir_all(root.join("Downloads/a/b/c/d")).unwrap();
        // realistic, incompressible content (xorshift), so the compressed copy is image-sized
        let mut disk = vec![0u8; 2 * MIN as usize];
        let mut s: u64 = 0x9E37_79B9_7F4A_7C15;
        for b in disk.iter_mut().skip(1024) { s ^= s << 13; s ^= s >> 7; s ^= s << 17; *b = s as u8; }
        disk[510] = 0x55; disk[511] = 0xAA;
        let mut iso = vec![0u8; 2 * MIN as usize];
        iso[16 * 2048 + 1..16 * 2048 + 6].copy_from_slice(b"CD001");
        std::fs::write(root.join("Downloads/old.img"), &disk).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1100));
        std::fs::write(deep.join("new.iso"), &iso).unwrap();                   // depth 3 under Downloads
        std::fs::write(root.join("Downloads/fake.img"), vec![7u8; 2 * MIN as usize]).unwrap(); // no structure
        std::fs::write(root.join(".hidden/secret.img"), &disk).unwrap();       // hidden: skipped
        std::fs::write(root.join("Downloads/a/b/c/d/too-deep.img"), &disk).unwrap();
        let mut zip = vec![0u8; 2 * MIN as usize]; zip[..4].copy_from_slice(b"PK\x03\x04");
        std::fs::write(root.join("Downloads/archive.img"), &zip).unwrap();     // a zip named .img
        if format::tool_present(format::Compression::Xz) {
            std::fs::write(root.join("Downloads/packed.img"), &disk).unwrap();
            let ok = std::process::Command::new("xz").args(["-T1", "-z", "--"]).arg(root.join("Downloads/packed.img")).status().unwrap();
            assert!(ok.success());
        }
        let found = scan(&roots(&root, None, &[]));
        let names: Vec<String> = found.iter().map(|f| f.path.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert!(names.contains(&"new.iso".to_string()) && names.contains(&"old.img".to_string()), "{names:?}");
        for skipped in ["fake.img", "secret.img", "too-deep.img", "archive.img"] {
            assert!(!names.contains(&skipped.to_string()), "{skipped} should not be offered: {names:?}");
        }
        let pos = |n: &str| names.iter().position(|x| x == n).unwrap();
        assert!(pos("new.iso") < pos("old.img"), "newest first: {names:?}");
        if format::tool_present(format::Compression::Xz) {
            let p = found.iter().find(|f| f.path.ends_with("packed.img.xz")).expect("compressed disk image listed");
            assert_eq!(p.compression, Some(format::Compression::Xz));
            assert_eq!(p.write_size, Some(2 * MIN));
        }
        assert_eq!(names.iter().filter(|n| *n == "old.img").count(), 1, "listed once though reached from two roots");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn device_of_virtual_fs_is_none_and_never_panics() {
        assert_eq!(device_of(Path::new("/proc/self/status")), None);
        assert_eq!(device_of(Path::new("/definitely/not/here")), None);
    }
}
