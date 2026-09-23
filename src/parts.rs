//! What is actually in the first sectors of an image, and switching a stick between schemes.
//!
//! A modern installer image is usually a HYBRID: a real MBR at sector 0 so an old BIOS will boot
//! it, a GPT at sector 1 so UEFI firmware will, and an El Torito catalog inside the ISO9660
//! filesystem. Writing it to a stick copies all three, and almost every machine then boots.
//!
//! Almost. Some firmware refuses a stick whose first sector looks like an MBR, and some old
//! BIOSes refuse one that looks like GPT, which is what a "partition scheme" choice is for. This
//! module can read the structures and, after a write has been verified, leave the stick looking
//! like one scheme or the other:
//!
//!   mbr  the GPT headers are cleared, so firmware sees a plain MBR disk
//!   gpt  the MBR is replaced by a protective one, so firmware sees a plain GPT disk
//!
//! Neither touches the filesystems or the boot files, and neither happens before the read back
//! verification: the stick is proved to match the image first, and only then deliberately changed.

pub const SECTOR: usize = 512;
const GPT_MAGIC: &[u8; 8] = b"EFI PART";

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Entry {
    pub bootable: bool,
    pub kind: u8,
    pub start_lba: u32,
    pub sectors: u32,
}

impl Entry {
    fn parse(b: &[u8]) -> Option<Entry> {
        if b.len() < 16 { return None; }
        let e = Entry {
            bootable: b[0] == 0x80,
            kind: b[4],
            start_lba: u32::from_le_bytes([b[8], b[9], b[10], b[11]]),
            sectors: u32::from_le_bytes([b[12], b[13], b[14], b[15]]),
        };
        if e.kind == 0 && e.sectors == 0 { None } else { Some(e) }
    }

    pub fn kind_name(&self) -> &'static str {
        match self.kind {
            0x00 => "unused or raw",
            0x0b | 0x0c => "FAT32",
            0x07 => "NTFS or exFAT",
            0x83 => "Linux",
            0x8e => "Linux LVM",
            0xee => "GPT protective",
            0xef => "EFI system",
            0x17 => "hidden NTFS",
            _ => "other",
        }
    }
}

#[derive(Debug, Default, PartialEq)]
pub struct Scheme {
    pub mbr: bool,
    pub gpt: bool,
    pub el_torito: bool,
    pub efi_partition: bool,
    pub protective_only: bool,
    pub entries: Vec<Entry>,
}

impl Scheme {
    /// What firmware will do with it, in a sentence someone can act on.
    pub fn boots(&self) -> String {
        let bios = self.mbr && !self.protective_only;
        match (bios, self.gpt || self.efi_partition) {
            (true, true) => "hybrid: boots on UEFI and on old BIOS machines".into(),
            (false, true) => "UEFI only".into(),
            (true, false) => "BIOS only (no EFI partition and no GPT)".into(),
            (false, false) => "no partition table found; the firmware may still boot it if it is a raw disk image".into(),
        }
    }

    pub fn summary(&self) -> String {
        let mut bits = Vec::new();
        if self.mbr { bits.push(if self.protective_only { "protective MBR".to_string() } else { "MBR".to_string() }); }
        if self.gpt { bits.push("GPT".into()); }
        if self.el_torito { bits.push("El Torito".into()); }
        if bits.is_empty() { "no partition table".into() } else { bits.join(" + ") }
    }
}

/// Read the first two sectors plus the ISO9660 boot record area.
///
/// `head` must start at byte 0 of the image. Anything missing is simply reported as absent: this
/// never guesses, because the answer decides whether somebody's machine boots.
pub fn read(head: &[u8]) -> Scheme {
    let mut s = Scheme::default();
    if head.len() >= SECTOR && head[510] == 0x55 && head[511] == 0xaa {
        s.mbr = true;
        for i in 0..4 {
            let off = 446 + i * 16;
            if let Some(e) = Entry::parse(&head[off..off + 16]) {
                if e.kind == 0xef { s.efi_partition = true; }
                s.entries.push(e);
            }
        }
        s.protective_only = !s.entries.is_empty() && s.entries.iter().all(|e| e.kind == 0xee);
    }
    if head.len() >= SECTOR * 2 && &head[SECTOR..SECTOR + 8] == GPT_MAGIC {
        s.gpt = true;
    }
    // ISO9660 boot record: volume descriptors start at 32768, the boot record is the next one
    const BOOT_REC: usize = 34816;
    if head.len() >= BOOT_REC + 30 && &head[BOOT_REC + 7..BOOT_REC + 30] == b"EL TORITO SPECIFICATION" {
        s.el_torito = true;
    }
    s
}

/// A protective MBR: one entry of type 0xEE covering the disk, which is what a GPT disk is
/// supposed to have at sector 0 so older tools do not think it is empty.
pub fn protective_mbr(total_sectors: u64) -> [u8; SECTOR] {
    let mut mbr = [0u8; SECTOR];
    let span = u32::try_from(total_sectors.saturating_sub(1)).unwrap_or(u32::MAX);
    let e = &mut mbr[446..462];
    e[0] = 0x00;                 // not bootable
    e[1] = 0x00; e[2] = 0x02; e[3] = 0x00;   // CHS of LBA 1, by convention
    e[4] = 0xee;                 // GPT protective
    e[5] = 0xff; e[6] = 0xff; e[7] = 0xff;   // CHS "too big to express"
    e[8..12].copy_from_slice(&1u32.to_le_bytes());
    e[12..16].copy_from_slice(&span.to_le_bytes());
    mbr[510] = 0x55;
    mbr[511] = 0xaa;
    mbr
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hybrid_head() -> Vec<u8> {
        let mut h = vec![0u8; 35000];
        // an MBR with a bootable entry and an EFI system partition, the shape a hybrid ISO has
        h[446] = 0x80;
        h[446 + 4] = 0x00;
        h[446 + 8..446 + 12].copy_from_slice(&64u32.to_le_bytes());
        h[446 + 12..446 + 16].copy_from_slice(&7_572_928u32.to_le_bytes());
        h[462 + 4] = 0xef;
        h[462 + 8..462 + 12].copy_from_slice(&7_572_992u32.to_le_bytes());
        h[462 + 12..462 + 16].copy_from_slice(&675_840u32.to_le_bytes());
        h[510] = 0x55; h[511] = 0xaa;
        h[SECTOR..SECTOR + 8].copy_from_slice(GPT_MAGIC);
        h[34823..34846].copy_from_slice(b"EL TORITO SPECIFICATION");
        h
    }

    #[test]
    fn a_hybrid_image_is_recognised_as_booting_both_ways() {
        let s = read(&hybrid_head());
        assert!(s.mbr && s.gpt && s.el_torito && s.efi_partition);
        assert!(!s.protective_only);
        assert_eq!(s.summary(), "MBR + GPT + El Torito");
        assert!(s.boots().starts_with("hybrid"));
        assert_eq!(s.entries.len(), 2);
        assert_eq!(s.entries[1].kind_name(), "EFI system");
    }

    #[test]
    fn a_gpt_only_disk_says_uefi_only() {
        let mut h = vec![0u8; SECTOR * 2];
        h[..SECTOR].copy_from_slice(&protective_mbr(1000));
        h[SECTOR..SECTOR + 8].copy_from_slice(GPT_MAGIC);
        let s = read(&h);
        assert!(s.mbr && s.gpt && s.protective_only);
        assert_eq!(s.boots(), "UEFI only");
        assert_eq!(s.summary(), "protective MBR + GPT");
    }

    #[test]
    fn a_plain_mbr_disk_says_bios_only() {
        let mut h = vec![0u8; SECTOR * 2];
        h[446] = 0x80;
        h[446 + 4] = 0x0c;                        // FAT32
        h[446 + 12..446 + 16].copy_from_slice(&2048u32.to_le_bytes());
        h[510] = 0x55; h[511] = 0xaa;
        let s = read(&h);
        assert!(s.mbr && !s.gpt);
        assert_eq!(s.boots(), "BIOS only (no EFI partition and no GPT)");
        assert_eq!(s.entries[0].kind_name(), "FAT32");
    }

    #[test]
    fn nothing_at_all_is_reported_as_nothing_rather_than_guessed() {
        let s = read(&vec![0u8; SECTOR * 2]);
        assert!(!s.mbr && !s.gpt);
        assert_eq!(s.summary(), "no partition table");
        assert!(s.boots().starts_with("no partition table"));
    }

    #[test]
    fn a_protective_mbr_covers_the_disk_and_claims_no_boot() {
        let m = protective_mbr(1_000_000);
        assert_eq!(m[510], 0x55);
        assert_eq!(m[511], 0xaa);
        assert_eq!(m[446], 0x00, "a protective entry is never bootable");
        assert_eq!(m[446 + 4], 0xee);
        let start = u32::from_le_bytes(m[454..458].try_into().unwrap());
        let span = u32::from_le_bytes(m[458..462].try_into().unwrap());
        assert_eq!(start, 1);
        assert_eq!(span, 999_999);
        // and it reads back as exactly that
        let mut head = vec![0u8; SECTOR * 2];
        head[..SECTOR].copy_from_slice(&m);
        assert!(read(&head).protective_only);
    }

    #[test]
    fn a_disk_bigger_than_a_u32_of_sectors_clamps_instead_of_wrapping() {
        // 4 TB at 512 bytes is more sectors than the field can hold; the convention is to fill it
        let m = protective_mbr(8_000_000_000);
        assert_eq!(u32::from_le_bytes(m[458..462].try_into().unwrap()), u32::MAX);
    }
}
