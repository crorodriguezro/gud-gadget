#!/bin/sh
set -eu

release=6.12.47+rpt-rpi-v8-ffs-trace
home=/home/cristian
boot=/boot/firmware

check_hash()
{
	expected=$1
	path=$2
	actual=$(sha256sum "$path")
	actual=${actual%% *}
	[ "$actual" = "$expected" ] || {
		echo "hash mismatch: $path" >&2
		exit 1
	}
}

[ "$(id -u)" -eq 0 ] || { echo "run as root" >&2; exit 1; }
[ "$(systemctl is-active gud-userspace.service 2>/dev/null || true)" = inactive ] || {
	echo "refusing install: gud-userspace.service is not inactive" >&2
	exit 1
}
[ "$(cat /sys/class/udc/3f980000.usb/state)" = "not attached" ] || {
	echo "refusing install: DWC2 UDC is attached" >&2
	exit 1
}

for target in "$boot/kernel8-ffs-trace.img" "$boot/initramfs8-ffs-trace" \
	"$boot/tryboot.txt" "/lib/modules/$release" "$boot/config-$release" \
	"$boot/System.map-$release" "/boot/initrd.img-$release"
do
	[ ! -e "$target" ] || { echo "refusing existing target: $target" >&2; exit 1; }
done

check_hash b03233c6ae5d33b2f6140a0b6ecafeee581c0854fc4905f8914df7f8255da3de "$home/kernel8-ffs-trace.img.upload"
check_hash bb1607d0dc3f1f7b3b23eb32982da5366e3776d90a7ad9044d8accd866e82341 "$home/System.map-$release.upload"
check_hash 84377eada8e29a4555f5a45f873f191a0da19142246e961f4bf99bd8e2cd99c3 "$home/config-$release.upload"
check_hash 688737d07268eaf0a6187fff27ab9e614184235f30142f0b33be53a236ede984 "$home/functionfs-ffs-trace-modules.tar.gz.upload"

tar -tzf "$home/functionfs-ffs-trace-modules.tar.gz.upload" |
	awk -v root="$release" '$0 != root && index($0, root "/") != 1 { bad = 1 } END { exit bad }'

install -m 0644 "$home/config-$release.upload" "$boot/config-$release"
install -m 0644 "$home/System.map-$release.upload" "$boot/System.map-$release"
tar -xzf "$home/functionfs-ffs-trace-modules.tar.gz.upload" -C /lib/modules
depmod -a "$release"
update-initramfs -c -k "$release"
install -m 0644 "$home/kernel8-ffs-trace.img.upload" "$boot/kernel8-ffs-trace.img"
install -m 0644 "$boot/initrd.img-$release" "$boot/initramfs8-ffs-trace"
{
	cat "$boot/config.txt"
	printf '\nauto_initramfs=0\nkernel=kernel8-ffs-trace.img\ninitramfs initramfs8-ffs-trace followkernel\n'
} > "$boot/tryboot.txt"
sync
echo "installed_release=$release"
