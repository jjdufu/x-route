#!/bin/sh
# shellcheck disable=SC2016,SC2317,SC2034
# XRouter: 虚拟机内脚本（xrouter-tunmtu / xrouter-vpnstat / xrouter-acct）模拟测试：假 ip / nft / logger，假 /sys/class/net
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
LX="$ROOT/openwrt/files/usr/libexec"
T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT
fail=0; chk() { if eval "$1"; then :; else echo "FAIL: $1"; fail=1; fi; }
mkdir -p "$T/bin" "$T/sys/eth0" "$T/ctl/host" "$T/ctl/guest"
export XRG_SYSNET="$T/sys" XRG_CTL="$T/ctl" XRG_TUN="$T/tun" XRG_TMP="$T/vs" PATH="$T/bin:$PATH" MOCK="$T"
echo 1500 > "$T/sys/eth0/mtu"
cat > "$T/bin/ip" <<'EOF'
#!/bin/sh
echo "ip $*" >> "$MOCK/ip.calls"
case "$*" in
  "link set dev "*" mtu "*) echo "$6" > "$XRG_SYSNET/$4/mtu" ;;
  "-4 -o addr show dev tun0") echo "5: tun0    inet 10.8.0.5/24 scope global tun0" ;;
  "-4 -o addr show dev tun1") ;;
  "-6 -o addr show dev tun1 scope global") echo "6: tun1    inet6 fd00:9::6/64 scope global" ;;
  "-4 route show dev tun0") printf '%s\n' "0.0.0.0/1 via 10.8.0.1" "10.8.0.0/24 proto kernel" "192.168.31.0/24 via 10.8.0.1" ;;
  "-4 route show dev tun1") echo "192.168.50.0/24 via 10.9.0.1" ;;
esac
exit 0
EOF
cat > "$T/bin/nft" <<'EOF'
#!/bin/sh
echo "nft $*" >> "$MOCK/nft.calls"
[ "$1" = -f ] && cp "$2" "$MOCK/nft.last"
[ "$1" = list ] && [ -f "$MOCK/nft.last" ] && exit 0
[ "$1" = list ] && exit 1
exit 0
EOF
printf '#!/bin/sh\necho "$*" >> "$MOCK/logger.calls"\n' > "$T/bin/logger"
printf '#!/bin/sh\nexit 1\n' > "$T/bin/pidof"
printf '#!/bin/sh\nexit 1\n' > "$T/bin/uci"
chmod +x "$T/bin/"*
TM="sh $LX/xrouter-tunmtu"; VS="sh $LX/xrouter-vpnstat"
# busybox sh（独立 applet 模式）优先用自带的 ip / nft 等，假命令不生效：跳过
[ "$(command -v ip)" = "$T/bin/ip" ] || { echo "test-guest skip (applets shadow mocks)"; exit 0; }

# 1. 每条 tun* 各按外层协议扣封装开销（不写死 tun0）；上游 MTU 取手机侧 host/mtu
mkdir -p "$T/sys/tun0" "$T/sys/tun1"; echo 1500 > "$T/sys/tun0/mtu"; echo 1500 > "$T/sys/tun1/mtu"
echo 1410 > "$T/ctl/host/mtu"
$TM proto tun0 udp4; $TM proto tun1 tcp6; $TM apply
chk '[ "$(cat "$T/sys/tun0/mtu")" = 1342 ] && [ "$(cat "$T/sys/tun1/mtu")" = 1308 ]'
chk '[ "$($TM show | tr "\n" ";")" = "tun0 1342 1302 1282;tun1 1308 1268 1248;" ]'
# MSS 钳制：独立表原子替换（先建再删再定义），进出每条隧道、IPv4 / IPv6 分开
chk 'head -n 3 "$T/nft.last" | tr "\n" ";" | grep -qx "table inet xrouter_tun;delete table inet xrouter_tun;table inet xrouter_tun {;"'
chk 'grep -q "oifname \"tun0\" meta nfproto ipv4 tcp flags & (syn|rst) == syn tcp option maxseg size > 1302 tcp option maxseg size set 1302" "$T/nft.last"'
chk 'grep -q "iifname \"tun1\" meta nfproto ipv6 .* size set 1248" "$T/nft.last"'
chk '[ "$(grep -c "priority mangle" "$T/nft.last")" = 2 ] && ! sed -n "/chain output/,/}/p" "$T/nft.last" | grep -q iifname'
# 内容不变不重复加载
n="$(grep -c "^nft -f" "$T/nft.calls")"; $TM apply; chk '[ "$(grep -c "^nft -f" "$T/nft.calls")" = "$n" ]'
# 2. 上游变化：只改 MTU 不重启；外层未知按 udp6；下限 1280
echo 1500 > "$T/ctl/host/mtu"; $TM proto tun1 bogus; $TM apply
chk '[ "$(cat "$T/sys/tun0/mtu")" = 1432 ] && [ "$(cat "$T/sys/tun1/mtu")" = 1412 ]'
echo 1300 > "$T/ctl/host/mtu"; $TM apply; chk '[ "$(cat "$T/sys/tun0/mtu")" = 1280 ]'
echo 9000 > "$T/ctl/host/mtu"; echo 1400 > "$T/sys/eth0/mtu"; $TM apply; chk '[ "$(cat "$T/sys/tun0/mtu")" = 1432 ]'
rm -f "$T/ctl/host/mtu"; $TM apply; chk '[ "$(cat "$T/sys/tun0/mtu")" = 1332 ]'
# 隧道消失：清掉记录与规则
rm -rf "$T/sys/tun0"; $TM apply; chk '[ ! -f "$T/tun/tun0.proto" ] && ! grep -q tun0 "$T/nft.last"'
mkdir -p "$T/sys/tun0"; echo 1500 > "$T/sys/tun0/mtu"
chk '$TM bad >/dev/null; [ $? -eq 2 ]'

# 3. 隧道上报：每条一行（名称、地址、MTU、路由；分半默认路由不上报），汇总兼容旧版
echo home > "$T/tun/tun0.name"
$VS
chk '[ "$(cat "$T/ctl/guest/openvpn.status")" = up ] && [ "$(cat "$T/ctl/guest/openvpn.addr")" = 10.8.0.5/24 ] && [ "$(cat "$T/ctl/guest/openvpn.if")" = tun0 ]'
chk '[ "$(sed -n 1p "$T/ctl/guest/openvpn.tunnels")" = "tun0|home|10.8.0.5/24|1500|10.8.0.0/24,192.168.31.0/24" ]'
chk '[ "$(sed -n 2p "$T/ctl/guest/openvpn.tunnels")" = "tun1|tun1|fd00:9::6/64|1312|192.168.50.0/24" ]'
chk '[ "$(sort "$T/ctl/guest/openvpn.routes" | tr "\n" " ")" = "10.8.0.0/24 192.168.31.0/24 192.168.50.0/24 " ]'
# 内容没变不重写（避免刷闪存）
touch -d "2001-01-01" "$T/ctl/guest/openvpn.tunnels"; $VS; chk '[ "$(date -r "$T/ctl/guest/openvpn.tunnels" +%Y)" = 2001 ]'
rm -rf "$T/sys/tun0" "$T/sys/tun1"; $VS
chk '[ "$(cat "$T/ctl/guest/openvpn.status")" = down ] && [ ! -s "$T/ctl/guest/openvpn.tunnels" ] && [ ! -s "$T/ctl/guest/openvpn.routes" ]'

# 4. 分来源流量计数表：手机 / 热点 × 公网 / 内网
rm -f "$T/nft.last"; sh "$LX/xrouter-acct" init
chk 'grep -q "^table inet xrouter_acct {" "$T/nft.last" && [ "$(grep -c "^  counter " "$T/nft.last")" = 8 ] && grep -q "hook forward priority -5" "$T/nft.last"'
chk 'grep -q "iifname \"eth1\" oifname \"tun\*\" counter name hs_lan_up return" "$T/nft.last" && grep -q "ip saddr 172.29.233.1 counter name ph_wan_up return" "$T/nft.last"'
chk '! grep -qE "chain (fwd|forward) " "$T/nft.last"'   # fwd 是 nft 关键字
cat > "$T/bin/nft" <<'EOF'
#!/bin/sh
[ "$1" = list ] && [ "$2" = counters ] && printf 'table inet xrouter_acct {\n\tcounter ph_wan_up {\n\t\tpackets 3 bytes 100\n\t}\n\tcounter hs_lan_dn {\n\t\tpackets 9 bytes 4096\n\t}\n}\n'
exit 0
EOF
chk '[ "$(sh "$LX/xrouter-acct" show)" = "100 0 0 0 0 0 0 4096" ]'

# 5. OpenClash 状态上报（xrouter-clashstat）：未安装 / 核心未启动 / 已安装+已启动
export XRG_CLASH_UI="$T/zb"
mkdir -p "$XRG_CLASH_UI"
sh "$LX/xrouter-clashstat"
chk 'grep -qx "installed=0" "$T/ctl/guest/openclash.status" && grep -qx "running=0" "$T/ctl/guest/openclash.status" && grep -qx "port=9090" "$T/ctl/guest/openclash.status"'
echo "<html>zashboard</html>" > "$XRG_CLASH_UI/index.html"
printf '#!/bin/sh\n[ "$1" = clash ] && exit 0; exit 1\n' > "$T/bin/pidof"
printf '#!/bin/sh\n[ "$3" = "openclash.config.cn_port" ] && echo 9092; exit 0\n' > "$T/bin/uci"
chmod +x "$T/bin/pidof" "$T/bin/uci"
sh "$LX/xrouter-clashstat"
chk 'grep -qx "installed=1" "$T/ctl/guest/openclash.status" && grep -qx "running=1" "$T/ctl/guest/openclash.status" && grep -qx "port=9092" "$T/ctl/guest/openclash.status"'
touch -d "2001-01-01" "$T/ctl/guest/openclash.status"
sh "$LX/xrouter-clashstat"
chk '[ "$(date -r "$T/ctl/guest/openclash.status" +%Y)" = 2001 ]'

# 6. OpenClash hvc1 内部通道（xrouter-clashd）：ui 静态读取 + 越权拦截 + 本地 HTTP 代理 + WebSocket
if command -v ruby >/dev/null 2>&1 && ruby -rjson -rsocket -e 'exit 0' 2>/dev/null; then
  ruby - "$LX/xrouter-clashd" "$XRG_CLASH_UI" <<'PYRB' || fail=1
require "json"
require "socket"
require "digest/sha1"

clashd, ui_dir = ARGV
srv = TCPServer.new("127.0.0.1", 0)
port = srv.addr[1]
t_srv = Thread.new do
  loop do
    c = srv.accept
    Thread.new(c) do |s|
      req = +""
      req << s.readpartial(4096) until req.include?("\r\n\r\n")
      hdr, rest = req.split("\r\n\r\n", 2)
      first = hdr.lines.first.to_s
      if first.start_with?("GET /version")
        auth = hdr[/^Authorization:\s*(.+)$/i, 1].to_s.strip
        body = JSON.generate(version: "mihomo-test", auth: auth)
        s.write("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: #{body.bytesize}\r\n\r\n#{body}")
      elsif first.start_with?("GET /ws")
        key = hdr[/^Sec-WebSocket-Key:\s*(.+)$/i, 1].to_s.strip
        acc = [Digest::SHA1.digest("#{key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11")].pack("m0")
        s.write("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: #{acc}\r\n\r\n")
        msg = '{"down":1024}'.b
        s.write([0x81, msg.bytesize].pack("CC") + msg)
        sleep 0.2
        s.write([0x88, 2, 1000].pack("CCn"))
      end
    ensure
      s.close rescue nil
    end
  end
rescue StandardError
  nil
end

env = {
  "XRG_HVC_DEV" => "-",
  "XRG_CLASH_UI" => ui_dir,
  "XRG_CLASH_PORT" => port.to_s,
  "XRG_CLASH_SECRET" => "topsecret",
  "XRG_CLASH_RUNNING" => "1"
}
IO.popen(env, ["ruby", clashd], "r+") do |io|
  io.puts(JSON.generate(id: 1, op: "ping"))
  r1 = JSON.parse(io.gets)
  abort("ping failed: #{r1}") unless r1["ok"] && r1["installed"] && r1["running"] && r1["port"] == port

  io.puts(JSON.generate(id: 2, op: "ui", path: "index.html"))
  r2 = JSON.parse(io.gets)
  abort("ui index failed: #{r2}") unless r2["ok"] && r2["body_b64"].unpack1("m0").include?("zashboard")

  File.binwrite(File.join(ui_dir, "logo.webp"), "RIFFwebp")
  io.puts(JSON.generate(id: 21, op: "ui", path: "logo.webp"))
  r21 = JSON.parse(io.gets)
  abort("ui webp mime failed: #{r21}") unless r21["ok"] && r21["mime"] == "image/webp"

  io.puts(JSON.generate(id: 3, op: "ui", path: "../etc/passwd"))
  r3 = JSON.parse(io.gets)
  abort("ui traversal not blocked: #{r3}") if r3["ok"]

  io.puts(JSON.generate(id: 4, op: "req", method: "GET", path: "/version", headers: { "Authorization" => "Bearer xrouter-internal" }))
  r4 = JSON.parse(io.gets)
  body4 = JSON.parse(r4["body_b64"].unpack1("m0"))
  abort("req proxy failed: #{r4} #{body4}") unless r4["status"] == 200 && body4["auth"] == "Bearer topsecret"

  io.puts(JSON.generate(id: 5, op: "ws_open", ws_id: "w1", path: "/ws?token=xrouter-internal"))
  got = []
  3.times { got << JSON.parse(io.gets) }
  unless got.any? { |g| g["op"] == "ws_open_ok" } && got.any? { |g| g["op"] == "ws_msg" && g["data"].include?("1024") }
    abort("ws failed: #{got}")
  end
end
srv.close
PYRB
fi

# 7. OpenClash / Aurora 首次开机离线安装（--no-network 避免无网超时阻塞 70s）与自启软链检查
AU_DEF="$ROOT/openwrt/files/etc/uci-defaults/25-xrouter-aurora"
OC_DEF="$ROOT/openwrt/files/etc/uci-defaults/30-xrouter-openclash"
OC_RDY="$ROOT/openwrt/files/etc/init.d/xrouter-ready"
chk 'grep -q "apk add --no-network " "$AU_DEF" && grep -q "apk add --no-network " "$OC_DEF"'
chk '! grep -En "^[[:space:]]*apk add " "$ROOT/openwrt/files/etc/uci-defaults/"* | grep -v -- "--no-network"'
chk 'grep -q "/etc/init.d/openclash enable" "$OC_DEF" && ! grep -q "/etc/init.d/openclash disable" "$OC_DEF"'
chk 'grep -q "openclash.config.enable=0" "$OC_DEF"'
chk 'grep -q "/etc/init.d/openclash enabled" "$OC_RDY" && grep -q "/etc/init.d/openclash boot" "$OC_RDY"'

# 8. xrouter-vpn 上报 IPv6 状态：内容不变不替换 guest/ipv6（控制目录在手机闪存上），临时文件写在客机 /tmp
VPN_INIT="$ROOT/openwrt/files/etc/init.d/xrouter-vpn"
mkdir -p "$T/ip6/usr/libexec" "$T/ip6/tmp" "$T/ip6/ctl/guest"
printf '#!/bin/sh\ncat "%s/ip6/state"\n' "$T" > "$T/ip6/usr/libexec/xrouter-ipv6"; chmod +x "$T/ip6/usr/libexec/xrouter-ipv6"
sed -n '/# ipv6-report-begin/,/# ipv6-report-end/p' "$VPN_INIT" | sed -e "s# /tmp/xrouter-ipv6.state# $T/ip6/tmp/xrouter-ipv6.state#g" -e "s#/usr/libexec/xrouter-ipv6 #$T/ip6/usr/libexec/xrouter-ipv6 #g" > "$T/ip6/step.sh"
chk '[ "$(grep -c . "$T/ip6/step.sh")" -ge 3 ] && ! grep -q "> \"\${CTL}/guest/ipv6.tmp\"" "$VPN_INIT"'
echo "1" > "$T/ip6/state"
CTL="$T/ip6/ctl" sh "$T/ip6/step.sh"
chk '[ "$(cat "$T/ip6/ctl/guest/ipv6")" = 1 ] && [ ! -e "$T/ip6/ctl/guest/ipv6.tmp" ]'
touch -t 200001010000 "$T/ip6/ctl/guest/ipv6"
CTL="$T/ip6/ctl" sh "$T/ip6/step.sh"
chk '[ -n "$(find "$T/ip6/ctl/guest/ipv6" -mtime +1000)" ]'   # 第二轮内容相同：不替换
echo "0" > "$T/ip6/state"
CTL="$T/ip6/ctl" sh "$T/ip6/step.sh"
chk '[ "$(cat "$T/ip6/ctl/guest/ipv6")" = 0 ] && [ -z "$(find "$T/ip6/ctl/guest/ipv6" -mtime +1000)" ]'

# 9. xrouter-diag：nft 规则用 terse（不含 set 元素）并单列各 set 元素数；新增「监听端口」段（需求 4c）
mkdir -p "$T/diagbin"
cat > "$T/diagbin/nft" <<'EON'
#!/bin/sh
case "$*" in
  "-t list ruleset") printf 'table inet fw4 {\n\tset oc_v4 {\n\t\ttype ipv4_addr\n\t}\n\tchain forward {\n\t\tjump xr_fwd\n\t}\n}\n' ;;
  "-t list sets") printf 'table inet fw4 {\n\tset oc_v4 {\n\t\ttype ipv4_addr\n\t}\n}\n' ;;
  "list set inet fw4 oc_v4") printf 'table inet fw4 {\n\tset oc_v4 {\n\t\ttype ipv4_addr\n\t\telements = { 1.1.1.1, 2.2.2.0/24,\n\t\t\t     3.3.3.3 }\n\t}\n}\n' ;;
  *) exit 1 ;;
esac
EON
cat > "$T/diagbin/netstat" <<'EON'
#!/bin/sh
printf '%s\n' "Active Internet connections" "Proto Recv-Q Send-Q Local Foreign State" "tcp 0 0 172.29.233.254:22 0.0.0.0:* LISTEN" "tcp 0 0 :::80 :::* LISTEN"
EON
chmod +x "$T/diagbin/"*
PATH="$T/diagbin:$PATH" sh "$LX/xrouter-diag" > "$T/diag.out" 2>&1
chk 'grep -q "^## nft 规则（terse" "$T/diag.out" && grep -q "jump xr_fwd" "$T/diag.out" && ! grep -q "elements" "$T/diag.out"'
chk 'grep -qx "inet fw4 oc_v4 3" "$T/diag.out"'
chk 'grep -qx "## 监听端口" "$T/diag.out" && grep -qx "172.29.233.254:22" "$T/diag.out" && grep -qx ":::80" "$T/diag.out" && grep -q "无 tentative/dadfailed 地址" "$T/diag.out"'
chk '! grep -q "## nft（前 400 行）" "$T/diag.out"'

# 10. xrouter-stats：CPU 两次采样差值、内存占用、邻居表去重计数（FAILED 不计），第二次调用不 sleep；watch 循环按 host/stats.cmd 应答（需求 8）
P="$T/stproc"; mkdir -p "$P/sys/net/netfilter" "$T/stbin" "$T/sttmp"
echo "cpu  100 0 100 700 100 0 0 0 0 0" > "$P/stat"
printf 'MemTotal:       1000 kB\nMemFree:         100 kB\nMemAvailable:    570 kB\n' > "$P/meminfo"
echo "0.15 0.10 0.05 1/80 123" > "$P/loadavg"
echo "19967.12 30000.00" > "$P/uptime"
echo 123 > "$P/sys/net/netfilter/nf_conntrack_count"
cat > "$T/stbin/ip" <<'EOS'
#!/bin/sh
case "$*" in
  "-4 neigh show dev eth1") printf '%s\n' "192.168.9.2 lladdr aa:bb:cc:dd:ee:01 REACHABLE" "192.168.9.3 lladdr aa:bb:cc:dd:ee:02 FAILED" ;;
  "-6 neigh show dev eth1") echo "fe80::1 lladdr aa:bb:cc:dd:ee:01 STALE" ;;
esac
EOS
printf '#!/bin/sh\necho "sleep $*" >> "%s/sleep.calls"\n' "$T" > "$T/stbin/sleep"
chmod +x "$T/stbin/"*
ST="sh $LX/xrouter-stats"
XRG_PROC="$P" XRG_TMP="$T/sttmp" PATH="$T/stbin:$PATH" $ST > "$T/st1.out"
chk '[ "$(cat "$T/sleep.calls")" = "sleep 1" ] && ! grep -q "^cpu_pct=" "$T/st1.out"'
echo "cpu  200 0 150 1000 100 0 0 0 0 0" > "$P/stat"
XRG_PROC="$P" XRG_TMP="$T/sttmp" PATH="$T/stbin:$PATH" $ST > "$T/st2.out"
chk '[ "$(grep -c . "$T/sleep.calls")" = 1 ]'   # 第二次调用有上次采样：不 sleep
chk '[ "$(cat "$T/st2.out")" = "$(printf "cpu_pct=33\nmem_pct=43\nload1=0.15\nuptime=19967\nconntrack=123\nhs=off\ndev_hotspot=1")" ]'
mkdir -p "$T/stw/ctl/host" "$T/stw/ctl/guest"
printf '#!/bin/sh\necho cpu_pct=5\n' > "$T/stw/xrouter-stats"; chmod +x "$T/stw/xrouter-stats"
sed -n '/# stats-cmd-begin/,/# stats-cmd-end/p' "$VPN_INIT" | sed -e "s#/usr/libexec/xrouter-stats#$T/stw/xrouter-stats#g" -e "s#/tmp/xrouter-stats.out#$T/stw/out#g" > "$T/stw/step.sh"
chk '[ "$(grep -c . "$T/stw/step.sh")" -ge 6 ]'
printf 'laststats=""\n. "%s/stw/step.sh"\n' "$T" > "$T/stw/round.sh"
CTL="$T/stw/ctl" sh "$T/stw/round.sh"
chk '[ ! -e "$T/stw/ctl/guest/stats" ]'   # 无请求不写
echo 111 > "$T/stw/ctl/host/stats.cmd"
printf 'laststats=""\n. "%s/stw/step.sh"\ntouch -t 200001010000 "$CTL/guest/stats"\n. "%s/stw/step.sh"\n' "$T" "$T" > "$T/stw/round.sh"
CTL="$T/stw/ctl" sh "$T/stw/round.sh"
chk '[ "$(head -n 1 "$T/stw/ctl/guest/stats")" = ts=111 ] && grep -qx cpu_pct=5 "$T/stw/ctl/guest/stats" && [ ! -e "$T/stw/ctl/guest/stats.tmp" ]'
chk '[ -n "$(find "$T/stw/ctl/guest/stats" -mtime +1000)" ]'   # 同一请求第二轮不重写
chk 'grep -q "echo 1 > \"\${CTL}/guest/stats.cap\"" "$VPN_INIT"'

if [ "$fail" = 0 ]; then echo "test-guest OK"; else echo "test-guest FAILED"; exit 1; fi
