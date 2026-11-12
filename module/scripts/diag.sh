#!/system/bin/sh
# shellcheck disable=SC2034
# ==============================================================================
# XRouter: 调试快照与诊断导出（被 net.sh / vm.sh / keepalive.sh / xr 引入）
#   diag_phone          手机侧网络快照（规则、路由表、XR_ 链、地址、网桥、热点、DNS、运行状态）
#   diag_guest [秒]     经控制目录请求 OpenWrt 快照（host/diag.cmd -> guest/diag.txt，客侧已脱敏）
#   diag_snap 原因      调试模式下把两份快照追加进 debug.log（加锁，同一时间只拍一份）
#   diag_snap_bg 原因   同上，后台执行，不阻塞调用方
#   diag_redact         脱敏过滤器：证书/密钥块、密码类字段、URL、OpenVPN 服务器地址
# 所有命令直接用系统绝对路径，不经 xr_exec，避免快照内容和命令日志交错。
# ==============================================================================
DIAG_SYSBIN="${XR_SYSBIN:-/system/bin}"

_dsec() { echo "## $*"; "$@" 2>&1; echo; }

# OpenVPN 服务器地址（来自导入的配置的 remote 行），用于脱敏；只输出主机名/IP，不输出其他内容
diag_vpn_hosts() {
  for _f in "${XR_DATA}/config/openvpn.ovpn" "${RUN}/guest/host/openvpn.ovpn" "${XR_DATA}/config/vpn"/*.ovpn "${RUN}/guest/host/vpn"/*.ovpn; do
    [ -f "$_f" ] && awk '$1=="remote" && $2!="" {print $2}' "$_f" 2>/dev/null
  done | sort -u
}

diag_redact() {
  _sedf="${RUN}/.redact.$$"
  # ERE（sed -E），toybox / busybox / GNU sed 通用
  cat > "$_sedf" <<'SED'
/-----BEGIN/,/-----END/{/-----END/!d;s/.*/<已隐去证书或密钥>/;}
s#[a-zA-Z][a-zA-Z0-9+.-]*://[^[:space:]"<>]*#<URL>#g
/<(ca|cert|key|tls-auth|tls-crypt|tls-crypt-v2|secret|pkcs12|extra-certs)>/,/<\/(ca|cert|key|tls-auth|tls-crypt|tls-crypt-v2|secret|pkcs12|extra-certs)>/d
s/(([Pp][Aa][Ss][Ss]([Ww][Oo][Rr][Dd]|[Ww][Dd]|[Pp][Hh][Rr][Aa][Ss][Ee])?|[Ss][Ee][Cc][Rr][Ee][Tt]|[Tt][Oo][Kk][Ee][Nn]|[Pp][Ss][Kk]|private_?key|preshared_?key|auth-user-pass)["']?[[:space:]]*[=:][[:space:]]*)[^[:space:]]+/\1<已隐去>/g
s/(option[[:space:]]+(password|passwd|key|secret|token|psk)[[:space:]]+)[^[:space:]]+/\1<已隐去>/g
s/\[AF_INET6?\][^[:space:]]*/[AF_INET]<VPN服务器>/g
SED
  for _h in $(diag_vpn_hosts); do
    printf 's/%s/<VPN服务器>/g\n' "$(printf '%s' "$_h" | sed 's/[].[\*^$/]/\\&/g')" >> "$_sedf"
  done
  sed -E -f "$_sedf"
  rm -f "$_sedf"
}

diag_phone() {
  _ip="${DIAG_SYSBIN}/ip"
  echo "### 手机网络快照 $(date '+%F %T')"
  _dsec "$_ip" -4 rule
  _dsec "$_ip" -6 rule
  _up="$(cat "${RUN}/net.up_if" 2>/dev/null)"
  for _t in main local_network 97 233 234 ${_up}; do
    _dsec "$_ip" -4 route show table "$_t"
    _dsec "$_ip" -6 route show table "$_t"
  done
  _dsec "$_ip" -br addr
  _dsec "$_ip" -br link
  _dsec "$_ip" -6 neigh show proxy
  _dsec "$_ip" neigh
  for _c in "-S FORWARD" "-S XR_FWD" "-nvxL XR_FWD" "-t nat -S POSTROUTING" "-t nat -S XR_NAT" "-S OUTPUT" "-S XR_HS"; do
    # shellcheck disable=SC2086
    _dsec "${DIAG_SYSBIN}/iptables" -w $_c
  done
  for _c in "-S FORWARD" "-S XR_FWD6" "-nvxL XR_FWD6" "-S OUTPUT" "-S XR_HS6"; do
    # shellcheck disable=SC2086
    _dsec "${DIAG_SYSBIN}/ip6tables" -w $_c
  done
  echo "## 网桥"
  for _b in /sys/class/net/xrbr*; do
    [ -d "$_b" ] || continue
    _ports=""; for _pp in "$_b"/brif/*; do [ -e "$_pp" ] && _ports="${_ports} ${_pp##*/}"; done
    echo "${_b##*/}: 端口${_ports:- 无}"
  done
  echo
  echo "## sysctl"
  for _f in /proc/sys/net/ipv4/ip_forward /proc/sys/net/ipv6/conf/all/forwarding; do echo "${_f#/proc/sys/}=$(cat "$_f" 2>/dev/null)"; done
  for _i in xr0 xr1 xrbr1 ${_up} $(cat "${RUN}/net.hs_if" 2>/dev/null); do
    for _k in disable_ipv6 forwarding proxy_ndp accept_ra; do
      [ -f "/proc/sys/net/ipv6/conf/${_i}/${_k}" ] && echo "ipv6.${_i}.${_k}=$(cat "/proc/sys/net/ipv6/conf/${_i}/${_k}")"
    done
  done
  echo
  echo "## 热点"
  for _w in /sys/class/net/*; do
    _n="${_w##*/}"
    [ -d "$_w/wireless" ] || [ -d "$_w/phy80211" ] || continue
    "${DIAG_SYSBIN}/iw" dev "$_n" info 2>/dev/null | grep -E 'Interface|type|ssid|channel'
    "${DIAG_SYSBIN}/iw" dev "$_n" station dump 2>/dev/null | grep -E 'Station|inactive time|connected time|signal:'
  done
  echo "tether_offload_disabled=$(settings get global tether_offload_disabled 2>/dev/null)"
  dumpsys tethering 2>/dev/null | grep -E ' - |Tethering|Upstream|upstream|offload|Offload|state|State' | head -n 60
  echo
  echo "## DNS"
  getprop 2>/dev/null | grep -iE 'net\.dns|dns' | head -n 20
  echo "private_dns_mode=$(settings get global private_dns_mode 2>/dev/null)"
  echo
  echo "## 运行状态"
  for _f in "${RUN}"/state "${RUN}"/net.* "${RUN}"/guest/guest/ipv6 "${RUN}"/guest/guest/openvpn.status "${RUN}"/guest/guest/hotspot; do
    [ -f "$_f" ] && echo "${_f#"${RUN}"/}=$(head -c 300 "$_f" | tr '\n' ' ')"
  done
  echo
}

# $1=最多等待秒数。客侧每 2 秒查一次命令，生成快照约 1–2 秒
diag_guest() {
  _w="${1:-10}"
  _p="$(cat "${RUN}/crosvm.pid" 2>/dev/null)"
  if [ -z "$_p" ] || ! kill -0 "$_p" 2>/dev/null || [ ! -d "${RUN}/guest/host" ]; then
    echo "### OpenWrt 快照：服务未运行"; return 1
  fi
  _ts="$(date +%s)$$"
  echo "snap ${_ts}" > "${RUN}/guest/host/diag.cmd"
  _i=0
  while [ "$_i" -lt "$((_w * 2))" ]; do
    [ "$(head -n 1 "${RUN}/guest/guest/diag.txt" 2>/dev/null)" = "ts=${_ts}" ] && break
    sleep 0.5 2>/dev/null || sleep 1
    _i=$((_i + 1))
  done
  if [ "$(head -n 1 "${RUN}/guest/guest/diag.txt" 2>/dev/null)" = "ts=${_ts}" ]; then
    echo "### OpenWrt 快照 $(date '+%F %T')"
    tail -n +2 "${RUN}/guest/guest/diag.txt"
  else
    echo "### OpenWrt 快照：${_w} 秒内无响应（旧版系统盘不支持，或服务未就绪）"
    return 1
  fi
}

diag_snap() {
  dbg_on || return 0
  _lk="${RUN}/.snap.lock"
  if ! mkdir "$_lk" 2>/dev/null; then
    # 锁超过 60 秒视为残留
    [ -n "$(find "$_lk" -maxdepth 0 -mmin +1 2>/dev/null)" ] || return 0
    rm -rf "$_lk"; mkdir "$_lk" 2>/dev/null || return 0
  fi
  {
    echo "======== $(date '+%F %T') 快照：$1"
    diag_phone
    case "$(cat "${RUN}/state" 2>/dev/null)" in ready|stable) diag_guest "${2:-10}" ;; esac
    echo "======== 快照结束"
  } 2>&1 | diag_redact >> "${DLOG}"
  rmdir "$_lk" 2>/dev/null
  rotate_debug
}

diag_snap_bg() {
  dbg_on || return 0
  ( diag_snap "$1" ) </dev/null >/dev/null 2>&1 &
}

# 相关系统日志：热点、网络栈、netd 等
diag_logcat() {
  logcat -d -v threadtime -t 30000 2>/dev/null \
    | grep -iE 'Tethering|IpServer|hostapd|SoftAp|netd|NetworkStack|DhcpServer|BpfCoordinator|OffloadController|RouterAdvert|ConnectivityService|WifiService|xrouter|xrnotify' \
    | tail -n 4000
}
