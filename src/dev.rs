//! Finding block devices, and deciding which ones must never be written to.
//!
//! Everything here reads /sys and /proc rather than shelling out to lsblk, so arxburn works in a
//! rescue shell or an installer environment where util-linux may be trimmed down.

use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Device {
    pub name: String,          // sdc
    pub path: PathBuf,         // /dev/sdc
    pub size: u64,             // bytes
    pub removable: bool,
    pub model: String,
    pub mounts: Vec<String>,   // mountpoints of this disk or any of its partitions
    pub holds_root: bool,      // carries the running "/" (never writable)
}

impl Device {
    /// Why this device must not be written, or None when it is a legitimate target.
    /// Kept as one function so `list` can explain a device and `write` can refuse it for the
    /// same reason, instead of two checks drifting apart.
    pub fn refusal(&self, allow_internal: bool) -> Option<String> {
        if self.holds_root {
            return Some("carries the running system (/)".into());
        }
        if self.size == 0 {
            return Some("no medium".into());
        }
        if !self.removable && !allow_internal {
            return Some("internal disk (pass --allow-internal if you really mean it)".into());
        }
        None
    }
}

fn read_trim(p: &Path) -> Option<String> {
    fs::read_to_string(p).ok().map(|s| s.trim().to_string())
}

/// The mounts the running system cannot survive losing.
const CRITICAL: [&str; 5] = ["/", "/boot", "/boot/efi", "/usr", "/var"];

/// Pull both identifications of the critical mounts out of mountinfo: the major:minor the kernel
/// reports, and the device path it was mounted from.
///
/// Both are needed. btrfs, ZFS and any filesystem on an anonymous block device report something
/// like 0:35 for "/", which matches no disk in /sys, so a check on numbers alone quietly decides
/// the system disk is an ordinary target. That is the one mistake this tool must never make, so
/// the mount SOURCE (/dev/nvme0n1p2) is read as well and resolved back to its disk.
fn critical_sources(mountinfo: &str) -> (Vec<(u32, u32)>, Vec<String>) {
    let (mut nums, mut srcs) = (Vec::new(), Vec::new());
    for line in mountinfo.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 5 || !CRITICAL.contains(&f[4]) { continue; }
        let mut it = f[2].split(':');
        if let (Some(Ok(a)), Some(Ok(b))) = (it.next().map(str::parse), it.next().map(str::parse)) {
            nums.push((a, b));
        }
        // after the optional fields and their "-" separator come fstype, source, options
        if let Some(sep) = f.iter().position(|x| *x == "-") {
            if let Some(src) = f.get(sep + 2) {
                if src.starts_with("/dev/") { srcs.push((*src).to_string()); }
            }
        }
    }
    (nums, srcs)
}

/// Walk a mount source back to the whole disks it lives on: a partition to its disk, and a
/// device-mapper node (LUKS, LVM) through its slaves, which is how an encrypted root resolves.
fn disks_behind(source: &str) -> Vec<String> {
    let real = fs::canonicalize(source).unwrap_or_else(|_| PathBuf::from(source));
    let name = match real.file_name() { Some(n) => n.to_string_lossy().to_string(), None => return vec![] };
    let mut out = Vec::new();
    let slaves = Path::new("/sys/class/block").join(&name).join("slaves");
    if slaves.is_dir() {
        if let Ok(rd) = fs::read_dir(&slaves) {
            for e in rd.flatten() {
                out.extend(disks_behind(&format!("/dev/{}", e.file_name().to_string_lossy())));
            }
        }
        if !out.is_empty() { return out; }
    }
    // a partition's directory sits inside its disk's directory
    if let Ok(link) = fs::read_link(Path::new("/sys/class/block").join(&name)) {
        let parts: Vec<String> = link.iter().map(|c| c.to_string_lossy().to_string()).collect();
        if parts.len() >= 2 {
            let parent = &parts[parts.len() - 2];
            if Path::new("/sys/block").join(parent).is_dir() { out.push(parent.clone()); return out; }
        }
    }
    if Path::new("/sys/block").join(&name).is_dir() { out.push(name); }
    out
}

fn mounts_for(prefixes: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(m) = fs::read_to_string("/proc/mounts") {
        for line in m.lines() {
            let mut it = line.split_whitespace();
            let (src, dst) = (it.next().unwrap_or(""), it.next().unwrap_or(""));
            if prefixes.iter().any(|p| src == p || src.starts_with(&format!("{p}p")) || (src.starts_with(p) && src[p.len()..].chars().all(|c| c.is_ascii_digit()))) {
                out.push(dst.to_string());
            }
        }
    }
    out
}

pub fn list(include_loop: bool) -> Vec<Device> {
    let mut devices = Vec::new();
    let mi = fs::read_to_string("/proc/self/mountinfo").unwrap_or_default();
    let (root_nums, root_srcs) = critical_sources(&mi);
    let system_disks: Vec<String> = root_srcs.iter().flat_map(|s| disks_behind(s)).collect();
    let entries = match fs::read_dir("/sys/block") { Ok(e) => e, Err(_) => return devices };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        // ram/zram/dm/sr are not burn targets; loop only when asked (it is how you test an image)
        if name.starts_with("ram") || name.starts_with("zram") || name.starts_with("dm-") || name.starts_with("sr") {
            continue;
        }
        if name.starts_with("loop") && !include_loop {
            continue;
        }
        let sysdir = e.path();
        let sectors: u64 = read_trim(&sysdir.join("size")).and_then(|s| s.parse().ok()).unwrap_or(0);
        let removable = read_trim(&sysdir.join("removable")).map(|s| s == "1").unwrap_or(false);
        let model = read_trim(&sysdir.join("device/model"))
            .or_else(|| read_trim(&sysdir.join("device/name")))
            .unwrap_or_else(|| "unknown".into());
        let dev = read_trim(&sysdir.join("dev")).unwrap_or_default();
        // signal one: this disk, or one of its partitions, was resolved from a critical mount
        let mut holds_root = system_disks.contains(&name);
        // signal two: the kernel's own major:minor for a critical mount
        if !holds_root {
            let mut nums = vec![dev.clone()];
            if let Ok(rd) = fs::read_dir(&sysdir) {
                for p in rd.flatten() {
                    let pn = p.file_name().to_string_lossy().to_string();
                    if pn.starts_with(&name) {
                        if let Some(d) = read_trim(&p.path().join("dev")) { nums.push(d); }
                    }
                }
            }
            holds_root = nums.iter().any(|d| {
                let mut it = d.split(':');
                match (it.next().and_then(|x| x.parse::<u32>().ok()), it.next().and_then(|x| x.parse::<u32>().ok())) {
                    (Some(a), Some(b)) => root_nums.contains(&(a, b)),
                    _ => false,
                }
            });
        }
        let path = PathBuf::from(format!("/dev/{name}"));
        let mounts = mounts_for(&[path.to_string_lossy().to_string()]);
        devices.push(Device { name, path, size: sectors * 512, removable, model, mounts, holds_root });
    }
    devices.sort_by(|a, b| a.name.cmp(&b.name));
    devices
}

/// Resolve what the user typed: /dev/sdc, sdc, or a filesystem UUID of one of its partitions.
/// UUID is the safer habit, because sd* letters move when you replug, and that is precisely how
/// people overwrite the wrong disk.
pub fn resolve(target: &str, include_loop: bool) -> Result<Device, String> {
    let devices = list(include_loop);
    let want = target.trim_start_matches("/dev/");
    if let Some(d) = devices.iter().find(|d| d.name == want) {
        return Ok(d.clone());
    }
    // by-uuid points at a partition; climb to its disk
    let by_uuid = PathBuf::from("/dev/disk/by-uuid").join(target);
    if let Ok(real) = fs::canonicalize(&by_uuid) {
        let part = real.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        if let Some(d) = devices.iter().find(|d| part.starts_with(&d.name)) {
            return Ok(d.clone());
        }
    }
    Err(format!("no block device matches '{target}' (try: arxburn list)"))
}

pub fn human(bytes: u64) -> String {
    const U: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 { v /= 1024.0; i += 1; }
    if i == 0 { format!("{} {}", bytes, U[0]) } else { format!("{v:.1} {}", U[i]) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn dev(removable: bool, holds_root: bool, size: u64) -> Device {
        Device { name: "sdx".into(), path: PathBuf::from("/dev/sdx"), size, removable,
                 model: "test".into(), mounts: vec![], holds_root }
    }

    #[test]
    fn the_running_system_is_never_a_target() {
        // even with --allow-internal, even when removable: this one is absolute
        assert!(dev(true, true, 1 << 30).refusal(true).is_some());
        assert!(dev(false, true, 1 << 30).refusal(true).is_some());
    }

    #[test]
    fn internal_disks_need_an_explicit_flag() {
        assert!(dev(false, false, 1 << 30).refusal(false).is_some());
        assert!(dev(false, false, 1 << 30).refusal(true).is_none());
    }

    #[test]
    fn a_removable_stick_is_allowed_and_an_empty_reader_is_not() {
        assert!(dev(true, false, 1 << 30).refusal(false).is_none());
        assert!(dev(true, false, 0).refusal(false).is_some()); // card reader with no card
    }

    // The host this was written on runs btrfs, where "/" reports device 0:35 and no disk in
    // /sys carries that number. An earlier version checked only the number and would have
    // offered the system disk as an ordinary target.
    #[test]
    fn a_btrfs_root_is_still_traced_back_to_its_disk() {
        let mi = "25 1 0:35 /@ / rw,relatime shared:1 - btrfs /dev/nvme0n1p2 rw,ssd,subvol=/@\n\
                  31 25 259:1 / /boot/efi rw - vfat /dev/nvme0n1p1 rw\n\
                  40 25 8:33 / /run/media/me/STICK rw - vfat /dev/sdc1 rw";
        let (nums, srcs) = critical_sources(mi);
        assert!(nums.contains(&(0, 35)), "the anonymous number is still recorded");
        assert!(srcs.contains(&"/dev/nvme0n1p2".to_string()), "the real source must be found");
        assert!(srcs.contains(&"/dev/nvme0n1p1".to_string()), "/boot/efi counts too");
        assert!(!srcs.contains(&"/dev/sdc1".to_string()), "a plugged in stick is not critical");
    }

    #[test]
    fn human_sizes_read_like_a_person_wrote_them() {
        assert_eq!(human(0), "0 B");
        assert_eq!(human(512), "512 B");
        assert_eq!(human(4 * 1024 * 1024 * 1024), "4.0 GB");
    }
}
