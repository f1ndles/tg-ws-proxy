#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }

for file in \
    install.sh \
    openwrt/build-luci-package.sh \
    openwrt/luci-app/Makefile \
    openwrt/luci-app/root/etc/config/tg-ws-proxy-rs \
    openwrt/luci-app/root/etc/init.d/tg-ws-proxy-rs \
    .github/workflows/release.yml; do
    [[ -f "$ROOT/$file" ]] || fail "missing $file"
done
for obsolete in openwrt/Makefile openwrt/build-package.sh openwrt/release-architectures.tsv; do
    [[ ! -e "$ROOT/$obsolete" ]] || fail "obsolete core packaging remains: $obsolete"
done

makefile="$ROOT/openwrt/luci-app/Makefile"
grep -Eq '^[[:space:]]*PKGARCH:=all$' "$makefile" || fail 'LuCI recipe must declare OpenWrt PKGARCH=all'
# shellcheck disable=SC2016 # Match a literal OpenWrt make variable.
grep -Fq 'include $(INCLUDE_DIR)/package.mk' "$makefile" || fail 'LuCI package does not use the standard package recipe'
if grep -Fq '+tg-ws-proxy' "$makefile"; then fail 'architecture-independent LuCI package must not depend on a core package'; fi
grep -Fq 'PKG_NAME:=luci-app-tg-ws-proxy-rs' "$makefile" || fail 'LuCI package is not named luci-app-tg-ws-proxy-rs'
grep -Fq '/etc/config/tg-ws-proxy-rs' "$makefile" || fail 'UCI conffile is not declared'
# shellcheck disable=SC2016 # Match the literal package staging path in the recipe.
grep -Fq 'chmod 0600 $(1)/etc/config/tg-ws-proxy-rs' "$makefile" || \
    fail 'packaged UCI config secret is not mode 0600'
# shellcheck disable=SC2016 # Match the literal config variable in the installation hook.
grep -Fq 'chmod 0600 "/etc/config/$config"' \
    "$ROOT/openwrt/luci-app/root/etc/uci-defaults/95_luci-tg-ws-proxy-rs" || \
    fail 'UCI config secret is not restricted to mode 0600'

stray="$(find "$ROOT/openwrt/luci-app" -name '*tg-ws-proxy*' ! -name '*tg-ws-proxy-rs*')"
[[ -z "$stray" ]] || fail "packaged path collides with the upstream tg-ws-proxy package: $stray"

cargo_version="$(python3 -c 'import tomllib,sys; print(tomllib.load(open(sys.argv[1], "rb"))["package"]["version"])' "$ROOT/Cargo.toml")"
grep -Fq ",$cargo_version)" "$makefile" || fail "LuCI default version does not match Cargo $cargo_version"
[[ -f "$ROOT/docs/release-notes/$cargo_version.md" ]] || fail "release notes for $cargo_version are missing"
# shellcheck disable=SC2016 # Match a literal Bash parameter expansion.
grep -Fq 'output_name="${output_name//\~/.}"' "$ROOT/openwrt/build-luci-package.sh" || \
    fail 'beta IPK release filename is not normalized before checksums'

workflow="$ROOT/.github/workflows/release.yml"
grep -Fq 'openwrt/build-luci-package.sh' "$workflow" || fail 'release does not build LuCI through the SDK recipe'
grep -Fq -- '--format apk' "$workflow" || fail 'release misses LuCI APK build'
grep -Fq -- '--format ipk' "$workflow" || fail 'release misses LuCI IPK build'
grep -Fq 'sha256sum -- *.tar.gz *.apk *.ipk > SHA256SUMS' "$workflow" || fail 'release checksums do not cover binaries and LuCI packages'
if grep -Eq 'build-package\.sh|release-architectures|dist/openwrt/tg-ws-proxy_|set -- tg-ws-proxy_' "$workflow"; then
    fail 'release workflow still builds core OpenWrt packages'
fi

printf 'PASS: minimal OpenWrt package contract\n'
