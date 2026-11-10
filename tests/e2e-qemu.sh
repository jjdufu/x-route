#!/bin/sh
# shellcheck disable=SC2016,SC2317,SC2034
# XRouter: 端到端冒烟测试（手动运行）：vm.sh 生成的参数 -> QEMU 启动 dist/openwrt 下的真实 Image 与 rootfs，
# 检查 XR_READY、稳定期、last_good、VM 内网络/防火墙/软件包，再停止。
# 用法：sh tests/e2e-qemu.sh（需要 qemu-system-aarch64，先运行 build-kernel.sh / build-rootfs.sh）
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
T="$(mktemp -d)"
fail=0; chk() { if eval "$1"; then echo "ok: $1"; else echo "FAIL: $1"; fail=1; fi; }
mkdir -p "$T/bin" "$T/mod"; cp -a "$ROOT/module/." "$T/mod/"
printf '#!/bin/sh\ncase "$1" in sys.boot_completed) echo 1;; sys.user.0.ce_available) echo true;; esac\n' > "$T/bin/getprop"; chmod +x "$T/bin/getprop"
printf '#!/bin/sh\necho "net $*" >> "%s/net.calls"\n' "$T" > "$T/mod/scripts/net.sh"
[ -f "$T/mod/scripts/share.sh" ] || printf "share_prepare() { share_spec=; share_mode=off; share_wrap=; }\n" > "$T/mod/scripts/share.sh"
export XR_DAEMON_FOREGROUND=1 PATH="$T/bin:$PATH" XR_DATA="$T/data" MODDIR="$T/mod" CROSVM="$ROOT/tests/mock/crosvm-qemu"
trap 'sh "$T/mod/scripts/vm.sh" stop >/dev/null 2>&1; rm -rf "$T"' EXIT
. "$T/mod/scripts/common.sh"; ensure_dirs; cp "$T/mod/config/config.conf" "$CONF"
sed -i "s/^STABLE_SEC=.*/STABLE_SEC=\"${E2E_STABLE:-60}\"/" "$CONF"
cp "$ROOT/dist/openwrt/Image" "$IMG/Image"; cp "$ROOT/dist/openwrt/openwrt-rootfs.img.gz" "$IMG/"
gzip -dc "$IMG/openwrt-rootfs.img.gz" > "$ROOTFS"; truncate -s 1024M "$ROOTFS"
S="ssh -p ${E2E_SSH_PORT:-2222} -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -o PreferredAuthentications=none,password -o NumberOfPasswordPrompts=0 root@127.0.0.1"

sh "$T/mod/scripts/vm.sh" start; chk '[ $? -eq 0 ]'
i=0; while [ $i -lt 240 ] && [ "$(cat "$RUN/state" 2>/dev/null)" != ready ] && [ "$(cat "$RUN/state" 2>/dev/null)" != stable ]; do sleep 2; i=$((i+2)); done
chk 'grep -q "^XR_READY" "$RUN/vm-ctl.log"'
chk 'grep -aq "Linux version 6.12.103" "$RUN/console.log"'
out="$($S 'uname -r; ip -4 -o addr show eth0 | awk "{print \$4}"; ip route | grep default; fw4 check >/dev/null 2>&1 && echo FW4_OK; uci get luci.main.lang; uci get system.@system[0].hostname; cat /tmp/sysinfo/model; uci -q get firewall.vpn_lan || echo no_vpn_lan; which openvpn tcpdump apk; df -m / | awk "NR==2{print \$2}"; grep "^root:" /etc/shadow | cut -d: -f2 | wc -c; ping -c1 -W3 172.29.233.1 >/dev/null && echo GW_OK' </dev/null 2>&1)"
echo "$out" | sed 's/^/  vm> /'
chk 'echo "$out" | grep -qx 6.12.103'
chk 'echo "$out" | grep -qx 172.29.233.254/24'
chk 'echo "$out" | grep -q "default via 172.29.233.1"'
chk 'echo "$out" | grep -qx FW4_OK && echo "$out" | grep -qx zh_cn && echo "$out" | grep -qx GW_OK'
chk 'echo "$out" | grep -qx XRouter && echo "$out" | grep -qx "AMD Ryzen Threadripper PRO 9995WX"'
chk 'echo "$out" | grep -q /usr/sbin/openvpn && echo "$out" | grep -q /usr/bin/apk'
chk '[ "$(echo "$out" | grep -E "^[0-9]+$" | sed -n 1p)" -gt 900 ]'
caps="$($S sh -s < "$ROOT/tests/vm-caps.sh" 2>&1)"
echo "$caps" | sed 's/^/  caps> /'
chk 'echo "$caps" | grep -qx CAPS_OK'
chk 'echo "$out" | grep -qx vpn_lan'
i=0; while [ $i -lt 200 ] && [ "$(cat "$RUN/state" 2>/dev/null)" != stable ]; do sleep 2; i=$((i+2)); done
chk '[ "$(cat "$RUN/state")" = stable ] && [ ! -f "$FUSE" ] && [ "$(cat "$LAST_GOOD")" = "$(config_hash)" ]'
sh "$T/mod/scripts/vm.sh" stop
chk '! pgrep -f "qemu-system-aarch64.*$T" >/dev/null'
[ "$fail" = 0 ] && echo "e2e-qemu OK"
exit "$fail"
