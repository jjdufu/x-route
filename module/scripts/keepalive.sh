#!/system/bin/sh
# XRouter: 保活辅助（cgroup 脱离、异常退出撤路由、通知校验、电池优化白名单）。
# 由 daemon.sh / service.sh / watchdog.sh 引入。
# ColorOS 上 cmd notification post --ongoing 可能打印 usage 仍返回 0，不得把退出码 0 当成发出成功。
# shellcheck disable=SC2034

XR_KEEPALIVE_PKG="${XR_KEEPALIVE_PKG:-com.xrouter.keepalive}"
XR_KEEPALIVE_SVC="${XR_KEEPALIVE_SVC:-com.xrouter.keepalive/.KeepAliveService}"
# 测试时可把 cgroup 根指到临时目录
XR_CGROUP_ROOT="${XR_CGROUP_ROOT:-/sys/fs/cgroup}"
XR_PROC_BASE="${XR_PROC_BASE:-/proc}"

# 把进程从管理器/应用 cgroup 挪到系统或根 cgroup，避免上滑或一键清理时被连带杀掉。
# 仅 setsid/nohup 不够：进程仍可能留在 SukiSU 的 apps/uid_*/pid_* 下。
cgroup_escape() {
  _pid="$1"
  [ -n "$_pid" ] || return 1
  is_pid_running "$_pid" || return 1
  _root="${XR_CGROUP_ROOT}"
  # 依次尝试：cgroup v2 根、system 片、uid_0；以及传统 memcg / cpuset
  for _f in \
      "${_root}/cgroup.procs" \
      "${_root}/system/cgroup.procs" \
      "${_root}/uid_0/cgroup.procs" \
      /dev/memcg/system/cgroup.procs \
      /dev/cpuset/foreground/cgroup.procs \
      /dev/cpuset/foreground/tasks \
      /dev/stune/foreground/cgroup.procs \
      /dev/stune/foreground/tasks
  do
    [ -w "$_f" ] || continue
    # 只要有一处写入成功即记下，其余能写也写（cpu/mem 控制器可能分挂）
    echo "$_pid" > "$_f" 2>/dev/null || continue
  done
  echo -1000 > "${XR_PROC_BASE}/${_pid}/oom_score_adj" 2>/dev/null
  return 0
}

# 虚拟机是否在跑。看门狗不引入 vm.sh 时也必须可靠：未定义 vm_running 就直接看 crosvm.pid。
# 不能把“函数不存在”当成“虚拟机已死”，否则会误撤 xr0，导致 crosvm 写 tap 报 EBADFD、整机断网。
xr_vm_alive() {
  if type vm_running >/dev/null 2>&1; then
    vm_running 2>/dev/null
    return $?
  fi
  _p="$(cat "${RUN}/crosvm.pid" 2>/dev/null)"
  [ -n "$_p" ] || return 1
  [ -d "${XR_PROC_BASE}/${_p}" ] || return 1
  grep -q crosvm "${XR_PROC_BASE}/${_p}/cmdline" 2>/dev/null
}

# 当前 cgroup 路径（去掉控制器前缀），供测试与日志
cgroup_path() {
  _pid="$1"
  _f="${XR_PROC_BASE}/${_pid}/cgroup"
  [ -r "$_f" ] || return 1
  sed -n 's/^0:://p; s/^[^:]*:[^:]*://p' "$_f" 2>/dev/null | head -1
}

# 进程是否仍落在应用/管理器 cgroup 里
cgroup_in_apps() {
  _p="$(cgroup_path "$1")" || return 1
  case "$_p" in
    /apps/*|apps/*|*/apps/uid_*|*/uid_*/pid_*|*suki*) return 0 ;;
  esac
  return 1
}

# 把守护进程及其子进程（crosvm / net_watch / fuse_watch）一并脱离
cgroup_escape_tree() {
  cgroup_escape "${1:-$$}"
  for _f in "${RUN}/crosvm.pid" "${RUN}/net_watch.pid" "${RUN}/fuse_watch.pid" "${RUN}/watchdog.pid" "${RUN}/daemon.pid"; do
    _p="$(cat "$_f" 2>/dev/null)"
    [ -n "$_p" ] && cgroup_escape "$_p"
  done
}

# 息屏 / Doze / 电池优化：用 root 把保活应用加入白名单。不为此再加永久唤醒锁。
doze_whitelist() {
  _pkg="${1:-$XR_KEEPALIVE_PKG}"
  dumpsys deviceidle whitelist "+${_pkg}" >/dev/null 2>&1
  cmd appops set "$_pkg" RUN_IN_BACKGROUND allow >/dev/null 2>&1
  cmd appops set "$_pkg" RUN_ANY_IN_BACKGROUND allow >/dev/null 2>&1
  cmd appops set "$_pkg" START_FOREGROUND allow >/dev/null 2>&1
  cmd appops set "$_pkg" AUTO_REVOKE_PERMISSIONS_IF_UNUSED ignore >/dev/null 2>&1
  cmd netpolicy add restrict-background-whitelist "$_pkg" >/dev/null 2>&1
  return 0
}

# 通知是否真的出现在状态栏（不能只看命令退出码）
notify_visible() {
  dumpsys notification --noredact 2>/dev/null | grep -qE 'xrouter|XRouter|com.xrouter.keepalive' && return 0
  dumpsys notification 2>/dev/null | grep -qE 'xrouter|XRouter|com.xrouter.keepalive'
}

notify_cmd_failed() {
  # 退出码 0 仍可能是打印了 usage / 入队后找不到通知
  _out="$1"
  _rc="$2"
  [ "$_rc" -ne 0 ] && return 0
  case "$_out" in
    *usage*|*Usage*|*find*notification*|*unknown*option*|*not*found*|*Error:*|*error:*) return 0 ;;
  esac
  return 1
}

apk_installed() {
  pm path "${XR_KEEPALIVE_PKG}" >/dev/null 2>&1
}

# 通过标准输入流式安装：system_server 读不到 /data/adb 下的文件（SELinux），直接给路径会失败。
install_keepalive_apk() {
  apk_installed && return 0
  _apk="${MODDIR}/apk/keepalive.apk"
  [ -f "$_apk" ] || return 1
  _sz="$(wc -c < "$_apk" | tr -d ' ')"
  _out="$(pm install -r -g -S "$_sz" < "$_apk" 2>&1)"
  case "$_out" in
    *Success*) return 0 ;;
  esac
  # 回退：复制到 shell 可读的临时目录再装
  _tmp="/data/local/tmp/xrouter-keepalive.apk"
  cp "$_apk" "$_tmp" 2>/dev/null && chmod 0644 "$_tmp" 2>/dev/null
  _out2="$(pm install -r -g "$_tmp" 2>&1)"
  rm -f "$_tmp"
  case "$_out2" in
    *Success*) return 0 ;;
  esac
  type log_err >/dev/null 2>&1 && log_err daemon "保活应用安装失败：$(echo "$_out $_out2" | tr '\n' ' ' | cut -c1-200)"
  return 1
}

# 用伴生前台服务发出常驻通知（ColorOS 上比 cmd notification 可靠）
notify_apk_start() {
  install_keepalive_apk || apk_installed || return 1
  doze_whitelist "${XR_KEEPALIVE_PKG}"
  am start-foreground-service -n "${XR_KEEPALIVE_SVC}" --es text "运行中" >/dev/null 2>&1 \
    || am startservice -n "${XR_KEEPALIVE_SVC}" --es text "运行中" >/dev/null 2>&1 \
    || return 1
  return 0
}

notify_apk_stop() {
  am stopservice -n "${XR_KEEPALIVE_SVC}" >/dev/null 2>&1
  return 0
}

# 尝试用 cmd notification 发送，但必须检查输出并确认通知可见；--ongoing 不作首选
notify_cmd_post() {
  _title="${1:-XRouter}"
  _text="${2:-运行中}"
  _out="$(cmd notification post -t "$_title" xrouter "$_text" 2>&1)"
  _rc=$?
  if ! notify_cmd_failed "$_out" "$_rc" && notify_visible; then
    return 0
  fi
  # 个别系统需要 --ongoing，仍须校验；失败不得当作已发出
  _out="$(cmd notification post --ongoing -t "$_title" xrouter "$_text" 2>&1)"
  _rc=$?
  if ! notify_cmd_failed "$_out" "$_rc" && notify_visible; then
    return 0
  fi
  return 1
}

notify_cancel() {
  notify_apk_stop
  cmd notification cancel xrouter >/dev/null 2>&1
  cmd notification cancel tag xrouter >/dev/null 2>&1
  return 0
}

notify_update() {
  if xr_vm_alive; then
    # 先走伴生 APK 前台服务；失败再试 cmd，且必须看得见通知
    if notify_apk_start && notify_visible; then
      return 0
    fi
    if notify_cmd_post "XRouter" "运行中"; then
      return 0
    fi
    log_info daemon "无法显示常驻通知（cmd 输出不可信，已尝试回退）"
    return 1
  else
    notify_cancel
  fi
}

# 虚拟机意外退出时尽量写出原因
vm_exit_reason() {
  _p="$(cat "${RUN}/crosvm.pid" 2>/dev/null)"
  if [ -n "$_p" ] && [ -r "${XR_PROC_BASE}/${_p}/cmdline" ]; then
    echo "crosvm 仍在运行"
    return 0
  fi
  if [ -f "${XR_DATA}/log/vmm.log" ]; then
    _tail="$(tail -n 8 "${XR_DATA}/log/vmm.log" 2>/dev/null | tr '\n' ' ' | tr -s ' ')"
    [ -n "$_tail" ] && { echo "crosvm 已退出：${_tail}"; return 0; }
  fi
  echo "crosvm 已退出，日志未记录退出原因"
}

# 立刻撤销全局路由与规则，避免 19100 仍指向已断开的 xr0
routes_emergency_down() {
  if [ -f "${MODDIR}/scripts/net.sh" ]; then
    sh "${MODDIR}/scripts/net.sh" down >/dev/null 2>&1
  fi
}

# $1=原因  守护进程或虚拟机异常退出时调用
emergency_teardown() {
  _why="${1:-未知原因}"
  log_err daemon "异常退出：${_why}"
  if xr_vm_alive; then
    # 虚拟机还在，只记原因，把路由留给仍在跑的 xr0；由看门狗拉起守护进程
    return 0
  fi
  echo "虚拟机意外退出" > "${RUN}/last_error"
  rm -f "${RUN}/state" "${RUN}/crosvm.pid"
  routes_emergency_down
  notify_cancel
}

# xr0 已无载波或网卡不在，但接管规则还在
stale_gateway() {
  [ -f "${RUN}/net.gw" ] || return 1
  if xr_vm_alive; then
    _car="${XR_SYSNET:-/sys/class/net}/xr0/carrier"
    [ -f "$_car" ] && [ "$(cat "$_car" 2>/dev/null)" = 0 ] && return 0
    return 1
  fi
  return 0
}
