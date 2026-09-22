# Building and testing arxburn

For whoever picks this up next, including Codex.

## Layout

```
src/            the CLI. std only, no dependencies, and it must stay that way: this is the
                binary someone runs on a machine that has just been installed.
  main.rs       commands, the burn path, the live byte counter
  dev.rs        block devices, and the refusal rules. The important file.
  catalog.rs    where images come from, and how "newest" is resolved
  fetch.rs      parallel range downloads (the embedded copy of the dnengine idea)
  net.rs        curl/wget wrapper, index scraping, natural version order
  sha256.rs     FIPS 180-4, with a runtime SHA extension path
  json.rs       the --json surface the GUI reads
src-tauri/      the GUI. Tauri v2, depends on tauri + serde. Runs the CLI with --json and
                forwards it; it decides nothing about what may be written.
ui/             the GUI's html, css and js
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
cargo test                            # 19 unit tests, host, no devices touched
```

Runtime tests never run on the host: they write to block devices. Use a throwaway VM, which is
also how the shipped results were produced.

```sh
# build a disposable initramfs with the binary in it, two raw disks attached
python3 mk-vm.py out/ init --bin arxburn --bin dd --bin od --bin sha256sum --file img.bin=/img.bin
qemu-system-x86_64 -enable-kvm -m 1024 -nographic -no-reboot \
  -kernel /boot/vmlinuz-linux -initrd out/initramfs.cpio \
  -append "console=ttyS0 panic=1 quiet rdinit=/init" \
  -drive file=target.img,format=raw,if=virtio \
  -drive file=small.img,format=raw,if=virtio
```

The seven cases that must keep passing, because each one is a way somebody loses a disk:

1. `list` marks the disk carrying `/` as SYSTEM.
2. an internal disk is refused without `--allow-internal`, and the disk is untouched afterwards
   (read the first bytes back and check they are still zero).
3. a write to a permitted disk verifies byte for byte.
4. `verify` passes on a good stick.
5. flipping ONE byte on the device makes `verify` fail.
6. an image larger than the device is refused before anything is written.
7. a mistyped confirmation writes nothing.

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
* **Catalog entries go stale.** They describe how to FIND the newest build, not a fixed URL.
  When a project moves its tree, fix the entry rather than pinning a version, and check it with
  `arxburn iso <id>`, which resolves live.
* **No em-dashes** in anything user facing, and commits are authored
  `0xb0rn3 | スティングレイ` with no trailers.
