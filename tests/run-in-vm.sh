#!/usr/bin/env bash
# Run arxburn's write-path cases in a THROWAWAY VM. Never on the host: they erase disks.
#
# Boots an ArxOS live ISO under QEMU with two scratch disks, both plain files in a temp dir:
#   - a USB stick (usb-storage, removable): the normal target, written and checked;
#   - an internal disk (virtio, not removable): must be refused without --allow-internal.
# Copies the binary and tests/cases.sh in over SSH, runs the cases as root, prints the result,
# and throws the VM and its disks away. Exit status is the number of failed cases.
#
# usage: tests/run-in-vm.sh <arxos-live.iso> [path/to/arxburn]
set -euo pipefail
ISO=${1:?usage: tests/run-in-vm.sh <arxos-live.iso> [path/to/arxburn]}
HERE=$(cd "$(dirname "$0")" && pwd)
BIN=${2:-$HERE/../target/release/arxburn}
[ -f "$ISO" ] || { echo "no ISO at $ISO"; exit 2; }
[ -x "$BIN" ] || { echo "no arxburn binary at $BIN (cargo build --release first)"; exit 2; }
for t in qemu-system-x86_64 qemu-img sshpass sfdisk; do command -v "$t" >/dev/null || { echo "needs $t"; exit 2; }; done

W=$(mktemp -d)
QPID=""
cleanup(){ [ -n "$QPID" ] && kill "$QPID" 2>/dev/null || true; rm -rf "$W"; }
trap cleanup EXIT

truncate -s 64M "$W/usb.img"; truncate -s 64M "$W/int.img"
# fixtures that need host tools (cases.sh builds the compressed ones inside the VM)
truncate -s 8M "$W/disk.img"
printf 'label: gpt\n,4M,U\n,,L\n' | sfdisk -q "$W/disk.img" >/dev/null
dd if=/dev/urandom of="$W/disk.img" bs=1M seek=1 count=6 conv=notrunc status=none
qemu-img create -q -f qcow2 "$W/vm.qcow2" 8M
python3 -I -c 'import sys,zipfile; zipfile.ZipFile(sys.argv[1],"w").write(sys.argv[2],"disk.img")' "$W/arch.zip" "$W/disk.img"

PORT=$((20000 + RANDOM % 20000))
# q35: a modern chipset with no floppy controller (the "pc" machine's empty fd0 is noise)
qemu-system-x86_64 -machine q35 -enable-kvm -m 3072 -smp 2 -display none -daemonize -pidfile "$W/q.pid" \
  -cdrom "$ISO" -boot d \
  -drive file="$W/int.img",format=raw,if=virtio \
  -device qemu-xhci -drive if=none,id=stick,file="$W/usb.img",format=raw \
  -device usb-storage,drive=stick,removable=on \
  -nic user,model=virtio-net-pci,hostfwd=tcp:127.0.0.1:"$PORT"-:22
QPID=$(cat "$W/q.pid")

SSHO=(-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=5 -o LogLevel=ERROR)
ssh_(){ sshpass -p arxos ssh "${SSHO[@]}" -p "$PORT" arxos@127.0.0.1 "$@"; }
echo ">> booting the live ISO in a throwaway VM (ssh on 127.0.0.1:$PORT)"
for _ in $(seq 1 72); do ssh_ true 2>/dev/null && break; sleep 5; done
ssh_ true || { echo "!! the VM never answered on ssh"; exit 2; }

echo ">> copying arxburn, the cases and fixtures in"
sshpass -p arxos scp "${SSHO[@]}" -P "$PORT" -q "$BIN" "$HERE/cases.sh" \
  "$W/disk.img" "$W/vm.qcow2" "$W/arch.zip" arxos@127.0.0.1:/tmp/
echo ">> running the cases as root inside the VM"
set +e
ssh_ 'mkdir -p /tmp/abt && cd /tmp && mv arxburn cases.sh disk.img vm.qcow2 arch.zip abt/ && chmod +x abt/arxburn abt/cases.sh && sudo bash abt/cases.sh'
rc=$?
set -e
ssh_ 'sudo systemctl poweroff' >/dev/null 2>&1 || true
exit "$rc"
