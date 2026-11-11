#!/system/bin/sh
# ==============================================================================
# XRouter: crosvm 启停、启动熔断、磁盘扩容与重置（docs/v2-plan/06-vm.md）
# 用法：vm.sh start|stop|status|expand <MB>|reset-disk
# 被 xr 用 . 引入时只定义函数；直接运行时才走文件末尾的分派。
# ==============================================================================
MODDIR="${MODDIR:-/data/adb/modules/xrouter}"
. "${MODDIR}/scripts/common.sh"
. "${MODDIR}/scripts/diag.sh"

CROSVM="${CROSVM:-/apex/com.android.virt/bin/crosvm}"
PID_F="${RUN}/crosvm.pid"
SOCK="${RUN}/crosvm.sock"
CON_LOG="${RUN}/console.log"
CTL_LOG="${RUN}/vm-ctl.log"
VMM_LOG="${XR_DATA}/log/vmm.log"

vm_running() {
  p="$(cat "${PID_F}" 2>/dev/null)"
  is_pid_running "$p" && grep -q crosvm "/proc/$p/cmdline" 2>/dev/null
}

# 启动失败原因（单行，供 WebUI 显示）；再次启动或就绪时清除
vm_fail() {
  log_err vm "$1"; echo "$2" > "${RUN}/last_error"
  # 开机日志留几秒给 WebUI 读到最后一行，再删掉
  ( sleep 5; rm -f "${BOOT_LOG}" ) </dev/null >/dev/null 2>&1 &
}

# 等待就绪期间每秒一行进度：已等待时间、串口已输出多少（OpenWrt 自身的输出走串口那一路）
boot_progress() { # $1=已等待秒数
  _cs="$(stat -c %s "${CON_LOG}" 2>/dev/null || echo 0)"
  blog vm "等待 OpenWrt 就绪 ${1}s · 串口已输出 $((_cs / 1024))KB"
}

# ---- 熔断 ----
fuse_arm()   { printf '%s %s\n' "$(date +%s)" "$(config_hash)" > "${FUSE}"; sync; sleep 1; }
ready_seen() { grep -q '^XR_READY' "${CTL_LOG}" 2>/dev/null || grep -aq 'XR_READY' "${CON_LOG}" 2>/dev/null; }
ready_channel() {
  if grep -q '^XR_READY' "${CTL_LOG}" 2>/dev/null; then echo hvc
  elif grep -aq 'XR_READY' "${CON_LOG}" 2>/dev/null; then echo ttyS0
  else echo null; fi
}

fuse_watch() {
  load_config; t=0
  while [ "$t" -lt "${READY_TIMEOUT_SEC}" ]; do
    vm_running || { rm -f "${FUSE}"; sync; vm_fail "就绪前 crosvm 已退出，详见 vmm.log" "虚拟机启动中退出"; return; }
    ready_seen && break
    sleep 1; t=$((t+1)); boot_progress "$t"
  done
  if ! ready_seen; then vm_fail "${READY_TIMEOUT_SEC} 秒内未就绪，停止 VM" "${READY_TIMEOUT_SEC} 秒内未就绪"; XR_IN_WATCH=1 vm_stop; return; fi
  echo ready > "${RUN}/state"; rm -f "${RUN}/last_error"; log_info vm "XR_READY（$(ready_channel)）"
  blog net "重建转发 / NAT / 接管规则"
  sh "${MODDIR}/scripts/net.sh" sync
  blog net "网络规则就绪，接管：$(cat "${RUN}/net.gw" 2>/dev/null || echo off)"
  load_config; diag_snap_bg "虚拟机就绪"
  t=0
  while [ "$t" -lt "${STABLE_SEC}" ]; do
    vm_running || { rm -f "${FUSE}"; sync; vm_fail "稳定期内 crosvm 退出" "虚拟机意外退出"; return; }
    sleep 5; t=$((t+5))
  done
  rm -f "${FUSE}"; config_hash > "${LAST_GOOD}"; sync
  echo stable > "${RUN}/state"; log_info vm "稳定，熔断已解除，last_good 已更新"
  rm -f "${BOOT_LOG}"
}

# ---- 启动 ----
vm_start_exec() {
  load_config
  vm_running && { log_info vm "已在运行"; return 0; }
  [ -f "${BOOT_LOG}" ] || : > "${BOOT_LOG}"
  rm -f "${RUN}/teardown.last" "${RUN}/shutdown" "${RUN}/shutdown.noted"
  blog vm "准备启动：CPU ${VM_CPUS} 核 · 内存 ${VM_MEM_MB}MB · 系统盘 $([ "${VM_DISK_RW}" = 1 ] && echo 读写 || echo 只读)"
  rm -f "${RUN}/last_error"
  [ -f "${TRIPPED}" ] && { log_err vm "熔断未确认（xr ack-trip）"; return 1; }
  if [ ! -x "${CROSVM}" ] || [ ! -f "${IMG}/Image" ] || [ ! -f "${ROOTFS}" ]; then
    vm_fail "缺少 crosvm 或镜像" "缺少虚拟化组件或系统镜像"; return 1
  fi
  ce_ready || log_info vm "警告：手机未解锁（CE 不可用）"
  blog vm "内核 $(( $(stat -c %s "${IMG}/Image" 2>/dev/null || echo 0) / 1048576 ))MB · 系统盘 $(( $(stat -c %s "${ROOTFS}" 2>/dev/null || echo 0) / 1048576 ))MB · crosvm 就绪"
  mem="${VM_MEM_MB}"; [ "${mem}" -lt 384 ] && mem=512

  net_args=""
  if [ "${VM_NET}" = 1 ]; then
    blog net "创建 xr0 / xr1、登记 netd 本地网络、建立转发规则"
    sh "${MODDIR}/scripts/net.sh" up || { vm_fail "net up 失败" "网络初始化失败"; return 1; }
    blog net "手机侧网络就绪：上游 $(cat "${RUN}/net.up_if" 2>/dev/null || echo 无) · 模式 $(cat "${RUN}/net.mode" 2>/dev/null)"
    . "${MODDIR}/scripts/hotspot.sh"
    net_args="--net tap-name=xr0,mac=${XR_VM_MAC}"
    # 第二块网卡 eth1：热点网桥用（net.sh up 已创建 xr1）
    [ -d /sys/class/net/xr1 ] && net_args="${net_args} --net tap-name=xr1,mac=$(hs_mac)"
  fi

  if [ "${VM_DISK_RW}" = 1 ]; then blk="${ROOTFS},lock=false,root=true"; rootmode=rw
  else blk="${ROOTFS},lock=false,root=true,ro=true"; rootmode=ro; fi

  share_spec=""; share_mode=off; share_wrap=""
  . "${MODDIR}/scripts/share.sh"   # 设置 share_spec、share_mode、share_wrap（09）
  share_prepare
  blog vm "共享目录：${share_mode} · 控制通道 virtiofs"
  mkdir -p "${RUN}/guest/host" "${RUN}/guest/guest"
  chmod 0700 "${RUN}/guest"
  # IPv6 状态以虚拟机内 OpenWrt 的实际配置为准，每次开机由客侧重新上报
  rm -f "${RUN}/guest/host/ipv6.cmd" "${RUN}/guest/guest/ipv6" "${RUN}/guest/host/diag.cmd" "${RUN}/guest/guest/diag.txt" \
    "${RUN}/guest/host/hotspot.cmd" "${RUN}/guest/guest/hotspot"
  ctl_spec="${RUN}/guest:xrctl:type=fs:cache=auto:timeout=1"
  if [ -f "${XR_DATA}/config/openvpn.ovpn" ]; then
    cp -f "${XR_DATA}/config/openvpn.ovpn" "${RUN}/guest/host/openvpn.ovpn"
    echo enable > "${RUN}/guest/host/openvpn.cmd"
    blog vpn "下发 OpenVPN 配置"
  fi

  params="root=/dev/vda ${rootmode} rootwait console=ttyS0,115200 earlycon panic=5"
  params="${params} xr.ip=${XR_NET_PREFIX}.254/24 xr.gw=${XR_NET_PREFIX}.1 xr.dns=$(echo "${XR_DNS}" | tr ' ' ',')"
  params="${params} xr.ip6=${XR_NET6}::254/64 xr.gw6=${XR_NET6}::1"
  params="${params} xr.share=${share_mode}"

  : > "${CON_LOG}"; : > "${CTL_LOG}"; rm -f "${SOCK}" "${RUN}/state"
  dlog vm "启动参数：cpus=${VM_CPUS} mem=${mem} swiotlb=${VM_SWIOTLB_MB} net=${net_args:-无} disk=${rootmode} share=${share_mode} params=${params}"
  fuse_arm
  blog vm "熔断已布防，启动 crosvm（gunyah）"
  # 解除 Android 默认 64KB RLIMIT_MEMLOCK（Gunyah 需要锁定 guest 内存）
  # shellcheck disable=SC3045
  ulimit -l unlimited 2>/dev/null
  [ "${VM_WAKELOCK}" = 1 ] && wakelock_on

  # shellcheck disable=SC2086
  # setsid：即使调用方是守护进程，crosvm 也不跟 WebUI 的进程组绑在一起
  ${share_wrap} setsid nohup "${CROSVM}" --log-level=info run \
    --hypervisor gunyah --protected-vm-without-firmware --disable-sandbox \
    --no-balloon --swiotlb "${VM_SWIOTLB_MB}" --cpus "${VM_CPUS}" --mem "${mem}" \
    ${net_args} \
    --block "${blk}" \
    --serial "type=file,path=${CON_LOG},hardware=serial,num=1,console=true,earlycon=true" \
    --serial "type=file,path=${CTL_LOG},hardware=virtio-console,num=1" \
    --socket "${SOCK}" \
    --shared-dir "${ctl_spec}" \
    ${share_spec:+--shared-dir} ${share_spec:+"${share_spec}"} \
    --params "${params}" \
    "${IMG}/Image" </dev/null >> "${VMM_LOG}" 2>&1 &
  echo $! > "${PID_F}"
  blog vm "crosvm pid $(cat "${PID_F}")，等待内核输出"
  sleep 2
  if ! vm_running; then
    vm_fail "crosvm 立即退出，详见 vmm.log" "虚拟机无法启动"; rm -f "${FUSE}" "${PID_F}"; sync; wakelock_off
    [ "${VM_NET}" = 1 ] && sh "${MODDIR}/scripts/net.sh" down
    return 1
  fi
  ( fuse_watch ) >/dev/null 2>&1 &
  echo $! > "${RUN}/fuse_watch.pid"
  echo "${share_mode}" > "${RUN}/share.mode"
  type cgroup_escape_tree >/dev/null 2>&1 && cgroup_escape_tree
  log_info vm "已启动 pid=$(cat "${PID_F}") mem=${mem}MB cpus=${VM_CPUS} share=${share_mode}"
}

# ---- 停止 ----
vm_stop_exec() {
  # 由 fuse_watch 自身调用时不能杀掉自己
  if [ "${XR_IN_WATCH:-0}" != 1 ]; then
    w="$(cat "${RUN}/fuse_watch.pid" 2>/dev/null)"; [ -n "$w" ] && kill "$w" 2>/dev/null
  fi
  p="$(cat "${PID_F}" 2>/dev/null)"
  load_config; diag_snap "停止前" 5
  if vm_running; then
    "${CROSVM}" stop "${SOCK}" >/dev/null 2>&1
    i=0; while vm_running && [ $i -lt 10 ]; do sleep 1; i=$((i+1)); done
    if vm_running; then kill -TERM "$p" 2>/dev/null; sleep 5; fi
    if vm_running; then kill -KILL "$p" 2>/dev/null; fi
  fi
  rm -f "${PID_F}" "${SOCK}" "${RUN}/state" "${RUN}/fuse_watch.pid" "${RUN}/share.mode" "${FUSE}" "${BOOT_LOG}"; sync
  sh "${MODDIR}/scripts/net.sh" down
  wakelock_off
  log_info vm "已停止"
}

# ---- 状态（JSON 片段，供 xr status-json 使用）----
vm_status() {
  load_config
  running=false; pid=null; up=0
  if vm_running; then
    running=true; pid="$(cat "${PID_F}")"
    st="$(awk '{print $22}' "/proc/${pid}/stat" 2>/dev/null)"; hz=100
    now="$(cut -d. -f1 /proc/uptime)"
    [ -n "$st" ] && up=$(( now - st / hz ))
  fi
  state="$(cat "${RUN}/state" 2>/dev/null)"; [ -n "$state" ] || state=booting
  [ "$running" = true ] || state=stopped
  tr="null"; [ -f "${TRIPPED}" ] && tr="\"$(cat "${TRIPPED}")\""
  lg=false; [ "$(cat "${LAST_GOOD}" 2>/dev/null)" = "$(config_hash)" ] && lg=true
  ch="$(ready_channel)"; [ "$ch" = null ] || ch="\"$ch\""
  er="$(head -n1 "${RUN}/last_error" 2>/dev/null | tr -d "\"\\\\")"
  printf '{"running":%s,"pid":%s,"state":"%s","uptime_s":%s,"tripped":%s,"last_good_match":%s,"ready_channel":%s,"error":"%s"}' \
    "$running" "$pid" "$state" "$up" "$tr" "$lg" "$ch" "$er"
}


# 未在守护进程内、且未开前台模式时，把启停交给独立会话中的 daemon.sh
_ensure_send() {
  if ! type daemon_send >/dev/null 2>&1; then
    # shellcheck disable=SC1091
    . "${MODDIR}/scripts/daemon.sh"
  fi
}

vm_start() {
  if [ "${XR_IN_DAEMON}" = 1 ] || [ "${XR_DAEMON_FOREGROUND}" = 1 ]; then
    vm_start_exec
    return $?
  fi
  _ensure_send
  daemon_send start
}

vm_stop() {
  if [ "${XR_IN_DAEMON}" = 1 ] || [ "${XR_DAEMON_FOREGROUND}" = 1 ]; then
    vm_stop_exec
    return $?
  fi
  _ensure_send
  if daemon_running; then
    daemon_send stop
    return $?
  fi
  vm_stop_exec
}

# ---- 磁盘 ----
disk_expand() { # $1=新大小(MB)，必须大于当前大小，VM 必须已停止
  vm_running && { log_err vm "请先停止 VM"; return 1; }
  is_uint "$1" || return 1
  cur=$(( $(stat -c %s "${ROOTFS}") / 1048576 ))
  [ "$1" -gt "$cur" ] || { log_err vm "新大小必须大于当前 ${cur}MB"; return 1; }
  set_kv VM_DISK_SIZE_MB "$1" || return 1
  truncate -s "$1M" "${ROOTFS}" && sync
  log_info vm "系统盘 ${cur}MB -> $1MB，下次启动时 VM 内执行 resize2fs"
}
disk_reset() {   # 重置系统盘：VM 内装过的包、改过的配置全部丢失
  vm_running && { log_err vm "请先停止 VM"; return 1; }
  [ -f "${IMG}/openwrt-rootfs.img.gz" ] || { log_err vm "缺少 openwrt-rootfs.img.gz"; return 1; }
  load_config
  if ! gzip -dc "${IMG}/openwrt-rootfs.img.gz" > "${ROOTFS}.new" \
     || ! truncate -s "${VM_DISK_SIZE_MB}M" "${ROOTFS}.new" \
     || ! mv -f "${ROOTFS}.new" "${ROOTFS}"; then
    rm -f "${ROOTFS}.new"; log_err vm "重置失败"; return 1
  fi
  sync
  rm -f "${LAST_GOOD}"
  log_info vm "系统盘已重置"
}

if [ "${0##*/}" = vm.sh ]; then
  ensure_dirs
  case "$1" in
    start) vm_start ;;
    stop) vm_stop ;;
    status) vm_status; echo ;;
    expand) disk_expand "$2" ;;
    reset-disk) disk_reset ;;
    *) echo "usage: vm.sh start|stop|status|expand <MB>|reset-disk"; exit 2 ;;
  esac
fi
