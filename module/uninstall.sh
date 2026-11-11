#!/system/bin/sh
# XRouter: 彻底卸载。每一步都容错，最后删除全部数据（含系统盘与日志）。
MODDIR="${0%/*}"
XR_DATA="${XR_DATA:-/data/adb/xrouter}"
RUN="${XR_DATA}/run"
SYSBIN="${XR_SYSBIN:-/system/bin}"
CROSVM="${CROSVM:-/apex/com.android.virt/bin/crosvm}"
ip()  { "${SYSBIN}/ip" "$@"; }
ndc() { "${SYSBIN}/ndc" "$@"; }
IPT="${SYSBIN}/iptables -w"
IP6T="${SYSBIN}/ip6tables -w"

# 1. 经正式路径停止虚拟机与网络（失败继续）
[ -f "${MODDIR}/scripts/xr" ] && sh "${MODDIR}/scripts/xr" stop >/dev/null 2>&1
[ -f "${MODDIR}/scripts/net.sh" ] && sh "${MODDIR}/scripts/net.sh" down >/dev/null 2>&1

# 2. 结束本模块记录的进程，并再扫一遍残留
if [ -d "${RUN}" ]; then
  for f in "${RUN}"/*.pid; do
    [ -f "$f" ] || continue
    p="$(cat "$f" 2>/dev/null)"
    [ -n "$p" ] && kill -TERM "$p" 2>/dev/null
  done
fi
[ -S "${RUN}/crosvm.sock" ] && "${CROSVM}" stop "${RUN}/crosvm.sock" >/dev/null 2>&1

kill_match() {
  sig="$1"
  for d in /proc/[0-9]*; do
    pid="${d#/proc/}"
    [ "$pid" = "$$" ] && continue
    cmd="$( { tr '\0' ' ' < "${d}/cmdline"; } 2>/dev/null )"
    [ -n "$cmd" ] || continue
    case "$cmd" in *uninstall.sh*) continue ;; esac
    case "$cmd" in
      *crosvm*"${XR_DATA}"*|*"${XR_DATA}/images"*|*"${MODDIR}/scripts/daemon.sh"*|*"${MODDIR}/scripts/watchdog.sh"*|*"${MODDIR}/scripts/net.sh"*|xrnotify|xrnotify\ *)
        kill "$sig" "$pid" 2>/dev/null
        ;;
    esac
  done
}
kill_match -TERM
sleep 1
# 对仍挂着的 pid 文件再发 KILL
if [ -d "${RUN}" ]; then
  for f in "${RUN}"/*.pid; do
    [ -f "$f" ] || continue
    p="$(cat "$f" 2>/dev/null)"
    [ -n "$p" ] && kill -KILL "$p" 2>/dev/null
  done
fi
kill_match -KILL

# 3. 网络兜底（脚本 down 失败时仍要拆干净）
while ${IPT} -D FORWARD -j XR_FWD 2>/dev/null; do :; done
${IPT} -F XR_FWD 2>/dev/null
${IPT} -X XR_FWD 2>/dev/null
while ${IPT} -t nat -D POSTROUTING -j XR_NAT 2>/dev/null; do :; done
${IPT} -t nat -F XR_NAT 2>/dev/null
${IPT} -t nat -X XR_NAT 2>/dev/null
while ${IP6T} -D FORWARD -j XR_FWD6 2>/dev/null; do :; done
${IP6T} -F XR_FWD6 2>/dev/null
${IP6T} -X XR_FWD6 2>/dev/null
while ${IP6T} -t nat -D POSTROUTING -j XR_NAT6 2>/dev/null; do :; done
${IP6T} -t nat -F XR_NAT6 2>/dev/null
${IP6T} -t nat -X XR_NAT6 2>/dev/null
# 流量统计链（只计数）：先摘 INPUT/OUTPUT 钩子，再清空删除
for t in "${IPT}" "${IP6T}"; do
  for c in XR_ACCT XR_ACCT6; do
    while ${t} -D INPUT -i xr0 -j "$c" 2>/dev/null; do :; done
    while ${t} -D OUTPUT -o xr0 -j "$c" 2>/dev/null; do :; done
    for s in "$c" "${c}_UP" "${c}_DN"; do ${t} -F "$s" 2>/dev/null; done
    for s in "$c" "${c}_UP" "${c}_DN"; do ${t} -X "$s" 2>/dev/null; done
  done
done
# 热点网桥：出口过滤链、网桥（端口先摘下，热点网卡交还安卓）、第二块 tap
while ${IPT} -D OUTPUT -j XR_HS 2>/dev/null; do :; done
${IPT} -F XR_HS 2>/dev/null; ${IPT} -X XR_HS 2>/dev/null
while ${IP6T} -D OUTPUT -j XR_HS6 2>/dev/null; do :; done
${IP6T} -F XR_HS6 2>/dev/null; ${IP6T} -X XR_HS6 2>/dev/null
if [ -d /sys/class/net/xrbr1 ]; then
  for p in /sys/class/net/xrbr1/brif/*; do [ -e "$p" ] && ip link set "${p##*/}" nomaster 2>/dev/null; done
  ip link del xrbr1 2>/dev/null
fi
ip link del xr1 2>/dev/null
ndc ipfwd disable xrouter >/dev/null 2>&1
ndc ipfwd disable xroute >/dev/null 2>&1
# pref：访问 VM 网段、VPN 网段、VM 出网、全局接管（见 docs/xrouter-plan/04）
for pref in 4800 4801 18800 18900 19050 19060 19100; do
  while ip rule del pref "${pref}" 2>/dev/null; do :; done
  while ip -6 rule del pref "${pref}" 2>/dev/null; do :; done
done
ip route flush table 97 dev xr0 2>/dev/null
ip route flush table 233 2>/dev/null
ip -6 route flush table 97 dev xr0 2>/dev/null
ip -6 route flush table 233 2>/dev/null
ip -6 route flush table 234 2>/dev/null
ip -6 route flush table 235 2>/dev/null
ip link del xr0 2>/dev/null

# 4. 常驻通知（android 身份发出，不随进程退出消失，须显式撤销）与唤醒锁
#    守护进程、看门狗、通知进程都已结束，不会再补发；撤销后以 dumpsys 核对，最多重试 3 次。
#    系统未开机完成（例如开机阶段执行卸载）时 system_server 还不可用：把 jar 复制到临时目录，
#    后台等开机完成后再撤，撤完删掉临时 jar。
notify_present() { dumpsys notification 2>/dev/null | grep -q "id=233 tag=xrouter"; }
notify_cancel_once() {
  "${SYSBIN}/app_process" -Djava.class.path="$1" /system/bin \
    --nice-name=xrnotify-cancel com.xrouter.notify.Main cancel >/dev/null 2>&1
}
if [ -f "${MODDIR}/bin/xrnotify.jar" ]; then
  if [ "$(getprop sys.boot_completed)" = 1 ]; then
    n=0
    while [ "$n" -lt 3 ]; do
      notify_cancel_once "${MODDIR}/bin/xrnotify.jar"
      notify_present || break
      sleep 1; n=$((n + 1))
    done
  else
    tj="/data/local/tmp/xrnotify-cancel.jar"
    cp -f "${MODDIR}/bin/xrnotify.jar" "$tj" 2>/dev/null && chmod 0644 "$tj"
    # shellcheck disable=SC2016
    setsid sh -c 'until [ "$(getprop sys.boot_completed)" = 1 ]; do sleep 3; done; sleep 10
      "$1/app_process" -Djava.class.path="$2" /system/bin --nice-name=xrnotify-cancel com.xrouter.notify.Main cancel
      rm -f "$2"' xr-cancel "${SYSBIN}" "$tj" </dev/null >/dev/null 2>&1 &
  fi
fi
{ echo xrouter > /sys/power/wake_unlock; } 2>/dev/null
{ echo x-route > /sys/power/wake_unlock; } 2>/dev/null

# 5. 删除数据目录（含日志、配置、镜像、系统盘、运行时文件）
#    不删除用户导出到 Download 的 xrouter-logs-*.zip
rm -rf "${XR_DATA}"
rm -rf /data/adb/x-route
sync
exit 0
