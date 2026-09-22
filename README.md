# arxburn

Write an image to a USB stick, and prove the stick got it. Fetch the image too, if you do not have
it yet.

Etcher refuses to touch your system disk and verifies the write afterwards, but ships a browser to
do it. `dd` is exact, always present, and will erase the wrong disk without a word. Rufus gives you
real control over the target and a list of images to download. arxburn keeps all of that in one
native binary with no dependencies at all.

```sh
arxburn list                          # what is plugged in, and what it refuses to touch
arxburn iso                           # images it can fetch, ours and everyone else's
arxburn get arch --to sdc             # download the newest Arch, check it, burn it, verify it
arxburn write arxos-0.0.1.iso --to sdc
arxburn verify arxos-0.0.1.iso --to sdc
```

## What it does that dd does not

* **Refuses the running system.** The disk carrying `/` is never a target, under any flag.
* **Refuses internal disks** unless you pass `--allow-internal`. Removable media is the default.
* **Checks the image first.** The image is hashed before anything is erased, so a truncated
  download is caught while the stick is still intact. `--expect <sha256>` makes that a hard gate,
  and a downloaded image is checked against the project's own published checksum when there is one.
* **Checks the size.** An image larger than the device is refused, not half written.
* **Unmounts deliberately** and tells you what it unmounted, rather than failing halfway through.
* **Asks for the device name**, not `y/n`. A yes is too easy to type by reflex.
* **Reads the bytes back** off the device after writing and hashes them against the image. The page
  cache is dropped first, so the verify reads the stick and not a copy of what was just written.

## Downloading

`arxburn iso` lists what it knows about. `arxburn iso <id>` resolves that entry live and shows the
exact file, its size and its published hash.

```sh
arxburn iso                  # everything
arxburn iso --windows        # just the Windows entries
arxburn iso security         # anything matching "security"
arxburn iso kali             # resolve Kali's newest build right now
arxburn get kali --out ~/iso # download it, verify the hash, leave it on disk
arxburn get kali --to sdc    # download it and burn it in one go
```

Most entries describe **how to find the newest image** rather than naming one, so the list does not
go stale: a directory to read, a version to pick, a GitHub release, an archive.org item. Downloads
resume, so a 5GB image over a bad line continues instead of starting over, and an image you already
have is re-checked rather than re-fetched.

What is in the box: **ArxOS**, Arch, CachyOS, EndeavourOS, openSUSE Tumbleweed, NixOS unstable,
Void, Gentoo, Debian (stable, live, testing weekly), Ubuntu, Fedora (stable and Rawhide), Linux
Mint, Kali (release and weekly), Parrot Security, Alpine, Rocky, AlmaLinux, Qubes, Tails, Proxmox,
FreeBSD, SystemRescue, Rescuezilla, netboot.xyz, Windows 11 and 10, Windows 11 Enterprise and
Windows Server evaluation images, and Tiny11, Tiny11 Core and Tiny10 for old hardware.

**About Windows.** Microsoft hands consumer images out through a browser session and refuses
requests it decides are automated, so `arxburn get win11` may be turned away, and it says so
plainly instead of pretending. `win11-eval` and `winserver-eval` are official Microsoft evaluation
images with direct links and no account, and they always work. The Tiny entries are community
rebuilds hosted on archive.org: they still need a valid Windows licence.

## Adding your own images

Drop a TSV at `~/.config/arxburn/catalog.tsv`, `/etc/arxburn/catalog.tsv`, or wherever
`ARXBURN_CATALOG` points. An entry with an existing id replaces the built-in one, which is how you
point Arch at your own local mirror.

```
# id      name          family  note              source                                    sums
arch      Arch (local)  linux   from the LAN      index:http://mirror.lan/iso/latest/|archlinux-|-x86_64.iso  sha256sums.txt
myimage   Our build     linux   nightly           direct:https://build.lan/latest.iso
```

Sources: `direct:URL`, `index:DIR|PREFIX|SUFFIX[|REJECT]`,
`chain:ROOT|DIRPREFIX|SUB|PREFIX|SUFFIX[|REJECT]`, `github:owner/repo|SUFFIX`, `archive:ID`,
`fwlink:URL`. `REJECT` is a comma separated list of substrings to skip, which is how the Debian
entry avoids picking up the mac and edu variants that sort above the one you want.

## Install

```sh
cargo build --release
sudo install -Dm755 target/release/arxburn /usr/bin/arxburn
```

or `sudo ./install.sh`, which does the same.

## Options

| option | meaning |
| --- | --- |
| `--to <dev\|UUID>` | `sdc`, `/dev/sdc`, or a filesystem UUID of one of its partitions |
| `--expect <sha256>` | verify the image against a published hash before writing |
| `--out <dir>` | where downloads land (default: the current directory) |
| `--yes` | skip the typed confirmation, for scripts |
| `--no-verify` | skip the read-back check (not advised) |
| `--allow-internal` | permit a non-removable disk; the running system stays refused |
| `--loop` | allow `/dev/loopN` targets, for testing against a file |

Passing a **UUID** is the safer habit: `sd*` letters move when you replug, and that is exactly how
people overwrite the wrong disk.

```sh
# burn a release and check it against the hash on the download page
sudo arxburn write arxos-0.0.1.iso --to sdc \
  --expect 18d87568b4e2cf4d2e83bb0052c93a5d76b12708bd942e86029ea612c1f2d44d
```

## Why there are no dependencies

The tool's promise is "the bytes on the stick are the bytes in the image", so the hash is the one
part that must never be unavailable. SHA-256 is implemented here against the FIPS 180-4 vectors
rather than pulled from a crate, and the only outside program it uses is curl (or wget) for
downloads. It builds on a freshly installed machine with no network and an empty cargo cache, which
is the machine someone reaches for a burn tool on.

## Tests

```sh
cargo test
```

15 unit tests: the SHA-256 vectors and chunk-boundary agreement, the refusal rules, version-order
resolution, checksum-file parsing and catalog overrides. The device path is exercised separately in
a throwaway VM against real block devices: refusal without the flag leaves the disk untouched, a
write verifies byte for byte, a single flipped byte makes verify fail, an oversized image is
refused, and a mistyped confirmation writes nothing.

---

Part of [ArxOS](https://arxos.uk). Belongs under Stingray Labs.
