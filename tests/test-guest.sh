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

# 7. OpenClash 预置自启软链与旧盘自愈检查
OC_DEF="$ROOT/openwrt/files/etc/uci-defaults/30-xrouter-openclash"
OC_RDY="$ROOT/openwrt/files/etc/init.d/xrouter-ready"
chk 'grep -q "/etc/init.d/openclash enable" "$OC_DEF" && ! grep -q "/etc/init.d/openclash disable" "$OC_DEF"'
chk 'grep -q "openclash.config.enable=0" "$OC_DEF"'
chk 'grep -q "/etc/init.d/openclash enabled" "$OC_RDY" && grep -q "/etc/init.d/openclash boot" "$OC_RDY"'

if [ "$fail" = 0 ]; then echo "test-guest OK"; else echo "test-guest FAILED"; exit 1; fi
