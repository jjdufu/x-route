#!/system/bin/sh
# ==============================================================================
# XRouter: 虚拟机流量统计（累计值，不做实时网速）。被 net.sh / xr / daemon.sh 引入。
# 只计数、不改包：filter 表里的专用链，规则全是 RETURN，开销接近零。
#   XR_ACCT      分发：-i xr0 -> XR_ACCT_UP（虚拟机发出，上传），-o xr0 -> XR_ACCT_DN（发往虚拟机，下载）
#   XR_ACCT_UP   按目的地址分：内网网段各一条 RETURN，最后一条无条件 RETURN 即公网
#   XR_ACCT_DN   按源地址分，结构同上
# 挂载点：XR_FWD / XR_FWD6 第一条（转发），以及 INPUT / OUTPUT（虚拟机与手机本机互访）。
# IPv6 同构（XR_ACCT6*），仅在 ip6tables filter 可用时启用，否则只统计 IPv4。
# 内网 = 虚拟机网段、私有 / CGNAT / 链路本地 / 组播、ULA，以及 OpenVPN 同步的路由（routes.active）。
# 持久化：规则重建或拆除前先把计数折算进 traffic.base；守护进程定期写 traffic.snap，
# 链被意外清空（整机重启）后以 snap 接续。总量 = base + 当前计数。
# 数值可能超过 32 位，所有加法都交给 awk，不用 shell 算术。
# ==============================================================================
SYSBIN="${XR_SYSBIN:-${SYSBIN:-/system/bin}}"
IPT="${IPT:-xr_exec ${SYSBIN}/iptables -w}"
IP6T="${IP6T:-xr_exec ${SYSBIN}/ip6tables -w}"
XR_IF="${XR_IF:-xr0}"
ACCT_BASE="${XR_DATA}/config/traffic.base"
ACCT_SNAP="${XR_DATA}/config/traffic.snap"
ACCT_SINCE="${XR_DATA}/config/traffic.since"

acct_lan4() {
  echo "10.0.0.0/8 172.16.0.0/12 192.168.0.0/16 100.64.0.0/10 169.254.0.0/16 127.0.0.0/8 224.0.0.0/4 255.255.255.255/32"
  [ -n "${XR_NET_PREFIX:-}" ] && echo "${XR_NET_PREFIX}.0/24"
  for _c in $(cat "${RUN}/routes.active" 2>/dev/null); do
    case "$_c" in *:*) ;; *) echo "$_c" ;; esac
  done
}
acct_lan6() {
  echo "fc00::/7 fe80::/10 ff00::/8 ::1/128"
  for _c in $(cat "${RUN}/routes.active" 2>/dev/null); do
    case "$_c" in *:*) echo "$_c" ;; esac
  done
}

# 读 4 个计数（公网上传 公网下载 内网上传 内网下载），文件缺失或损坏时为 0
acct_read_file() {
  awk 'NR==1{ok=1; for(i=1;i<=4;i++){v=$i; if(v !~ /^[0-9]+$/) v=0; o=o (i>1?" ":"") v}; print o; exit} END{if(!ok) print "0 0 0 0"}' "$1" 2>/dev/null \
    || echo "0 0 0 0"
}
acct_write_file() {
  _tf="$1.tmp.$$"
  echo "$2" > "$_tf" && mv -f "$_tf" "$1"
}
# $1 $2：两组 4 元组；$3=add|max
acct_combine() {
  echo "$1 $2" | awk -v op="${3:-add}" '{for(i=1;i<=4;i++){a=$i+0; b=$(i+4)+0; r=(op=="max")?(a>b?a:b):a+b; o=o (i>1?" ":"") sprintf("%.0f", r)}; print o}'
}

# 一条链的计数：输出「内网字节 公网字节」。最后一条规则是兜底的公网规则。
_acct_chain() {
  $1 -nvxL "$2" 2>/dev/null | awk 'NR>2 && $2 ~ /^[0-9]+$/ {n++; b[n]=$2}
    END{ if(!n){print "0 0"; exit}; l=0; for(i=1;i<n;i++) l+=b[i]; printf "%.0f %.0f\n", l, b[n] }'
}

acct_present() { ${IPT} -S XR_ACCT_UP >/dev/null 2>&1; }
acct_present6() { ${IP6T} -S XR_ACCT6_UP >/dev/null 2>&1; }

# 当前内核计数：公网上传 公网下载 内网上传 内网下载
acct_live() {
  _u="$(_acct_chain "${IPT}" XR_ACCT_UP)"; _d="$(_acct_chain "${IPT}" XR_ACCT_DN)"
  if acct_present6; then
    _u6="$(_acct_chain "${IP6T}" XR_ACCT6_UP)"; _d6="$(_acct_chain "${IP6T}" XR_ACCT6_DN)"
  else
    _u6="0 0"; _d6="0 0"
  fi
  echo "$_u $_d $_u6 $_d6" | awk '{printf "%.0f %.0f %.0f %.0f\n", $2+$6, $4+$8, $1+$5, $3+$7}'
}

# 简单互斥：规则重建（net watch）与快照（守护进程）可能同时发生
acct_lock() {
  _i=0
  while ! mkdir "${RUN}/acct.lock" 2>/dev/null; do
    _i=$((_i + 1))
    [ "$_i" -ge 30 ] && { rm -rf "${RUN}/acct.lock"; mkdir "${RUN}/acct.lock" 2>/dev/null; break; }
    sleep 0.1 2>/dev/null || sleep 1
  done
}
acct_unlock() { rmdir "${RUN}/acct.lock" 2>/dev/null; }

# 累计总量：链在时 base + 当前计数；链不在（已拆或整机重启）时取 base 与 snap 的较大者
acct_total() {
  _b="$(acct_read_file "${ACCT_BASE}")"
  if acct_present; then
    acct_combine "$_b" "$(acct_live)" add
  else
    acct_combine "$_b" "$(acct_read_file "${ACCT_SNAP}")" max
  fi
}

# 把当前计数并入 base（调用方随后清空计数），snap 同步
_acct_fold() {
  _t="$(acct_combine "$(acct_read_file "${ACCT_BASE}")" "$(acct_live)" add)"
  acct_write_file "${ACCT_BASE}" "$_t"
  acct_write_file "${ACCT_SNAP}" "$_t"
}

# $1=iptables 命令 $2=链前缀（XR_ACCT|XR_ACCT6） $3=内网列表
_acct_rules() {
  _ip="$1"; _p="$2"
  ${_ip} -F "${_p}_UP"; ${_ip} -F "${_p}_DN"
  for _c in $3; do
    ${_ip} -A "${_p}_UP" -d "$_c" -j RETURN
    ${_ip} -A "${_p}_DN" -s "$_c" -j RETURN
  done
  ${_ip} -A "${_p}_UP" -j RETURN
  ${_ip} -A "${_p}_DN" -j RETURN
}

# 新建一组链；已存在返回 1
_acct_new() {
  _ip="$1"; _p="$2"
  ${_ip} -N "${_p}_UP" 2>/dev/null || return 1
  ${_ip} -N "${_p}_DN" 2>/dev/null
  ${_ip} -N "${_p}" 2>/dev/null
  ${_ip} -F "${_p}"
  ${_ip} -A "${_p}" -i "${XR_IF}" -j "${_p}_UP"
  ${_ip} -A "${_p}" -o "${XR_IF}" -j "${_p}_DN"
  return 0
}

_acct_hooks() {
  _ip="$1"; _p="$2"
  ${_ip} -C INPUT -i "${XR_IF}" -j "${_p}" 2>/dev/null || ${_ip} -I INPUT 1 -i "${XR_IF}" -j "${_p}"
  ${_ip} -C OUTPUT -o "${XR_IF}" -j "${_p}" 2>/dev/null || ${_ip} -I OUTPUT 1 -o "${XR_IF}" -j "${_p}"
}
acct_hooks() {
  acct_present && _acct_hooks "${IPT}" XR_ACCT
  acct_present6 && _acct_hooks "${IP6T}" XR_ACCT6
  return 0
}

# 建链或按内网列表重建规则（幂等，列表不变时不动计数）。$1=1 表示同时统计 IPv6
acct_setup() {
  _l4="$(acct_lan4 | tr ' ' '\n' | grep . | sort -u | tr '\n' ' ')"
  _l6=""; [ "${1:-0}" = 1 ] && _l6="$(acct_lan6 | tr ' ' '\n' | grep . | sort -u | tr '\n' ' ')"
  _sig="4:${_l4}|6:${_l6}"
  acct_lock
  _fresh=0
  _acct_new "${IPT}" XR_ACCT && _fresh=1
  _fresh6=0
  if [ "${1:-0}" = 1 ] && _acct_new "${IP6T}" XR_ACCT6; then _fresh6=1; fi
  if [ "$_fresh" = 1 ]; then
    # 链是新建的（首次或整机重启后）：从上次快照接续，计数为 0
    acct_write_file "${ACCT_BASE}" "$(acct_combine "$(acct_read_file "${ACCT_BASE}")" "$(acct_read_file "${ACCT_SNAP}")" max)"
    [ -f "${ACCT_SINCE}" ] || date +%s > "${ACCT_SINCE}"
  fi
  if [ "$_fresh" = 1 ] || [ "$_fresh6" = 1 ] || [ "$_sig" != "$(cat "${RUN}/acct.sig" 2>/dev/null)" ]; then
    [ "$_fresh" = 1 ] || _acct_fold
    _acct_rules "${IPT}" XR_ACCT "$_l4"
    [ "${1:-0}" = 1 ] && _acct_rules "${IP6T}" XR_ACCT6 "$_l6"
    echo "$_sig" > "${RUN}/acct.sig"
  fi
  acct_unlock
  acct_hooks
}

_acct_drop() {
  _ip="$1"; _p="$2"
  while ${_ip} -D INPUT -i "${XR_IF}" -j "${_p}" 2>/dev/null; do :; done
  while ${_ip} -D OUTPUT -o "${XR_IF}" -j "${_p}" 2>/dev/null; do :; done
  for _c in "${_p}" "${_p}_UP" "${_p}_DN"; do ${_ip} -F "$_c" 2>/dev/null; done
  for _c in "${_p}" "${_p}_UP" "${_p}_DN"; do ${_ip} -X "$_c" 2>/dev/null; done
}

# 拆除：先把计数折算保存，再删钩子与链（调用方需先拆掉 XR_FWD / XR_FWD6 里的跳转）
acct_teardown() {
  acct_lock
  acct_present && _acct_fold
  _acct_drop "${IPT}" XR_ACCT
  _acct_drop "${IP6T}" XR_ACCT6
  rm -f "${RUN}/acct.sig"
  acct_unlock
}

# 守护进程定期调用：把当前总量写进 snap（变化时才写，避免频繁写盘）
acct_snapshot() {
  acct_present || return 0
  acct_lock
  _t="$(acct_total)"
  [ "$_t" = "$(acct_read_file "${ACCT_SNAP}")" ] || acct_write_file "${ACCT_SNAP}" "$_t"
  acct_unlock
}

acct_reset() {
  acct_lock
  acct_present && { ${IPT} -Z XR_ACCT_UP; ${IPT} -Z XR_ACCT_DN; }
  acct_present6 && { ${IP6T} -Z XR_ACCT6_UP; ${IP6T} -Z XR_ACCT6_DN; }
  acct_write_file "${ACCT_BASE}" "0 0 0 0"
  acct_write_file "${ACCT_SNAP}" "0 0 0 0"
  date +%s > "${ACCT_SINCE}"
  acct_unlock
}

# 人类可读：B/KB/MB/GB/TB，一位小数（1024 进制）
acct_human() {
  echo "$1" | awk '{v=$1+0; split("B KB MB GB TB", u, " "); i=1; while(v>=1024 && i<5){v/=1024; i++}
    if(i==1) printf "%d B\n", v; else printf "%.1f %s\n", v, u[i]}'
}

acct_json() {
  _t="$(acct_total)"
  _s="$(cat "${ACCT_SINCE}" 2>/dev/null)"; case "$_s" in ''|*[!0-9]*) _s=0 ;; esac
  _on=false; acct_present && _on=true
  _v6=false; acct_present6 && _v6=true
  echo "$_t" | awk -v s="$_s" -v on="$_on" -v v6="$_v6" '{
    printf "{\"wan\":{\"up\":%.0f,\"down\":%.0f},\"lan\":{\"up\":%.0f,\"down\":%.0f},\"total\":{\"up\":%.0f,\"down\":%.0f},\"since\":%s,\"counting\":%s,\"ipv6\":%s}\n",
      $1, $2, $3, $4, $1+$3, $2+$4, s, on, v6 }'
}
