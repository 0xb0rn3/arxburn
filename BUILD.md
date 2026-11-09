# Building and testing arxburn

For whoever picks this up next, including Codex.

## Layout

```
src/            the CLI. std only, no dependencies, and it must stay that way: this is the
                binary someone runs on a machine that has just been installed.
  main.rs       commands, the burn path, the live byte counter
  dev.rs        block devices, and the refusal rules. The important file.
  format.rs     what a file IS, from its bytes: ISO, raw disk, xz/zstd/gzip/bzip2, and the
                formats that are refused (VM disks, DMG, zip/7z/rar/tar). Decompression goes
                through the system's xz/zstd/gzip/bzip2; no crate, no codec of our own.
  local.rs      `arxburn local`: images already on the machine, by content, and the device a
                file lives on (for the image-on-its-own-target refusal)
  catalog.rs    where images come from, and how "newest" is resolved
  fetch.rs      parallel range downloads (the embedded copy of the dnengine idea)
  net.rs        curl/wget wrapper, index scraping, natural version order
  sha256.rs     FIPS 180-4, with a runtime SHA extension path
  json.rs       the --json surface the GUI reads
src-tauri/      the GUI. Tauri v2, depends on tauri + serde. Runs the CLI with --json and
                forwards it; it decides nothing about what may be written.
ui/             the GUI's html, css and js
tests/          run-in-vm.sh (boots a throwaway VM) and cases.sh (the device cases, run inside it)
```

## Build

```sh
cargo build --release                 # the CLI ONLY: the workspace root is itself a package,
                                      # so a bare build does not touch src-tauri
cargo build --release -p arxburn-gui  # the GUI (needs webkit2gtk-4.1, libsoup3, gtk3)
cargo build --release --workspace     # both
sudo ./install.sh                     # builds as $SUDO_USER, installs both when it can
```

`install.sh` never builds as root on purpose. rustup toolchains are per user, so under sudo there
is usually no default toolchain and cargo refuses outright; and anything root did build would
leave root-owned files in the checkout. It builds as `$SUDO_USER` and installs as root.

The GUI needs a system webkit; the CLI needs nothing. If a build box lacks webkit, build the CLI
alone rather than adding a dependency to it.

## Test

```sh
cargo test                            # 46 unit tests, host, no devices touched
```

Runtime tests never run on the host: they write to block devices. Use a throwaway VM, which is
also how the shipped results were produced.

```sh
cargo build --release
tests/run-in-vm.sh /path/to/arxos-live.iso    # exit status = number of failed cases
```

`run-in-vm.sh` boots the live ISO under QEMU (q35, so there is no empty floppy for a careless
selector to pick) with two scratch disks that are plain files in a temp dir: a `usb-storage`
stick, the normal target, and a virtio internal disk that must be refused. It copies the binary,
`tests/cases.sh` and the fixtures in over SSH (the live ISO's `arxos`/`arxos` test login), runs
the cases as root, prints the result and throws the VM and its disks away. `cases.sh` picks the
stick by `TRAN=usb`, never by `removable` (an empty floppy drive is removable too). Refusal cases
put a marker on the device first and check it afterwards, so "refused" provably means "nothing
was written".

The cases that must keep passing, because each one is a way somebody loses a disk or boots a bad
stick (19 cases, 25 checks; last run 2026-11-09 on 0.1.8, 25 passed):

1. `list` runs.
2. an internal disk is refused without `--allow-internal`, and its bytes are unchanged.
3. a write to a permitted disk verifies, and the device equals the image.
4. `verify` passes on a good stick.
5. flipping ONE byte on the device makes `verify` fail.
6. an image larger than the device is refused before anything is written.
7. a mistyped confirmation writes nothing.
8. `.xz`, `.zst`, `.gz` and `.bz2` images are unpacked while written, verified, device == raw image.
9. `verify` against the `.xz` the stick was written from passes.
10. a corrupt and a truncated `.xz` are rejected as damaged and never called verified.
11. a qcow2, a zip and a `.tar.xz` are refused before anything is erased.
12. an oversized `.xz` (its size is recorded in the file) is refused before writing.
13. an oversized `.gz` (size not recorded) stops partway, says the device is full, and is never
    called verified. Runs under a deadline: up to 0.1.7 this case hung forever.
14. `--expect` with the published `.xz` sha256 passes.
15. a wrong `--expect` is refused before anything is erased.
16. `local` finds an image in `~/Downloads` by its content.
17. a qcow2 named `.img` is not offered by `local`.
18. an image on a mounted USB drive is listed and marked external.
19. an image stored on the target stick itself is refused, and the file survives.

## What is easy to get wrong here

* **The system disk check must not rely on the device number alone.** On btrfs, zfs, LVM and
  anything else on an anonymous block device, `/proc/self/mountinfo` reports something like
  `0:35` for `/`, which matches no disk in `/sys`. `dev.rs` also reads the mount SOURCE and
  resolves it back through partitions and device-mapper slaves. Removing either signal reopens
  the worst bug this tool can have.
* **Verification must drop the page cache first** (`/proc/sys/vm/drop_caches`), or it confirms
  what we just wrote from RAM instead of what the stick stored.
* **The hash engines must agree.** `sha256.rs` has a hardware path that only runs on CPUs with
  the SHA extensions; the test compares it against the portable one over many lengths. Keep that
  test passing or a burn could be declared good on a lie.
* **Every wait must also end when the writer dies.** `write_pipelined` waits for the device to
  take everything that was read. Until 0.1.8 that wait only watched a byte counter, so a writer
  that stopped on an error (device full, stick pulled out) left it waiting forever, with the
  decompressor blocked on a full pipe behind it. Any new wait in the write path needs the same
  exit, and `write_tests` holds the regression against `/dev/full` on a deadline.
* **A decompressor's exit status is a verdict, not noise.** A truncated archive simply ends early,
  and the read back alone would pass it, because it compares the device with what was written.
  The exit status is where xz/zstd/gzip/bzip2 report their own checksums failing.
* **gzip and bzip2 do not record the unpacked size reliably**, so an oversized one cannot be
  refused up front: it is written until the device is full and then reported as such, never
  verified. xz and zstd record it, and an oversized one is refused before anything is erased.
* **The GUI renders text it did not write.** File names, a stick's model string and a mirror's
  file listing all reach the page. They go through `esc()` before any `innerHTML`; a stick can
  name itself.
* **Catalog entries go stale.** They describe how to FIND the newest build, not a fixed URL.
  When a project moves its tree, fix the entry rather than pinning a version, and check it with
  `arxburn iso <id>`, which resolves live.
* **No em-dashes** in anything user facing, and commits are authored
  `0xb0rn3 | スティングレイ` with no trailers.
