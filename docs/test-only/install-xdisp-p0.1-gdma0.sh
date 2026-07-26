#!/bin/sh
set -eu

release=6.12.47+rpt-rpi-v8-xdisp-gdma0
home=/home/cristian
boot=/boot/firmware
modules=/lib/modules

image_upload=$home/kernel8-xdisp-gdma0.img.upload
map_upload=$home/System.map-$release.upload
config_upload=$home/config-$release.upload
modules_upload=$home/xdisp-gdma0-modules.tar.gz.upload
tryboot_upload=$home/tryboot-xdisp-gdma0.txt.upload

image_target=$boot/kernel8-xdisp-gdma0.img
initramfs_target=$boot/initramfs8-xdisp-gdma0
tryboot_target=$boot/tryboot.txt
modules_target=$modules/$release
config_target=/boot/config-$release
map_target=/boot/System.map-$release
initramfs_source=/boot/initrd.img-$release

if [ "$(id -u)" -ne 0 ]; then
	echo "run as root" >&2
	exit 1
fi

if [ "$(systemctl is-active gud-userspace.service 2>/dev/null || true)" != inactive ]; then
	echo "refusing install: gud-userspace.service is not inactive" >&2
	exit 1
fi

if [ "$(cat /sys/class/udc/3f980000.usb/state)" != "not attached" ]; then
	echo "refusing install: DWC2 UDC is attached" >&2
	exit 1
fi

for target in \
	"$image_target" \
	"$initramfs_target" \
	"$tryboot_target" \
	"$modules_target" \
	"$config_target" \
	"$map_target" \
	"$initramfs_source"
do
	if [ -e "$target" ]; then
		echo "refusing install: target already exists: $target" >&2
		exit 1
	fi
done

check_hash()
{
	expected=$1
	path=$2
	actual=$(sha256sum "$path")
	actual=${actual%% *}
	if [ "$actual" != "$expected" ]; then
		echo "hash mismatch: $path" >&2
		exit 1
	fi
}

check_hash f3716136a03061e2a81039b65ee5234f785f04706c1b1ed8297e67600835cb97 "$image_upload"
check_hash f6703a8d141eb037c51bbe4643e42a1e37328e06c72fa4cd88c65cb823a3063c "$map_upload"
check_hash 75eef4dafb1a419b87bb01688ac2223535da037286b0e2f6278fea71b20d5c9e "$config_upload"
check_hash 9d1dc40d35c7a41a1b55de54cf20e79a20a763aabcf291c01a6bd5c360648df2 "$modules_upload"
check_hash 8f70b66fe4bdc125f3c65fefd052738e45cab8953222d6b5f917385a97b5d950 "$tryboot_upload"

tar -tzf "$modules_upload" |
	awk -v root="$release" '
		$0 != root && index($0, root "/") != 1 { bad = 1 }
		END { exit bad }
	'

stamp=$(date -u +%Y%m%dT%H%M%SZ)
config_backup=$boot/config.txt.pre-xdisp-p0.1-gdma0-$stamp
cp --preserve=all "$boot/config.txt" "$config_backup"

install -m 0644 "$config_upload" "$config_target"
install -m 0644 "$map_upload" "$map_target"
tar -xzf "$modules_upload" -C "$modules"
depmod -a "$release"
update-initramfs -c -k "$release"
test -s "$initramfs_source"

install -m 0644 "$image_upload" "$image_target"
install -m 0644 "$initramfs_source" "$initramfs_target"
install -m 0644 "$tryboot_upload" "$tryboot_target"
sync

echo "installed_release=$release"
echo "stock_config_backup=$config_backup"
echo "normal_config_unchanged=$boot/config.txt"
echo "one_shot_config=$tryboot_target"
