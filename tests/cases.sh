#!/usr/bin/env bash
# arxburn runtime cases. Run INSIDE a throwaway VM as root; tests/run-in-vm.sh sets that up.
# Never run this on a real machine: it writes to whole block devices.
#
# Each case is a way somebody loses a disk or boots a bad stick. Refusal cases put a marker on
# the device first and check it is still there afterwards, so "refused" provably means "nothing
# was written". Exit status is the number of failures.
set -u
cd "$(dirname "$0")"
B=./arxburn
# the stick is the disk on the USB transport (not "removable": a VM's empty floppy is removable too)
USB=$(lsblk -dnro NAME,TRAN,TYPE | awk '$2=="usb" && $3=="disk"{print $1; exit}')
INT=$(lsblk -dnro NAME,TYPE | awk '$1 ~ /^vd/ && $2=="disk"{print $1; exit}')
echo "removable target: /dev/${USB:-?}   internal disk: /dev/${INT:-?}"
[ -n "$USB" ] && [ -n "$INT" ] || { echo "FAIL  test disks not found"; exit 1; }

pass=0; fail=0
ok(){ echo "PASS  $*"; pass=$((pass+1)); }
bad(){ echo "FAIL  $*"; fail=$((fail+1)); }
hex16(){ dd if="/dev/$1" bs=16 count=1 status=none | od -An -tx1 | tr -d ' \n'; }
MARK=$(printf 'ARXBURN-MARKER-0' | od -An -tx1 | tr -d ' \n')
put_mark(){ printf 'ARXBURN-MARKER-0' | dd of="/dev/$1" bs=16 count=1 conv=notrunc status=none; sync; }
untouched(){ [ "$(hex16 "$1")" = "$MARK" ]; }
matches_image(){ cmp -s -n "$(stat -c%s "$2")" "/dev/$1" "$2"; }   # device prefix == raw image
wipe(){ dd if=/dev/zero of="/dev/$1" bs=1M count=16 status=none; sync; }
last(){ printf '%s' "$1" | tail -2 | tr '\n' ' '; }

# fixtures, made here from disk.img (8 MiB, GPT, random payload)
xz -k -f -T1 disk.img; zstd -q -f -k disk.img; gzip -k -f disk.img; bzip2 -k -f disk.img
tar -cf - disk.img | xz -T1 > bundle.tar.xz
cp disk.img.xz corrupt.img.xz
printf '\x00\xff\x00\xff' | dd of=corrupt.img.xz bs=1 seek=$(( $(stat -c%s corrupt.img.xz) / 2 )) conv=notrunc status=none
head -c $(( $(stat -c%s disk.img.xz) - 200 )) disk.img.xz > trunc.img.xz
truncate -s 96M big.img; printf 'label: gpt\n,,L\n' | sfdisk -q big.img >/dev/null 2>&1
xz -k -f -T1 big.img; gzip -c big.img > big.img.gz
XZSHA=$(sha256sum disk.img.xz | cut -d' ' -f1)

# ---- the seven from BUILD.md ----------------------------------------------------------------
$B list --json >/dev/null 2>&1 && ok "1 list runs" || bad "1 list failed"

put_mark "$INT"
if $B write disk.img --to "$INT" --yes >/dev/null 2>&1; then bad "2 internal disk was NOT refused"
elif untouched "$INT"; then ok "2 internal disk refused without --allow-internal, untouched"
else bad "2 internal disk refused, but its bytes changed"; fi

wipe "$USB"; out=$($B write disk.img --to "$USB" --yes 2>&1)
if printf '%s' "$out" | grep -q VERIFIED && matches_image "$USB" disk.img; then ok "3 raw write verified, device == image"
else bad "3 raw write: $(last "$out")"; fi

$B verify disk.img --to "$USB" >/dev/null 2>&1 && ok "4 verify passes on a good stick" || bad "4 verify failed on a good stick"

printf '\x5a' | dd of="/dev/$USB" bs=1 seek=$((3 * 1024 * 1024)) conv=notrunc status=none; sync
echo 3 > /proc/sys/vm/drop_caches
$B verify disk.img --to "$USB" >/dev/null 2>&1 && bad "5 verify PASSED a corrupted stick" || ok "5 one flipped byte fails verify"

put_mark "$USB"
if $B write big.img --to "$USB" --yes >/dev/null 2>&1; then bad "6 oversize raw image was written"
elif untouched "$USB"; then ok "6 oversize image refused before anything was written"
else bad "6 oversize image refused, but the device changed"; fi

put_mark "$USB"
if echo wrong | $B write disk.img --to "$USB" >/dev/null 2>&1; then bad "7 a wrong confirmation still wrote"
elif untouched "$USB"; then ok "7 mistyped confirmation wrote nothing"
else bad "7 device changed after a wrong confirmation"; fi

# ---- compressed images ----------------------------------------------------------------------
for f in disk.img.xz disk.img.zst disk.img.gz disk.img.bz2; do
  wipe "$USB"; out=$($B write "$f" --to "$USB" --yes 2>&1)
  if printf '%s' "$out" | grep -q VERIFIED && matches_image "$USB" disk.img; then
    ok "8 $f decompressed while written, verified, device == raw image"
  else bad "8 $f: $(last "$out")"; fi
done

$B verify disk.img.xz --to "$USB" >/dev/null 2>&1 && ok "9 verify against the .xz it was written from" || bad "9 verify against the .xz failed"

for f in corrupt.img.xz trunc.img.xz; do
  wipe "$USB"; out=$($B write "$f" --to "$USB" --yes 2>&1)
  if printf '%s' "$out" | grep -q VERIFIED; then bad "10 $f was reported VERIFIED"
  elif printf '%s' "$out" | grep -q damaged; then ok "10 $f rejected as damaged, never verified"
  else bad "10 $f: $(last "$out")"; fi
done

for f in vm.qcow2 arch.zip bundle.tar.xz; do
  put_mark "$USB"; out=$($B write "$f" --to "$USB" --yes 2>&1)
  if printf '%s' "$out" | grep -q refusing && untouched "$USB"; then ok "11 $f refused before anything was erased"
  else bad "11 $f: $(last "$out")"; fi
done

put_mark "$USB"
if $B write big.img.xz --to "$USB" --yes >/dev/null 2>&1; then bad "12 oversize .xz was written"
elif untouched "$USB"; then ok "12 oversize .xz (size recorded) refused before writing"
else bad "12 oversize .xz: the device changed"; fi

# Up to 0.1.7 this case HUNG forever: the writer died on ENOSPC and the drain wait never noticed.
# So it runs under a deadline, and it must end by itself and say why.
out=$(timeout 120 $B write big.img.gz --to "$USB" --yes 2>&1); rc=$?
if [ $rc -eq 124 ]; then bad "13 oversize .gz HUNG (killed after 120s)"
elif [ $rc -ne 0 ] && ! printf '%s' "$out" | grep -q VERIFIED && printf '%s' "$out" | grep -q "device is full"; then
  ok "13 oversize .gz (size not recorded) stops partway, says the device is full, never verified"
else bad "13 oversize .gz: rc=$rc $(last "$out")"; fi

wipe "$USB"
$B write disk.img.xz --to "$USB" --yes --expect "$XZSHA" 2>&1 | grep -q VERIFIED \
  && ok "14 --expect with the published .xz sha256 passes" || bad "14 --expect with the right sha256 failed"
put_mark "$USB"
if $B write disk.img.xz --to "$USB" --yes --expect "$(printf '0%.0s' {1..64})" >/dev/null 2>&1; then bad "15 a wrong --expect was written"
elif untouched "$USB"; then ok "15 wrong --expect refused before anything was erased"
else bad "15 wrong --expect: the device changed"; fi

# ---- local scan, and images stored on the target itself -------------------------------------
UH=/home/${SUDO_USER:-arxos}
mkdir -p "$UH/Downloads"
cp disk.img "$UH/Downloads/fresh-download.img"
cp vm.qcow2 "$UH/Downloads/looks-like.img"; truncate -s 2M "$UH/Downloads/looks-like.img"  # a VM disk named .img
listing=$($B local --no-drives 2>&1)
printf '%s' "$listing" | grep -q fresh-download.img && ok "16 local finds an image in ~/Downloads by content" \
  || bad "16 ~/Downloads/fresh-download.img not listed: $(last "$listing")"
printf '%s' "$listing" | grep -q looks-like.img && bad "17 a qcow2 named .img was offered as burnable" \
  || ok "17 a qcow2 named .img is not offered"

wipe "$USB"
mkdir -p /run/media/arxos/STICK
if mkfs.ext4 -q -F "/dev/$USB" && mount "/dev/$USB" /run/media/arxos/STICK; then
  cp disk.img.xz /run/media/arxos/STICK/on-stick.img.xz; sync
  $B local 2>&1 | grep on-stick.img.xz | grep -q "external drive" \
    && ok "18 an image on a mounted USB drive is listed and marked external" \
    || bad "18 the image on the mounted drive was not listed as external"
  out=$($B write /run/media/arxos/STICK/on-stick.img.xz --to "$USB" --yes 2>&1)
  if printf '%s' "$out" | grep -q "the device it would be written to" && cmp -s disk.img.xz /run/media/arxos/STICK/on-stick.img.xz; then
    ok "19 an image stored on the target itself is refused, and it survives"
  else bad "19 image on the target: $(last "$out")"; fi
  umount /run/media/arxos/STICK
else bad "18/19 could not make and mount a filesystem on the stick"; fi

echo "RESULT $pass passed, $fail failed"
exit "$fail"
