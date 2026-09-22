//! Where the images come from.
//!
//! "Latest" is the hard part. A burn tool that ships a list of fixed URLs is out of date the week
//! after it is built, so most entries here describe HOW to find the newest image (a directory to
//! read, a version to pick) rather than naming one. Entries can also be replaced or added without
//! rebuilding: drop a TSV at ~/.config/arxburn/catalog.tsv or /etc/arxburn/catalog.tsv.

use crate::net;

#[derive(Debug, Clone, PartialEq)]
pub enum Family { Linux, Windows, Tool }

impl Family {
    pub fn label(&self) -> &'static str {
        match self { Family::Linux => "linux", Family::Windows => "windows", Family::Tool => "tool" }
    }
    fn parse(s: &str) -> Family {
        match s.trim().to_ascii_lowercase().as_str() {
            "windows" => Family::Windows,
            "tool" => Family::Tool,
            _ => Family::Linux,
        }
    }
}

#[derive(Debug, Clone)]
pub enum Source {
    /// A URL that always serves the current image.
    Direct(String),
    /// Read a directory index or a download page and take the newest name matching
    /// prefix/suffix. `reject` is a comma separated list of substrings that drop variants of the
    /// same shape (the mac build, the edu build, a beta), and in a Chain it filters the
    /// directories as well.
    Index { dir: String, prefix: String, suffix: String, reject: String },
    /// Take the newest versioned subdirectory, then do the same inside it.
    Chain { root: String, dir_prefix: String, sub: String, prefix: String, suffix: String, reject: String },
    /// The newest GitHub release of a repo, picking the asset ending in `suffix`.
    Github { repo: String, suffix: String },
    /// A Microsoft fwlink that redirects to the real file.
    Fwlink(String),
    /// An archive.org item: take its largest .iso.
    Archive(String),
    /// Microsoft's download page, resolved through their own download connector.
    Windows(String),
}

#[derive(Debug, Clone)]
pub struct Iso {
    pub id: String,
    pub name: String,
    pub family: Family,
    pub note: String,
    pub src: Source,
    /// A checksum file in the same directory, if the project publishes one.
    pub sums: Option<String>,
}

pub struct Resolved { pub url: String, pub filename: String, pub sha256: Option<String> }

fn e(id: &str, name: &str, family: Family, note: &str, src: Source, sums: Option<&str>) -> Iso {
    Iso { id: id.into(), name: name.into(), family, note: note.into(), src,
          sums: sums.map(String::from) }
}

pub fn builtin() -> Vec<Iso> {
    vec![
        // ---- ours -------------------------------------------------------------------------
        e("arxos", "ArxOS 0.0.1", Family::Linux, "our own release, hash published with it",
          Source::Direct("https://pub-d5ff3efb1d204998aa120ded02d070b9.r2.dev/arxos-0.0.1.iso".into()), None),

        // ---- rolling and bleeding edge ----------------------------------------------------
        e("arch", "Arch Linux", Family::Linux, "monthly rolling snapshot",
          Source::Index { dir: "https://geo.mirror.pkgbuild.com/iso/latest/".into(),
                  prefix: "archlinux-".into(), suffix: "-x86_64.iso".into(), reject: "".into() }, Some("sha256sums.txt")),
        e("cachyos", "CachyOS Desktop", Family::Linux, "performance-tuned Arch",
          Source::Chain { root: "https://mirror.cachyos.org/ISO/desktop/".into(), dir_prefix: "2".into(),
                  sub: "".into(), prefix: "cachyos-desktop-linux-".into(), suffix: ".iso".into(), reject: "".into() }, None),
        e("endeavouros", "EndeavourOS", Family::Linux, "Arch with an installer",
          Source::Index { dir: "https://mirror.alpix.eu/endeavouros/iso/".into(),
                  prefix: "EndeavourOS_".into(), suffix: ".iso".into(), reject: "".into() }, None),
        e("tumbleweed", "openSUSE Tumbleweed", Family::Linux, "rolling, always current",
          Source::Direct("https://download.opensuse.org/tumbleweed/iso/openSUSE-Tumbleweed-DVD-x86_64-Current.iso".into()),
          Some("openSUSE-Tumbleweed-DVD-x86_64-Current.iso.sha256")),
        e("nixos", "NixOS unstable", Family::Linux, "minimal, from the unstable channel",
          Source::Direct("https://channels.nixos.org/nixos-unstable/latest-nixos-minimal-x86_64-linux.iso".into()), None),
        e("void", "Void Linux", Family::Linux, "rolling, musl or glibc live image",
          Source::Index { dir: "https://repo-default.voidlinux.org/live/current/".into(),
                  prefix: "void-live-x86_64-".into(), suffix: ".iso".into(), reject: "musl".into() }, Some("sha256sum.txt")),
        e("gentoo", "Gentoo minimal", Family::Linux, "weekly autobuild",
          Source::Index { dir: "https://distfiles.gentoo.org/releases/amd64/autobuilds/current-install-amd64-minimal/".into(),
                  prefix: "install-amd64-minimal-".into(), suffix: ".iso".into(), reject: "".into() }, None),
        e("debian-sid", "Debian testing weekly", Family::Linux, "weekly netinst of testing",
          Source::Index { dir: "https://cdimage.debian.org/cdimage/weekly-builds/amd64/iso-cd/".into(),
                  prefix: "debian-testing-".into(), suffix: "-netinst.iso".into(), reject: "".into() }, Some("SHA256SUMS")),
        e("fedora-rawhide", "Fedora Rawhide", Family::Linux, "Fedora's development branch",
          Source::Index { dir: "https://mirrors.kernel.org/fedora/development/rawhide/Workstation/x86_64/iso/".into(),
                  prefix: "Fedora-Workstation-Live-".into(), suffix: ".iso".into(), reject: "".into() }, None),
        e("kali-weekly", "Kali weekly", Family::Linux, "weekly build, ahead of the release",
          Source::Index { dir: "https://cdimage.kali.org/kali-weekly/".into(),
                  prefix: "kali-linux-".into(), suffix: "-installer-amd64.iso".into(), reject: "".into() }, Some("SHA256SUMS")),

        // ---- stable releases ---------------------------------------------------------------
        e("debian", "Debian stable netinst", Family::Linux, "current stable",
          Source::Index { dir: "https://cdimage.debian.org/debian-cd/current/amd64/iso-cd/".into(),
                  prefix: "debian-".into(), suffix: "-amd64-netinst.iso".into(), reject: "debian-mac,debian-edu".into() }, Some("SHA256SUMS")),
        e("debian-live", "Debian stable live", Family::Linux, "current stable, GNOME live",
          Source::Index { dir: "https://cdimage.debian.org/debian-cd/current-live/amd64/iso-hybrid/".into(),
                  prefix: "debian-live-".into(), suffix: "-amd64-gnome.iso".into(), reject: "".into() }, Some("SHA256SUMS")),
        e("ubuntu", "Ubuntu desktop", Family::Linux, "newest release on releases.ubuntu.com",
          Source::Chain { root: "https://releases.ubuntu.com/".into(), dir_prefix: "2".into(), sub: "".into(),
                  prefix: "ubuntu-".into(), suffix: "-desktop-amd64.iso".into(), reject: "".into() }, Some("SHA256SUMS")),
        e("fedora", "Fedora Workstation", Family::Linux, "newest stable release",
          Source::Chain { root: "https://mirrors.kernel.org/fedora/releases/".into(), dir_prefix: "".into(),
                  sub: "Workstation/x86_64/iso/".into(), prefix: "Fedora-Workstation-Live-".into(),
                  suffix: ".iso".into(), reject: "".into() }, None),
        e("kali", "Kali Linux live", Family::Linux, "current release",
          Source::Index { dir: "https://cdimage.kali.org/current/".into(),
                  prefix: "kali-linux-".into(), suffix: "-installer-amd64.iso".into(), reject: "".into() }, Some("SHA256SUMS")),
        e("parrot", "Parrot Security", Family::Linux, "current security edition",
          Source::Chain { root: "https://deb.parrot.sh/parrot/iso/".into(), dir_prefix: "".into(), sub: "".into(),
                  prefix: "Parrot-security-".into(), suffix: "_amd64.iso".into(), reject: "".into() }, Some("signed-hashes.txt")),
        e("mint", "Linux Mint Cinnamon", Family::Linux, "newest stable",
          Source::Chain { root: "https://mirrors.kernel.org/linuxmint/stable/".into(), dir_prefix: "2".into(),
                  sub: "".into(), prefix: "linuxmint-".into(), suffix: "-cinnamon-64bit.iso".into(), reject: "".into() },
          Some("sha256sum.txt")),
        e("alpine", "Alpine standard", Family::Linux, "small, stable branch",
          Source::Index { dir: "https://dl-cdn.alpinelinux.org/alpine/latest-stable/releases/x86_64/".into(),
                  prefix: "alpine-standard-".into(), suffix: "-x86_64.iso".into(), reject: "".into() }, None),
        e("rocky", "Rocky Linux minimal", Family::Linux, "RHEL rebuild",
          Source::Chain { root: "https://download.rockylinux.org/pub/rocky/".into(), dir_prefix: "".into(),
                  sub: "isos/x86_64/".into(), prefix: "Rocky-".into(), suffix: "-x86_64-minimal.iso".into(), reject: "".into() },
          Some("CHECKSUM")),
        e("alma", "AlmaLinux minimal", Family::Linux, "RHEL rebuild",
          Source::Chain { root: "https://repo.almalinux.org/almalinux/".into(), dir_prefix: "".into(),
                  sub: "isos/x86_64/".into(), prefix: "AlmaLinux-".into(), suffix: "-x86_64-minimal.iso".into(), reject: "beta".into() },
          Some("CHECKSUM")),
        e("qubes", "Qubes OS", Family::Linux, "compartmentalized workstation",
          Source::Index { dir: "https://mirrors.edge.kernel.org/qubes/iso/".into(),
                  prefix: "Qubes-R".into(), suffix: "-x86_64.iso".into(), reject: "rc".into() }, None),
        e("tails", "Tails", Family::Linux, "amnesic live system, routes through Tor",
          Source::Chain { root: "https://mirrors.kernel.org/tails/stable/".into(), dir_prefix: "tails-amd64-".into(),
                  sub: "".into(), prefix: "tails-amd64-".into(), suffix: ".iso".into(), reject: "".into() }, None),
        e("proxmox", "Proxmox VE", Family::Linux, "hypervisor installer",
          Source::Index { dir: "https://enterprise.proxmox.com/iso/".into(),
                  prefix: "proxmox-ve_".into(), suffix: ".iso".into(), reject: "".into() }, None),
        e("freebsd", "FreeBSD disc1", Family::Tool, "not Linux, still boots",
          Source::Chain { root: "https://download.freebsd.org/releases/amd64/amd64/ISO-IMAGES/".into(),
                  dir_prefix: "".into(), sub: "".into(), prefix: "FreeBSD-".into(),
                  suffix: "-RELEASE-amd64-disc1.iso".into(), reject: "".into() }, None),

        // ---- rescue and firmware -----------------------------------------------------------
        e("systemrescue", "SystemRescue", Family::Tool, "repair, partitioning, data recovery",
          Source::Index { dir: "https://www.system-rescue.org/Download/".into(),
                  prefix: "systemrescue-".into(), suffix: "-amd64.iso".into(), reject: "".into() }, None),
        e("rescuezilla", "Rescuezilla", Family::Tool, "backup, imaging and recovery, graphical",
          Source::Github { repo: "rescuezilla/rescuezilla".into(), suffix: ".iso".into() }, None),
        e("netbootxyz", "netboot.xyz", Family::Tool, "tiny image that boots anything else over the network",
          Source::Github { repo: "netbootxyz/netboot.xyz".into(), suffix: "netboot.xyz-multiarch.iso".into() }, None),

        // ---- Windows ------------------------------------------------------------------------
        e("win11", "Windows 11", Family::Windows, "official image; Microsoft often refuses automated requests, then use win11-eval",
          Source::Windows("https://www.microsoft.com/en-us/software-download/windows11".into()), None),
        e("win10", "Windows 10", Family::Windows, "official image; Microsoft often refuses automated requests, then use win11-eval",
          Source::Windows("https://www.microsoft.com/en-us/software-download/windows10ISO".into()), None),
        e("win11-eval", "Windows 11 Enterprise (90 day eval)", Family::Windows,
          "official Microsoft evaluation image, direct download, no account",
          Source::Fwlink("https://go.microsoft.com/fwlink/p/?LinkID=2289031".into()), None),
        e("winserver-eval", "Windows Server (180 day eval)", Family::Windows,
          "official Microsoft evaluation image, direct download, no account",
          Source::Fwlink("https://go.microsoft.com/fwlink/p/?LinkID=2195280".into()), None),
        e("tiny11", "Tiny11 23H2", Family::Windows, "trimmed Windows 11 for old hardware; still needs a Windows licence",
          Source::Archive("tiny11-2311".into()), None),
        e("tiny11-core", "Tiny11 Core", Family::Windows, "smallest Windows 11 build, test use; still needs a licence",
          Source::Archive("tiny-11-core-x-64-beta-1".into()), None),
        e("tiny10", "Tiny10 x64", Family::Windows, "trimmed Windows 10; still needs a Windows licence",
          Source::Archive("tiny-10-23-h2".into()), None),
    ]
}

/// Built-in entries, with a user or system TSV layered over them by id.
pub fn all() -> Vec<Iso> {
    let mut list = builtin();
    for path in overlay_paths() {
        let body = match std::fs::read_to_string(&path) { Ok(b) => b, Err(_) => continue };
        for iso in parse_tsv(&body) {
            match list.iter().position(|x| x.id == iso.id) {
                Some(i) => list[i] = iso,
                None => list.push(iso),
            }
        }
    }
    list
}

fn overlay_paths() -> Vec<String> {
    let mut v = Vec::new();
    if let Some(p) = std::env::var_os("ARXBURN_CATALOG") { v.push(p.to_string_lossy().into_owned()); }
    if let Some(h) = std::env::var_os("HOME") {
        v.push(format!("{}/.config/arxburn/catalog.tsv", h.to_string_lossy()));
    }
    v.push("/etc/arxburn/catalog.tsv".into());
    v
}

/// id \t name \t family \t note \t source \t sums
/// source: direct:URL | index:DIR|PREFIX|SUFFIX | chain:ROOT|DIRPREFIX|SUB|PREFIX|SUFFIX
///         | archive:ID | windows:URL
pub fn parse_tsv(body: &str) -> Vec<Iso> {
    let mut out = Vec::new();
    for line in body.lines() {
        let line = line.trim_end();
        if line.is_empty() || line.starts_with('#') { continue; }
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 5 { continue; }
        let src = match parse_source(f[4]) { Some(s) => s, None => continue };
        out.push(Iso {
            id: f[0].trim().into(), name: f[1].trim().into(), family: Family::parse(f[2]),
            note: f[3].trim().into(), src,
            sums: f.get(5).map(|s| s.trim()).filter(|s| !s.is_empty()).map(String::from),
        });
    }
    out
}

fn parse_source(spec: &str) -> Option<Source> {
    let (kind, rest) = spec.split_once(':')?;
    let p: Vec<&str> = rest.split('|').collect();
    Some(match kind.trim() {
        "direct" => Source::Direct(rest.into()),
        "archive" => Source::Archive(rest.into()),
        "windows" => Source::Windows(rest.into()),
        "github" if p.len() >= 2 => Source::Github { repo: p[0].into(), suffix: p[1].into() },
        "fwlink" => Source::Fwlink(rest.into()),
        "index" if p.len() >= 3 => Source::Index { dir: p[0].into(), prefix: p[1].into(), suffix: p[2].into(),
                                                   reject: p.get(3).copied().unwrap_or("").into() },
        "chain" if p.len() >= 5 => Source::Chain { root: p[0].into(), dir_prefix: p[1].into(),
                                                   sub: p[2].into(), prefix: p[3].into(), suffix: p[4].into(),
                                                   reject: p.get(5).copied().unwrap_or("").into() },
        _ => return None,
    })
}

fn join(dir: &str, name: &str) -> String {
    if name.starts_with("http") { return name.to_string(); }
    let name = name.trim_start_matches("./");
    if dir.ends_with('/') { format!("{dir}{name}") } else { format!("{dir}/{name}") }
}

/// Returns (filename, href). The href is kept because plenty of "directories" are really
/// download pages whose links point at a CDN somewhere else entirely.
fn newest(body: &str, prefix: &str, suffix: &str, reject: &str) -> Option<(String, String)> {
    let mut best: Option<(String, String)> = None;
    for h in net::hrefs(body) {
        let name = percent_decode(h.rsplit('/').next().unwrap_or(&h));
        if !name.starts_with(prefix) || !name.ends_with(suffix) || name.contains("..") { continue; }
        if rejected(&name, reject) { continue; }
        best = match best {
            Some(b) if net::natural_newer(&name, &b.0) != std::cmp::Ordering::Greater => Some(b),
            _ => Some((name, h)),
        };
    }
    best
}

/// True when the name contains any of the comma separated reject substrings.
fn rejected(name: &str, reject: &str) -> bool {
    reject.split(',').map(str::trim).any(|r| !r.is_empty() && name.contains(r))
}

fn newest_dir(body: &str, prefix: &str, reject: &str) -> Option<String> {
    let mut best: Option<String> = None;
    for h in net::hrefs(body) {
        if !h.ends_with('/') || h.starts_with("http") || h.starts_with('/') || h.starts_with('?') || h.starts_with("..") { continue; }
        let d = h.trim_end_matches('/').to_string();
        if !d.starts_with(prefix) || d.is_empty() || rejected(&d, reject) { continue; }
        // a versioned directory starts with a digit somewhere; "test/" and "sources/" do not qualify
        if !d.chars().any(|c| c.is_ascii_digit()) { continue; }
        best = match best {
            Some(b) if net::natural_newer(&d, &b) != std::cmp::Ordering::Greater => Some(b),
            _ => Some(d),
        };
    }
    best
}

fn sha_from_sums(body: &str, filename: &str) -> Option<String> {
    for line in body.lines() {
        if !line.contains(filename) { continue; }
        // "<hash>  <name>" is the usual shape; CHECKSUM files use "SHA256 (name) = <hash>"
        if let Some(h) = line.split_whitespace().next() {
            if h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()) { return Some(h.to_lowercase()); }
        }
        if let Some(h) = line.rsplit('=').next() {
            let h = h.trim();
            if h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()) { return Some(h.to_lowercase()); }
        }
    }
    None
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) { out.push(v); i += 3; continue; }
        }
        out.push(b[i]); i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || "-._~/".contains(c) { out.push(c); }
        else { for b in c.to_string().as_bytes() { out.push_str(&format!("%{b:02X}")); } }
    }
    out
}

pub fn resolve(iso: &Iso) -> Result<Resolved, String> {
    match &iso.src {
        Source::Direct(url) => {
            let filename = url.rsplit('/').next().unwrap_or("image.iso").to_string();
            let sha = iso.sums.as_ref().and_then(|s| {
                let base = url.rsplit_once('/').map(|(d, _)| d.to_string()).unwrap_or_default();
                net::text(&join(&base, s)).ok().and_then(|b| sha_from_sums(&b, &filename))
            });
            Ok(Resolved { url: url.clone(), filename, sha256: sha })
        }
        Source::Index { dir, prefix, suffix, reject } => {
            let body = net::text(dir)?;
            let (name, href) = newest(&body, prefix, suffix, reject)
                .ok_or_else(|| format!("no image matching {prefix}*{suffix} at {dir}"))?;
            let sha = iso.sums.as_ref()
                .and_then(|s| net::text(&join(dir, s)).ok())
                .and_then(|b| sha_from_sums(&b, &name));
            Ok(Resolved { url: join(dir, &href), filename: name, sha256: sha })
        }
        Source::Chain { root, dir_prefix, sub, prefix, suffix, reject } => {
            let body = net::text(root)?;
            let newest_d = newest_dir(&body, dir_prefix, reject)
                .ok_or_else(|| format!("no versioned directory under {root}"))?;
            let dir = join(&join(root, &newest_d), sub);
            let dir = if dir.ends_with('/') { dir } else { format!("{dir}/") };
            let body = net::text(&dir)?;
            let (name, href) = newest(&body, prefix, suffix, reject)
                .ok_or_else(|| format!("no image matching {prefix}*{suffix} at {dir}"))?;
            let sha = iso.sums.as_ref()
                .and_then(|s| net::text(&join(&dir, s)).ok())
                .and_then(|b| sha_from_sums(&b, &name));
            Ok(Resolved { url: join(&dir, &href), filename: name, sha256: sha })
        }
        Source::Archive(id) => {
            let meta = net::text(&format!("https://archive.org/metadata/{id}"))?;
            let name = largest_iso(&meta).ok_or_else(|| format!("archive.org item {id} has no .iso"))?;
            Ok(Resolved { url: format!("https://archive.org/download/{id}/{}", percent_encode(&name)),
                          filename: name, sha256: None })
        }
        Source::Github { repo, suffix } => {
            let api = format!("https://api.github.com/repos/{repo}/releases/latest");
            let body = net::text(&api)?;
            let url = body.split("\"browser_download_url\":").skip(1)
                .filter_map(|c| c.split('"').nth(1))
                .find(|u| u.ends_with(suffix.as_str()))
                .ok_or_else(|| format!("newest {repo} release has no asset ending in {suffix}"))?
                .to_string();
            let filename = url.rsplit('/').next().unwrap_or("image.iso").to_string();
            Ok(Resolved { url, filename, sha256: None })
        }
        Source::Fwlink(url) => {
            let real = net::final_url(url).ok_or("Microsoft's link did not redirect anywhere")?;
            let filename = real.split('?').next().unwrap_or(&real)
                .rsplit('/').next().unwrap_or("windows.iso").to_string();
            Ok(Resolved { url: real, filename, sha256: None })
        }
        Source::Windows(page) => windows(page),
    }
}

/// archive.org metadata is JSON; we want the biggest .iso in it and nothing else, so a scan for
/// the two fields beats pulling in a parser.
fn largest_iso(meta: &str) -> Option<String> {
    let mut best: Option<(u64, String)> = None;
    for chunk in meta.split("{\"name\":\"").skip(1) {
        let name = chunk.split('"').next()?.to_string();
        if !name.to_ascii_lowercase().ends_with(".iso") { continue; }
        let size = chunk.split("\"size\":\"").nth(1)
            .and_then(|s| s.split('"').next())
            .and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
        if best.as_ref().map(|(b, _)| size > *b).unwrap_or(true) { best = Some((size, name)); }
    }
    best.map(|(_, n)| n.replace("\\/", "/"))
}

/// Microsoft publishes Windows images through a download connector rather than a plain link.
/// This walks the same three steps a browser does. If they change it, the failure says so and
/// points at the page, instead of pretending there is no way to get Windows.
fn windows(page: &str) -> Result<Resolved, String> {
    let fail = |why: String| format!(
        "{why}\n     Microsoft hands consumer images out through a browser session, and refuses \
         requests it thinks are automated.\n     Either download it from {page} yourself and run \
         `arxburn write <file> --to <device>`,\n     or use the official evaluation image, which \
         needs no account: `arxburn get win11-eval`");
    let session = std::fs::read_to_string("/proc/sys/kernel/random/uuid")
        .map_err(|e| fail(format!("no uuid source: {e}")))?.trim().to_string();
    let body = net::text_with(page, &["User-Agent: Mozilla/5.0 (X11; Linux x86_64; rv:128.0) Gecko/20100101 Firefox/128.0".to_string()])
        .map_err(|e| fail(e))?;
    let edition = body.split("option value=\"").skip(1)
        .filter_map(|c| c.split('"').next())
        .find(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
        .ok_or_else(|| fail("Microsoft's page no longer lists a product edition".into()))?
        .to_string();
    // registering the session is what makes the connector answer at all
    let _ = net::text(&format!("https://vlscppe.microsoft.com/fp/tags?org_id=y6jn8c31&session_id={session}"));
    let hdrs = vec![format!("Referer: {page}"),
                    "Accept: application/json, text/plain, */*".to_string(),
                    "Accept-Language: en-US,en;q=0.9".to_string(),
                    // their connector drops anything that does not look like a browser
                    "User-Agent: Mozilla/5.0 (X11; Linux x86_64; rv:128.0) Gecko/20100101 Firefox/128.0".to_string()];
    let sku_json = net::text_with(&format!(
        "https://www.microsoft.com/software-download-connector/api/getskuinformationbyproductedition\
         ?profile=606624d44497&ProductEditionId={edition}&SKU=undefined&friendlyFileName=undefined\
         &Locale=en-US&sessionID={session}"), &hdrs).map_err(|e| fail(e))?;
    let pick = |lang: &str| sku_json.split("{\"Id\":\"").skip(1)
        .find(|c| c.split("\"Language\":\"").nth(1).map(|l| l.starts_with(lang)).unwrap_or(false))
        .and_then(|c| c.split('"').next()).map(str::to_string);
    let sku = pick("English\"").or_else(|| pick("English"))
        .or_else(|| sku_json.split("{\"Id\":\"").nth(1).and_then(|c| c.split('"').next()).map(str::to_string))
        .ok_or_else(|| fail(if sku_json.contains("SentinelReject") {
            "Microsoft's anti-bot rejected this request".into()
        } else { "Microsoft listed no edition to download".into() }))?;
    let links = net::text_with(&format!(
        "https://www.microsoft.com/software-download-connector/api/GetProductDownloadLinksBySku\
         ?profile=606624d44497&ProductEditionId=undefined&SKU={sku}&friendlyFileName=undefined\
         &Locale=en-US&sessionID={session}"), &hdrs).map_err(|e| fail(e))?;
    let url = links.split("\"Uri\":\"").nth(1).and_then(|c| c.split('"').next())
        .map(|u| u.replace("\\u0026", "&").replace("\\/", "/"))
        .ok_or_else(|| fail(if links.contains("SentinelReject") {
            "Microsoft's anti-bot rejected this request".into()
        } else { "Microsoft returned no download link".into() }))?;
    let filename = url.split('?').next().unwrap_or("windows.iso")
        .rsplit('/').next().unwrap_or("windows.iso").to_string();
    Ok(Resolved { url, filename, sha256: None })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newest_picks_the_newest_not_the_last() {
        let page = r#"<a href="archlinux-2026.08.01-x86_64.iso">a</a>
                      <a href="archlinux-2026.09.01-x86_64.iso">b</a>
                      <a href="archlinux-2026.07.01-x86_64.iso">c</a>
                      <a href="archlinux-bootstrap-2026.09.01-x86_64.tar.zst">d</a>"#;
        assert_eq!(newest(page, "archlinux-", "-x86_64.iso", "").unwrap().0, "archlinux-2026.09.01-x86_64.iso");
    }

    #[test]
    fn versioned_directories_beat_string_order() {
        let page = r#"<a href="9.10/">9.10</a><a href="24.04/">24.04</a><a href="sources/">sources</a>"#;
        assert_eq!(newest_dir(page, "", "").unwrap(), "24.04");
    }

    #[test]
    fn checksums_are_read_from_both_common_layouts() {
        let plain = "abc  other.iso\n1111111111111111111111111111111111111111111111111111111111111111  x.iso\n";
        assert_eq!(sha_from_sums(plain, "x.iso").unwrap(), "1".repeat(64));
        let bsd = "SHA256 (y.iso) = 2222222222222222222222222222222222222222222222222222222222222222\n";
        assert_eq!(sha_from_sums(bsd, "y.iso").unwrap(), "2".repeat(64));
    }

    #[test]
    fn a_user_catalog_overrides_a_builtin_entry() {
        let tsv = "arch\tArch mine\tlinux\tlocal mirror\tdirect:http://x/y.iso\t\nzed\tZed\tlinux\tn\tindex:http://d/|p|.iso\t";
        let got = parse_tsv(tsv);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].id, "arch");
        assert!(matches!(got[1].src, Source::Index { .. }));
    }

    #[test]
    fn a_reject_keeps_the_variant_out() {
        // Debian ships debian-mac-13.7.0-amd64-netinst.iso next to the normal one, and "m"
        // sorts after "1", so without the reject the mac build wins every time.
        let page = r#"<a href="debian-13.7.0-amd64-netinst.iso">a</a>
                      <a href="debian-mac-13.7.0-amd64-netinst.iso">b</a>"#;
        let got = newest(page, "debian-", "-amd64-netinst.iso", "debian-mac").unwrap();
        assert_eq!(got.0, "debian-13.7.0-amd64-netinst.iso");
    }

    #[test]
    fn the_largest_iso_in_an_archive_item_wins() {
        let meta = r#"{"files":[{"name":"small.iso","size":"100"},{"name":"readme.txt","size":"999999"},{"name":"big.iso","size":"5000"}]}"#;
        assert_eq!(largest_iso(meta).unwrap(), "big.iso");
    }
}
