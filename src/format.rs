// What an image file actually is, read from its bytes rather than trusted from its name.
//
// Going by the extension is how a burn tool writes a .zip, a VM disk or a tarball to a stick,
// reports success, and leaves a stick that boots nothing. So the first bytes decide:
//   - an ISO 9660 / UDF image or a raw disk image is written as it is;
//   - a compressed image (xz, zstd, gzip, bzip2) is decompressed on the fly by the system's own
//     tool, which also checks the stream's integrity data. std only, no crates: the same choice
//     as net.rs using curl for downloads, so the binary still builds and runs with nothing;
//   - formats that are never right to write raw are refused, with what to do instead.
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::process::{Child, Command, Stdio};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression { Xz, Zstd, Gzip, Bzip2 }

impl Compression {
    /// Also the name of the system tool that decompresses it.
    pub fn name(self) -> &'static str {
        match self { Compression::Xz => "xz", Compression::Zstd => "zstd", Compression::Gzip => "gzip", Compression::Bzip2 => "bzip2" }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// ISO 9660 and/or UDF (Linux live ISOs, Windows installers). `hybrid` means it also carries
    /// a partition table, which is what lets a USB stick boot it.
    Optical { udf: bool, hybrid: bool },
    /// A raw disk image: a partition table or boot sector at the front (.img, .raw, appliances).
    Disk { mbr: bool, gpt: bool },
    Compressed(Compression),
    /// Never right to write raw. The text says why and what to do instead.
    Refuse(&'static str),
    /// No structure we recognise. It is still written as it is (that was always allowed), with a
    /// warning, because some valid images (bare filesystems, firmware blobs) carry no table.
    Unknown,
}

impl Kind {
    pub fn label(&self) -> String {
        match self {
            Kind::Optical { udf: true, hybrid: true } => "ISO image (UDF, hybrid: boots from USB)".into(),
            Kind::Optical { udf: true, hybrid: false } => "ISO image (UDF)".into(),
            Kind::Optical { udf: false, hybrid: true } => "ISO image (hybrid: boots from USB)".into(),
            Kind::Optical { udf: false, hybrid: false } => "ISO image".into(),
            Kind::Disk { gpt: true, .. } => "disk image (GPT)".into(),
            Kind::Disk { .. } => "disk image (MBR)".into(),
            Kind::Compressed(c) => format!("{}-compressed image", c.name()),
            Kind::Refuse(why) => format!("not writable: {why}"),
            Kind::Unknown => "unrecognised image".into(),
        }
    }
    /// The short machine-readable kind for --json.
    pub fn code(&self) -> &'static str {
        match self {
            Kind::Optical { .. } => "iso", Kind::Disk { .. } => "disk", Kind::Compressed(_) => "compressed",
            Kind::Refuse(_) => "refused", Kind::Unknown => "unknown",
        }
    }
}

/// How much of the front we read: the ISO 9660 / UDF volume descriptors live in sectors 16..32
/// (2048-byte sectors), so 64 KiB covers them and every signature below.
pub const HEAD: usize = 64 * 1024;

pub fn sniff(head: &[u8]) -> Kind {
    let at = |off: usize, sig: &[u8]| head.len() >= off + sig.len() && &head[off..off + sig.len()] == sig;

    // compression containers: their first bytes are unambiguous
    if at(0, &[0xFD, b'7', b'z', b'X', b'Z', 0x00]) { return Kind::Compressed(Compression::Xz); }
    if at(0, &[0x28, 0xB5, 0x2F, 0xFD]) { return Kind::Compressed(Compression::Zstd); }
    if at(0, &[0x1F, 0x8B, 0x08]) { return Kind::Compressed(Compression::Gzip); }
    if at(0, b"BZh") && head.len() > 3 && (b'1'..=b'9').contains(&head[3]) { return Kind::Compressed(Compression::Bzip2); }

    // archives hold files, not a disk
    if at(0, b"PK\x03\x04") { return Kind::Refuse("it is a .zip archive; unpack it and write the image inside"); }
    if at(0, &[b'7', b'z', 0xBC, 0xAF, 0x27, 0x1C]) { return Kind::Refuse("it is a .7z archive; unpack it and write the image inside"); }
    if at(0, b"Rar!\x1A\x07") { return Kind::Refuse("it is a .rar archive; unpack it and write the image inside"); }
    if at(257, b"ustar") { return Kind::Refuse("it is a tar archive; unpack it and write the image inside"); }

    // virtual machine disks: their bytes are a container, not the disk
    if at(0, b"QFI\xfb") { return Kind::Refuse("it is a QEMU virtual disk (qcow2); convert it first: qemu-img convert -O raw in.qcow2 out.img"); }
    if at(0, b"vhdxfile") { return Kind::Refuse("it is a Hyper-V virtual disk (VHDX); convert it first: qemu-img convert -O raw in.vhdx out.img"); }
    if at(0, b"KDMV") { return Kind::Refuse("it is a VMware virtual disk (VMDK); convert it first: qemu-img convert -O raw in.vmdk out.img"); }
    if at(0, b"conectix") { return Kind::Refuse("it is a dynamic VHD virtual disk; convert it first: qemu-img convert -O raw in.vhd out.img"); }
    if at(0, b"<<< Oracle VM VirtualBox Disk Image >>>") { return Kind::Refuse("it is a VirtualBox virtual disk (VDI); convert it first: qemu-img convert -O raw in.vdi out.img"); }

    // optical: identifiers sit one byte into each 2048-byte descriptor from sector 16
    let (mut iso, mut udf) = (false, false);
    for sector in 16..32 {
        let off = sector * 2048 + 1;
        if at(off, b"CD001") { iso = true; }
        if at(off, b"BEA01") || at(off, b"NSR02") || at(off, b"NSR03") { udf = true; }
    }
    let mbr = head.len() >= 512 && head[510] == 0x55 && head[511] == 0xAA;
    let gpt = at(512, b"EFI PART") || at(4096, b"EFI PART");
    if iso || udf { return Kind::Optical { udf, hybrid: mbr || gpt }; }
    if mbr || gpt { return Kind::Disk { mbr, gpt }; }
    Kind::Unknown
}

/// Read the head (and the last sector, where an Apple DMG keeps its signature) and classify.
pub fn sniff_file(path: &Path) -> std::io::Result<(Kind, Vec<u8>)> {
    let mut f = std::fs::File::open(path)?;
    let head = read_up_to(&mut f, HEAD)?;
    let kind = sniff(&head);
    if kind == Kind::Unknown || matches!(kind, Kind::Disk { .. }) {
        let len = f.metadata()?.len();
        if len >= 512 {
            f.seek(SeekFrom::Start(len - 512))?;
            let tail = read_up_to(&mut f, 512)?;
            if tail.starts_with(b"koly") {
                return Ok((Kind::Refuse("it is an Apple disk image (DMG); it cannot be written to a stick as it is"), head));
            }
        }
    }
    Ok((kind, head))
}

fn read_up_to(r: &mut impl Read, max: usize) -> std::io::Result<Vec<u8>> {
    let mut buf = vec![0u8; max];
    let mut filled = 0;
    while filled < max {
        match r.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    buf.truncate(filled);
    Ok(buf)
}

/// Is the decompressor installed? xz, zstd, gzip and bzip2 are in every Arch base system and on
/// most distributions, but a minimal system can lack zstd.
pub fn tool_present(c: Compression) -> bool {
    Command::new(c.name()).arg("--version").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
        .status().map(|s| s.success()).unwrap_or(false)
}

/// Start the system decompressor on `path`, writing the decompressed image to its stdout.
pub fn decompress(c: Compression, path: &Path) -> std::io::Result<Child> {
    Command::new(c.name()).arg("-dc").arg("--").arg(path)
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()
}

/// Same, but the compressed bytes are fed on stdin. The burn path uses this so it reads the file
/// itself and hashes it in the same pass: that keeps the guarantee that what was written is what
/// was checked before anything was erased, compressed or not.
pub fn decompress_stdin(c: Compression) -> std::io::Result<Child> {
    Command::new(c.name()).arg("-dc")
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()
}

/// The decompressed size, when the container records it. xz always does; zstd does when the
/// frame says so. gzip stores it modulo 4 GiB, which is wrong for exactly the images that matter,
/// and bzip2 not at all: those return None and the write runs with the total unknown.
pub fn uncompressed_size(c: Compression, path: &Path) -> Option<u64> {
    match c {
        Compression::Xz => {
            let out = Command::new("xz").args(["--robot", "--list", "--"]).arg(path).output().ok()?;
            let text = String::from_utf8_lossy(&out.stdout);
            let totals = text.lines().find(|l| l.starts_with("totals\t"))?;
            totals.split('\t').nth(4)?.parse().ok()
        }
        Compression::Zstd => {
            let out = Command::new("zstd").args(["-lv", "--"]).arg(path).output().ok()?;
            let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
            let line = text.lines().find(|l| l.trim_start().starts_with("Decompressed Size:"))?;
            let inner = line.rsplit_once('(')?.1;
            inner.split_whitespace().next()?.parse().ok()
        }
        Compression::Gzip | Compression::Bzip2 => None,
    }
}

/// The first HEAD bytes of the decompressed image, to see what is inside before anything is erased.
pub fn decompressed_head(c: Compression, path: &Path) -> std::io::Result<Vec<u8>> {
    let mut child = decompress(c, path)?;
    let mut out = child.stdout.take().ok_or_else(|| std::io::Error::other("no decompressor output"))?;
    let head = read_up_to(&mut out, HEAD);
    drop(out); // closing the pipe ends the decompressor; we only wanted the front
    let _ = child.kill();
    let _ = child.wait();
    head
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with(off: usize, sig: &[u8], len: usize) -> Vec<u8> {
        let mut v = vec![0u8; len.max(off + sig.len())];
        v[off..off + sig.len()].copy_from_slice(sig);
        v
    }

    #[test]
    fn compression_magics() {
        assert_eq!(sniff(&with(0, &[0xFD, b'7', b'z', b'X', b'Z', 0], 64)), Kind::Compressed(Compression::Xz));
        assert_eq!(sniff(&with(0, &[0x28, 0xB5, 0x2F, 0xFD], 64)), Kind::Compressed(Compression::Zstd));
        assert_eq!(sniff(&with(0, &[0x1F, 0x8B, 0x08], 64)), Kind::Compressed(Compression::Gzip));
        assert_eq!(sniff(&with(0, b"BZh9", 64)), Kind::Compressed(Compression::Bzip2));
        // "BZh" without a block-size digit is not bzip2
        assert_ne!(sniff(&with(0, b"BZhx", 64)), Kind::Compressed(Compression::Bzip2));
    }

    #[test]
    fn archives_and_vm_disks_are_refused() {
        for (off, sig) in [(0, &b"PK\x03\x04"[..]), (0, &[b'7', b'z', 0xBC, 0xAF, 0x27, 0x1C][..]),
                           (257, &b"ustar"[..]), (0, &b"QFI\xfb"[..]), (0, &b"vhdxfile"[..]),
                           (0, &b"KDMV"[..]), (0, &b"conectix"[..]), (0, &b"Rar!\x1A\x07"[..])] {
            assert!(matches!(sniff(&with(off, sig, 1024)), Kind::Refuse(_)), "{sig:?} should be refused");
        }
    }

    #[test]
    fn optical_images() {
        let mut v = with(16 * 2048 + 1, b"CD001", HEAD);
        assert_eq!(sniff(&v), Kind::Optical { udf: false, hybrid: false });
        v[510] = 0x55; v[511] = 0xAA; // isohybrid: the MBR is what makes a stick boot it
        assert_eq!(sniff(&v), Kind::Optical { udf: false, hybrid: true });
        // a Windows installer: ISO 9660 bridge plus UDF further into the descriptor set
        v[18 * 2048 + 1..18 * 2048 + 6].copy_from_slice(b"NSR02");
        assert_eq!(sniff(&v), Kind::Optical { udf: true, hybrid: true });
        // UDF only, no ISO 9660 at all
        assert_eq!(sniff(&with(17 * 2048 + 1, b"BEA01", HEAD)), Kind::Optical { udf: true, hybrid: false });
    }

    #[test]
    fn raw_disks_and_unknown() {
        let mut mbr = vec![0u8; 4096]; mbr[510] = 0x55; mbr[511] = 0xAA;
        assert_eq!(sniff(&mbr), Kind::Disk { mbr: true, gpt: false });
        let mut gpt = with(512, b"EFI PART", 4096); gpt[510] = 0x55; gpt[511] = 0xAA; // protective MBR
        assert_eq!(sniff(&gpt), Kind::Disk { mbr: true, gpt: true });
        assert_eq!(sniff(&with(4096, b"EFI PART", 8192)), Kind::Disk { mbr: false, gpt: true }); // 4K sectors
        assert_eq!(sniff(&vec![0u8; 4096]), Kind::Unknown);
        assert_eq!(sniff(&[]), Kind::Unknown);
    }

    #[test]
    fn real_decompression_and_sizes() {
        // round-trip a small disk image through every system decompressor that is installed
        let dir = std::env::temp_dir().join(format!("arxburn-fmt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut img = vec![0u8; 300_000];
        for (i, b) in img.iter_mut().enumerate() { *b = (i * 7 % 251) as u8; }
        img[510] = 0x55; img[511] = 0xAA;
        let raw = dir.join("t.img");
        std::fs::write(&raw, &img).unwrap();
        for c in [Compression::Xz, Compression::Zstd, Compression::Gzip, Compression::Bzip2] {
            if !tool_present(c) { continue; }
            let st = Command::new(c.name()).args(["-k", "-f", "-q", "--"]).arg(&raw).status().unwrap();
            assert!(st.success(), "{} could not compress the fixture", c.name());
            let ext = match c { Compression::Xz => "xz", Compression::Zstd => "zst", Compression::Gzip => "gz", Compression::Bzip2 => "bz2" };
            let packed = dir.join(format!("t.img.{ext}"));
            let (kind, _) = sniff_file(&packed).unwrap();
            assert_eq!(kind, Kind::Compressed(c));
            let inner = decompressed_head(c, &packed).unwrap();
            assert_eq!(sniff(&inner), Kind::Disk { mbr: true, gpt: false }, "{} head", c.name());
            let mut child = decompress(c, &packed).unwrap();
            let mut all = Vec::new();
            child.stdout.take().unwrap().read_to_end(&mut all).unwrap();
            assert!(child.wait().unwrap().success());
            assert_eq!(all, img, "{} round trip", c.name());
            match c {
                Compression::Xz | Compression::Zstd => assert_eq!(uncompressed_size(c, &packed), Some(img.len() as u64)),
                _ => assert_eq!(uncompressed_size(c, &packed), None),
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
