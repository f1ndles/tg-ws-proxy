#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SDK=''
OUTPUT="$ROOT/dist/openwrt"
VERSION=''
FORMAT=''

usage() {
    printf 'Usage: %s --sdk PATH --version PACKAGE_VERSION --format apk|ipk [--output PATH]\n' "$0"
}

while [[ "$#" -gt 0 ]]; do
    case "$1" in
        --sdk) SDK="$2"; shift 2 ;;
        --version) VERSION="$2"; shift 2 ;;
        --format) FORMAT="$2"; shift 2 ;;
        --output) OUTPUT="$2"; shift 2 ;;
        -h|--help) usage; exit 0 ;;
        *) printf 'error: unknown argument: %s\n' "$1" >&2; exit 1 ;;
    esac
done

[[ -n "$SDK" && -d "$SDK" ]] || { printf 'error: valid --sdk is required\n' >&2; exit 1; }
[[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+((_beta[0-9]+)|(~beta\.[0-9]+))?$ ]] || {
    printf 'error: invalid package version: %s\n' "$VERSION" >&2
    exit 1
}
case "$FORMAT" in apk|ipk) ;; *) printf 'error: --format must be apk or ipk\n' >&2; exit 1 ;; esac
[[ -f "$SDK/include/package.mk" ]] || { printf 'error: not an OpenWrt SDK: %s\n' "$SDK" >&2; exit 1; }

package_dir="$SDK/package/luci-app-tg-ws-proxy-rs"
rm -rf "$package_dir" "$SDK/package/luci-app-tg-ws-proxy"
mkdir -p "$package_dir"
cp -a "$ROOT/openwrt/luci-app/." "$package_dir/"

shopt -s nullglob
old_packages=("$SDK"/bin/packages/*/*/luci-app-tg-ws-proxy-rs*."$FORMAT")
((${#old_packages[@]} == 0)) || rm -f "${old_packages[@]}"

make -C "$SDK" defconfig >/dev/null
make -C "$SDK" package/luci-app-tg-ws-proxy-rs/clean >/dev/null
make -C "$SDK" package/luci-app-tg-ws-proxy-rs/compile \
    TG_PACKAGE_VERSION="$VERSION"

packages=("$SDK"/bin/packages/*/*/luci-app-tg-ws-proxy-rs*."$FORMAT")
[[ "${#packages[@]}" -eq 1 ]] || {
    printf 'error: expected one LuCI %s, found %s\n' "$FORMAT" "${#packages[@]}" >&2
    exit 1
}
package="${packages[0]}"

if [[ "$FORMAT" == apk ]]; then
    metadata="$("$SDK"/staging_dir/host/bin/apk adbdump "$package")"
    grep -Fq 'name: luci-app-tg-ws-proxy-rs' <<<"$metadata"
    grep -Fq "version: $VERSION-r1" <<<"$metadata"
    grep -Fq 'arch: noarch' <<<"$metadata"
else
    metadata="$(tar -xOzf "$package" ./control.tar.gz | tar -xzOf - ./control)"
    grep -Fq 'Package: luci-app-tg-ws-proxy-rs' <<<"$metadata"
    grep -Fq "Version: $VERSION-r1" <<<"$metadata"
    grep -Fq 'Architecture: all' <<<"$metadata"
fi

mkdir -p "$OUTPUT"
output_name="${package##*/}"
# GitHub Releases rewrites '~' to '.' in asset names. Normalize the public IPK
# filename before checksums are generated; package metadata keeps '~beta'.
if [[ "$FORMAT" == ipk ]]; then
    output_name="${output_name//\~/.}"
fi
cp "$package" "$OUTPUT/$output_name"
printf '%s\n' "$OUTPUT/$output_name"
