#!/system/bin/sh
# ==============================================================================
# x-route: Network Orchestrator & Event-Driven Netlink Watcher
# Features:
#   1. Virtual Switch & TAP Setup (tap-wan, tap-lan, br-lan)
#   2. Dynamic 5G/4G Cellular WAN Detection & NAT (rmnet_dataX drift resilient)
#   3. Zero-Touch Wi-Fi Hotspot Hijack (tc eBPF removal + NetworkStack DHCP block)
#   4. Loop-Free Host Local Traffic & DNS Redirection to OpenWrt (192.168.100.1)
# ==============================================================================

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
MODDIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
# shellcheck disable=SC1091
. "${SCRIPT_DIR}/common.sh"

NET_WATCH_PID_FILE="${RUN_DIR}/net_watch.pid"
STATE_CELL_IF="${RUN_DIR}/active_cell_if"
STATE_AP_IF="${RUN_DIR}/active_ap_if"
STATE_HOST_REDIRECT="${RUN_DIR}/host_redirect_active"
ORIG_PRIVATE_DNS_FILE="${RUN_DIR}/orig_private_dns"

# --- Helper: Detect Active Cellular Data Interface (rmnet_dataX / ccmniX) ---
detect_cellular_iface() {
    local cell_if=""
    # 1. Check Android per-interface routing tables for active default route
    cell_if="$(ip -4 route show table all 2>/dev/null | awk '/^default / && /dev (rmnet_data[0-9]+|r_rmnet_data[0-9]+|ccmni[0-9]+)/ {for(i=1;i<=NF;i++) if($i=="dev") {print $(i+1); exit}}')"
    if [ -n "${cell_if}" ]; then
        echo "${cell_if}"
        return 0
    fi
    # 2. Fallback: find any UP rmnet_data* interface with a global IPv4 address
    cell_if="$(ip -4 -o addr show scope global 2>/dev/null | awk '$2 ~ /^(rmnet_data|r_rmnet_data|ccmni)/ {print $2; exit}')"
    echo "${cell_if}"
}

# --- Helper: Detect Active Wi-Fi Hotspot AP Interface (ap0 / wlan1 / wlan2) ---
detect_hotspot_iface() {
    local ap_if=""
    # 1. Preferred: Query nl80211 via 'iw dev' for interface in 'type AP' mode
    if command -v iw >/dev/null 2>&1; then
        ap_if="$(iw dev 2>/dev/null | awk '$1=="Interface"{iface=$2} $1=="type" && $2=="AP"{print iface; exit}')"
        if [ -n "${ap_if}" ] && ip link show "${ap_if}" 2>/dev/null | grep -q "UP"; then
            echo "${ap_if}"
            return 0
        fi
    fi

    # 2. Fallback: Check hostapd active control sockets or interface names
    for candidate in ap0 wlan1 wlan2 swlan0 softap0; do
        if ip link show "${candidate}" 2>/dev/null | grep -q "state UP"; then
            # Ensure it is not the primary Wi-Fi STA interface carrying default route
            if ! ip -4 route show table all 2>/dev/null | grep -q "default.*dev ${candidate}"; then
                echo "${candidate}"
                return 0
            fi
        fi
    done
    echo ""
}

# --- 1. Initialize Virtual Interfaces & Bridge (tap-wan, tap-lan, br-lan) ---
init_taps() {
    load_config
    ensure_dirs

    # Kernel sysctl tuning for routing & L2 bridge isolation from Android netd
    sysctl -w net.ipv4.ip_forward=1 >/dev/null 2>&1 || echo 1 > /proc/sys/net/ipv4/ip_forward
    sysctl -w net.ipv6.conf.all.forwarding=1 >/dev/null 2>&1 || true
    sysctl -w net.ipv4.conf.all.rp_filter=0 >/dev/null 2>&1 || true
    sysctl -w net.ipv4.conf.default.rp_filter=0 >/dev/null 2>&1 || true
    sysctl -w net.bridge.bridge-nf-call-iptables=0 >/dev/null 2>&1 || true
    sysctl -w net.bridge.bridge-nf-call-ip6tables=0 >/dev/null 2>&1 || true

    # Create tap-wan (172.16.1.1/24)
    if ! ip link show "${WAN_TAP}" >/dev/null 2>&1; then
        ip tuntap add mode tap name "${WAN_TAP}"
    fi
    ip link set dev "${WAN_TAP}" up
    if ! ip -4 addr show dev "${WAN_TAP}" | grep -q "${WAN_HOST_IP}/24"; then
        ip -4 addr flush dev "${WAN_TAP}" 2>/dev/null || true
        ip -4 addr add "${WAN_HOST_IP}/24" dev "${WAN_TAP}"
    fi
    sysctl -w "net.ipv4.conf.${WAN_TAP}.rp_filter=0" >/dev/null 2>&1 || true

    # Create tap-lan
    if ! ip link show "${LAN_TAP}" >/dev/null 2>&1; then
        ip tuntap add mode tap name "${LAN_TAP}"
    fi
    ip link set dev "${LAN_TAP}" up

    # Create br-lan (192.168.100.2/24) and enslave tap-lan
    if ! ip link show "${LAN_BR}" >/dev/null 2>&1; then
        ip link add name "${LAN_BR}" type bridge 2>/dev/null || brctl addbr "${LAN_BR}" 2>/dev/null
    fi
    ip link set dev "${LAN_TAP}" master "${LAN_BR}" 2>/dev/null || brctl addif "${LAN_BR}" "${LAN_TAP}" 2>/dev/null || true
    ip link set dev "${LAN_BR}" up
    if ! ip -4 addr show dev "${LAN_BR}" | grep -q "${LAN_HOST_IP}/24"; then
        ip -4 addr flush dev "${LAN_BR}" 2>/dev/null || true
        ip -4 addr add "${LAN_HOST_IP}/24" dev "${LAN_BR}"
    fi
    sysctl -w "net.ipv4.conf.${LAN_BR}.rp_filter=0" >/dev/null 2>&1 || true

    # Ensure host can always reach WAN_SUBNET (172.16.1.0/24) and LAN_SUBNET (192.168.100.0/24) via main table
    ip rule del pref 4800 2>/dev/null || true
    ip rule del pref 4801 2>/dev/null || true
    ip rule add pref 4800 to "${WAN_SUBNET}.0/24" lookup main
    ip rule add pref 4801 to "${LAN_SUBNET}.0/24" lookup main

    log_info "net_orchestrator" "Virtual interfaces ready (${WAN_TAP}=${WAN_HOST_IP}/24, ${LAN_BR}=${LAN_HOST_IP}/24)"
}

# --- 2. Setup Custom iptables Chains & Sync Cellular WAN NAT ---
setup_base_firewall_chains() {
    # NAT Postrouting chain
    iptables -t nat -N XROUTE_NAT 2>/dev/null || iptables -t nat -F XROUTE_NAT
    iptables -t nat -C POSTROUTING -j XROUTE_NAT 2>/dev/null || iptables -t nat -I POSTROUTING 1 -j XROUTE_NAT

    # Filter Forward chain (inserted at top of FORWARD to bypass Android tetherctrl_FORWARD drop)
    iptables -t filter -N XROUTE_FWD 2>/dev/null || iptables -t filter -F XROUTE_FWD
    iptables -t filter -C FORWARD -j XROUTE_FWD 2>/dev/null || iptables -t filter -I FORWARD 1 -j XROUTE_FWD

    # Filter Input/Output chains (for blocking Android NetworkStack DHCP on bridged AP interface)
    iptables -t filter -N XROUTE_IN 2>/dev/null || iptables -t filter -F XROUTE_IN
    iptables -t filter -C INPUT -j XROUTE_IN 2>/dev/null || iptables -t filter -I INPUT 1 -j XROUTE_IN

    iptables -t filter -N XROUTE_OUT 2>/dev/null || iptables -t filter -F XROUTE_OUT
    iptables -t filter -C OUTPUT -j XROUTE_OUT 2>/dev/null || iptables -t filter -I OUTPUT 1 -j XROUTE_OUT

    # Always permit L2/L3 forwarding across br-lan and tap-lan
    iptables -A XROUTE_FWD -i "${LAN_BR}" -o "${LAN_BR}" -j ACCEPT
    iptables -A XROUTE_FWD -i "${LAN_TAP}" -j ACCEPT
    iptables -A XROUTE_FWD -o "${LAN_TAP}" -j ACCEPT
}

sync_wan_nat() {
    load_config
    setup_base_firewall_chains

    local cell_if
    cell_if="$(detect_cellular_iface)"
    echo "${cell_if}" > "${STATE_CELL_IF}"

    # Clean previous WAN routing rule
    ip rule del pref 4810 2>/dev/null || true

    if [ -z "${cell_if}" ]; then
        log_warn "net_orchestrator" "No active cellular interface (rmnet_dataX) detected yet."
        return 0
    fi

    # Find Android's routing table for cell_if so packets from 172.16.1.0/24 route out cell_if
    local cell_table=""
    if [ -f /data/misc/net/rt_tables ]; then
        cell_table="$(awk -v iface="${cell_if}" '$2 == iface {print $1; exit}' /data/misc/net/rt_tables 2>/dev/null)"
    fi
    if [ -z "${cell_table}" ]; then
        cell_table="$(ip -4 route show table all 2>/dev/null | awk -v iface="${cell_if}" '$1=="default" && $0 ~ ("dev " iface) {for(i=1;i<=NF;i++) if($i=="table") {print $(i+1); exit}}')"
    fi

    if [ -n "${cell_table}" ]; then
        ip rule add pref 4810 from "${WAN_SUBNET}.0/24" lookup "${cell_table}"
    fi

    # Populate XROUTE_NAT and XROUTE_FWD for cell_if
    iptables -t nat -F XROUTE_NAT
    iptables -t nat -A XROUTE_NAT -s "${WAN_SUBNET}.0/24" -o "${cell_if}" -j MASQUERADE

    iptables -A XROUTE_FWD -i "${WAN_TAP}" -o "${cell_if}" -j ACCEPT
    iptables -A XROUTE_FWD -i "${cell_if}" -o "${WAN_TAP}" -m conntrack --ctstate RELATED,ESTABLISHED -j ACCEPT 2>/dev/null || \
        iptables -A XROUTE_FWD -i "${cell_if}" -o "${WAN_TAP}" -m state --state RELATED,ESTABLISHED -j ACCEPT

    log_info "net_orchestrator" "Synced WAN NAT to cellular interface ${cell_if} (table=${cell_table:-main})"
}

# --- 3. Dynamic Wi-Fi Hotspot Takeover (ap0 / wlan1 -> br-lan) ---
sync_hotspot_bridge() {
    load_config
    if [ "${HOTSPOT_BRIDGE_ENABLED:-1}" != "1" ]; then
        rm -f "${STATE_AP_IF}"
        return 0
    fi

    local ap_if
    ap_if="$(detect_hotspot_iface)"
    echo "${ap_if}" > "${STATE_AP_IF}"

    iptables -t filter -F XROUTE_IN 2>/dev/null || true
    iptables -t filter -F XROUTE_OUT 2>/dev/null || true

    if [ -z "${ap_if}" ]; then
        return 0
    fi

    # 1. Remove Android Tethering eBPF offload hooks from AP interface so frames enter br-lan
    tc qdisc del dev "${ap_if}" clsact 2>/dev/null || true
    tc qdisc del dev "${ap_if}" ingress 2>/dev/null || true

    # 2. Block Android host's internal NetworkStack DhcpServer (UDP 67/68) on AP & br-lan
    #    Note: Bridged packets between ap_if and tap-lan traverse FORWARD, NOT INPUT/OUTPUT,
    #    so OpenWrt's dnsmasq receives all DHCP requests while Android's host DhcpServer is muted!
    iptables -A XROUTE_IN -i "${ap_if}" -p udp --dport 67:68 -j DROP
    iptables -A XROUTE_IN -i "${LAN_BR}" -p udp --dport 67:68 -j DROP
    iptables -A XROUTE_OUT -o "${ap_if}" -p udp --sport 67:68 -j DROP
    iptables -A XROUTE_OUT -o "${LAN_BR}" -p udp --sport 67:68 -j DROP

    # Block Android host IPv6 Router Advertisements on Hotspot so OpenWrt manages IPv6/RA
    ip6tables -D INPUT -i "${LAN_BR}" -p icmpv6 --icmpv6-type 133 -j DROP 2>/dev/null || true
    ip6tables -I INPUT 1 -i "${LAN_BR}" -p icmpv6 --icmpv6-type 133 -j DROP 2>/dev/null || true
    ip6tables -D OUTPUT -o "${LAN_BR}" -p icmpv6 --icmpv6-type 134 -j DROP 2>/dev/null || true
    ip6tables -I OUTPUT 1 -o "${LAN_BR}" -p icmpv6 --icmpv6-type 134 -j DROP 2>/dev/null || true

    # 3. Flush Android Tethering random IP from ap_if and enslave ap_if to br-lan
    ip -4 addr flush dev "${ap_if}" 2>/dev/null || true
    ip link set dev "${ap_if}" master "${LAN_BR}" 2>/dev/null || brctl addif "${LAN_BR}" "${ap_if}" 2>/dev/null || true
    ip link set dev "${ap_if}" up
    ip link set dev "${LAN_BR}" up

    # 4. Ensure FORWARD allows ap_if <-> tap-lan
    iptables -C XROUTE_FWD -i "${ap_if}" -j ACCEPT 2>/dev/null || iptables -A XROUTE_FWD -i "${ap_if}" -j ACCEPT
    iptables -C XROUTE_FWD -o "${ap_if}" -j ACCEPT 2>/dev/null || iptables -A XROUTE_FWD -o "${ap_if}" -j ACCEPT

    log_info "net_orchestrator" "Bridged Wi-Fi Hotspot interface '${ap_if}' into ${LAN_BR} (Gateway=${LAN_GW_IP})"
}

# --- 4. Host Local App Traffic & DNS Redirection to OpenWrt (192.168.100.1) ---
enable_host_redirect() {
    load_config
    # Verify OpenWrt LAN Gateway (192.168.100.1) is reachable before redirecting host traffic!
    if ! ping -c 1 -W 1 "${LAN_GW_IP}" >/dev/null 2>&1; then
        log_warn "net_orchestrator" "OpenWrt gateway ${LAN_GW_IP} not reachable yet; deferring host redirect."
        echo "pending" > "${STATE_HOST_REDIRECT}"
        return 0
    fi

    # 1. Disable Android Private DNS (DoT) so host DNS uses standard port 53 -> OpenWrt
    if [ ! -f "${ORIG_PRIVATE_DNS_FILE}" ]; then
        settings get global private_dns_mode > "${ORIG_PRIVATE_DNS_FILE}" 2>/dev/null || echo "opportunistic" > "${ORIG_PRIVATE_DNS_FILE}"
    fi
    settings put global private_dns_mode off 2>/dev/null || true

    # 2. Populate Routing Table 100
    ip route flush table "${ROUTE_TABLE_REDIRECT}" 2>/dev/null || true
    ip route replace "${LAN_SUBNET}.0/24" dev "${LAN_BR}" src "${LAN_HOST_IP}" table "${ROUTE_TABLE_REDIRECT}"
    ip route replace "${WAN_SUBNET}.0/24" dev "${WAN_TAP}" src "${WAN_HOST_IP}" table "${ROUTE_TABLE_REDIRECT}"
    ip route replace default via "${LAN_GW_IP}" dev "${LAN_BR}" src "${LAN_HOST_IP}" table "${ROUTE_TABLE_REDIRECT}"

    # 3. Mark Exempted Host Traffic in mangle OUTPUT (prevent routing loops)
    iptables -t mangle -N XROUTE_MANGLE_OUT 2>/dev/null || iptables -t mangle -F XROUTE_MANGLE_OUT
    iptables -t mangle -C OUTPUT -j XROUTE_MANGLE_OUT 2>/dev/null || iptables -t mangle -I OUTPUT 1 -j XROUTE_MANGLE_OUT

    iptables -t mangle -A XROUTE_MANGLE_OUT -d 127.0.0.0/8 -j MARK --set-xmark "${FWMARK_EXEMPT}"
    iptables -t mangle -A XROUTE_MANGLE_OUT -d "${WAN_SUBNET}.0/24" -j MARK --set-xmark "${FWMARK_EXEMPT}"
    iptables -t mangle -A XROUTE_MANGLE_OUT -d "${LAN_SUBNET}.0/24" -j MARK --set-xmark "${FWMARK_EXEMPT}"
    iptables -t mangle -A XROUTE_MANGLE_OUT -s "${WAN_HOST_IP}" -j MARK --set-xmark "${FWMARK_EXEMPT}"

    # 4. Configure Policy Routing Rules (pref 4900-5100)
    ip rule del pref 4900 2>/dev/null || true
    ip rule del pref 5000 2>/dev/null || true
    ip rule del pref 5100 2>/dev/null || true

    ip rule add pref 4900 table main suppress_prefixlength 0
    ip rule add pref 5000 fwmark "${FWMARK_EXEMPT}" lookup main
    ip rule add pref 5100 lookup "${ROUTE_TABLE_REDIRECT}"

    # 5. Redirect Host DNS (UDP/TCP 53) to OpenWrt (192.168.100.1:53)
    iptables -t nat -N XROUTE_DNS_OUT 2>/dev/null || iptables -t nat -F XROUTE_DNS_OUT
    iptables -t nat -C OUTPUT -j XROUTE_DNS_OUT 2>/dev/null || iptables -t nat -I OUTPUT 1 -j XROUTE_DNS_OUT

    iptables -t nat -A XROUTE_DNS_OUT -m mark --mark "${FWMARK_EXEMPT}" -j RETURN
    iptables -t nat -A XROUTE_DNS_OUT -s "${LAN_GW_IP}" -j RETURN
    iptables -t nat -A XROUTE_DNS_OUT -p udp --dport 53 -j DNAT --to-destination "${LAN_GW_IP}:53"
    iptables -t nat -A XROUTE_DNS_OUT -p tcp --dport 53 -j DNAT --to-destination "${LAN_GW_IP}:53"

    echo "active" > "${STATE_HOST_REDIRECT}"
    log_info "net_orchestrator" "Host local traffic & DNS redirection enabled (-> ${LAN_GW_IP})"
}

disable_host_redirect() {
    ip rule del pref 4900 2>/dev/null || true
    ip rule del pref 5000 2>/dev/null || true
    ip rule del pref 5100 2>/dev/null || true
    ip route flush table "${ROUTE_TABLE_REDIRECT}" 2>/dev/null || true

    iptables -t mangle -D OUTPUT -j XROUTE_MANGLE_OUT 2>/dev/null || true
    iptables -t mangle -F XROUTE_MANGLE_OUT 2>/dev/null || true
    iptables -t mangle -X XROUTE_MANGLE_OUT 2>/dev/null || true

    iptables -t nat -D OUTPUT -j XROUTE_DNS_OUT 2>/dev/null || true
    iptables -t nat -F XROUTE_DNS_OUT 2>/dev/null || true
    iptables -t nat -X XROUTE_DNS_OUT 2>/dev/null || true

    if [ -f "${ORIG_PRIVATE_DNS_FILE}" ]; then
        local orig_dns
        orig_dns="$(cat "${ORIG_PRIVATE_DNS_FILE}" 2>/dev/null)"
        if [ -n "${orig_dns}" ] && [ "${orig_dns}" != "null" ]; then
            settings put global private_dns_mode "${orig_dns}" 2>/dev/null || true
        fi
        rm -f "${ORIG_PRIVATE_DNS_FILE}"
    fi

    echo "inactive" > "${STATE_HOST_REDIRECT}"
    log_info "net_orchestrator" "Host local traffic redirection disabled."
}

# --- 5. Full Sync & Event-Driven Netlink Watcher ---
sync_all() {
    load_config
    init_taps
    sync_wan_nat
    sync_hotspot_bridge
    if [ "${HOST_REDIRECT_ENABLED:-1}" = "1" ]; then
        enable_host_redirect
    else
        disable_host_redirect
    fi
}

watch_loop() {
    log_info "net_orchestrator" "Starting event-driven network & hotspot watcher..."
    local last_cell=""
    local last_ap=""

    while true; do
        load_config
        local cur_cell cur_ap
        cur_cell="$(detect_cellular_iface)"
        cur_ap="$(detect_hotspot_iface)"

        if [ "${cur_cell}" != "${last_cell}" ]; then
            log_info "net_orchestrator" "Cellular interface change detected: '${last_cell}' -> '${cur_cell}'"
            sync_wan_nat
            last_cell="${cur_cell}"
        fi

        if [ "${cur_ap}" != "${last_ap}" ]; then
            log_info "net_orchestrator" "Hotspot AP interface change detected: '${last_ap}' -> '${cur_ap}'"
            sync_hotspot_bridge
            last_ap="${cur_ap}"
        elif [ -n "${cur_ap}" ] && [ "${HOTSPOT_BRIDGE_ENABLED:-1}" = "1" ]; then
            # Verify AP interface is still enslaved to br-lan (in case TetheringService reset it)
            if [ ! -e "/sys/class/net/${cur_ap}/brport" ]; then
                sync_hotspot_bridge
            fi
        fi

        # Auto-activate pending host redirect once OpenWrt gateway (192.168.100.1) comes online
        if [ "${HOST_REDIRECT_ENABLED:-1}" = "1" ]; then
            if [ "$(cat "${STATE_HOST_REDIRECT}" 2>/dev/null)" != "active" ]; then
                if ping -c 1 -W 1 "${LAN_GW_IP}" >/dev/null 2>&1; then
                    enable_host_redirect
                fi
            elif ! ping -c 1 -W 2 "${LAN_GW_IP}" >/dev/null 2>&1; then
                log_warn "net_orchestrator" "OpenWrt gateway ${LAN_GW_IP} unreachable; suspending host redirect to prevent blackhole."
                disable_host_redirect
                echo "pending" > "${STATE_HOST_REDIRECT}"
            fi
        fi

        # Wait up to 3 seconds or break early if ip monitor detects a link/addr/route event
        timeout 3 ip monitor link addr route >/dev/null 2>&1 || sleep 3
    done
}

start_orchestrator() {
    sync_all
    if is_pid_running "${NET_WATCH_PID_FILE}"; then
        return 0
    fi
    nohup "$0" watch >> "${LOG_DIR}/x-route.log" 2>&1 &
    echo "$!" > "${NET_WATCH_PID_FILE}"
}

stop_orchestrator() {
    if is_pid_running "${NET_WATCH_PID_FILE}"; then
        kill -TERM "$(cat "${NET_WATCH_PID_FILE}")" 2>/dev/null || true
        rm -f "${NET_WATCH_PID_FILE}"
    fi

    disable_host_redirect

    ip rule del pref 4800 2>/dev/null || true
    ip rule del pref 4801 2>/dev/null || true
    ip rule del pref 4810 2>/dev/null || true

    iptables -t nat -D POSTROUTING -j XROUTE_NAT 2>/dev/null || true
    iptables -t nat -F XROUTE_NAT 2>/dev/null || true
    iptables -t nat -X XROUTE_NAT 2>/dev/null || true

    iptables -t filter -D FORWARD -j XROUTE_FWD 2>/dev/null || true
    iptables -t filter -F XROUTE_FWD 2>/dev/null || true
    iptables -t filter -X XROUTE_FWD 2>/dev/null || true

    iptables -t filter -D INPUT -j XROUTE_IN 2>/dev/null || true
    iptables -t filter -F XROUTE_IN 2>/dev/null || true
    iptables -t filter -X XROUTE_IN 2>/dev/null || true

    iptables -t filter -D OUTPUT -j XROUTE_OUT 2>/dev/null || true
    iptables -t filter -F XROUTE_OUT 2>/dev/null || true
    iptables -t filter -X XROUTE_OUT 2>/dev/null || true

    ip6tables -D INPUT -i "${LAN_BR}" -p icmpv6 --icmpv6-type 133 -j DROP 2>/dev/null || true
    ip6tables -D OUTPUT -o "${LAN_BR}" -p icmpv6 --icmpv6-type 134 -j DROP 2>/dev/null || true

    # Detach hotspot interface from br-lan if present
    local ap_if
    ap_if="$(cat "${STATE_AP_IF}" 2>/dev/null)"
    if [ -n "${ap_if}" ]; then
        ip link set dev "${ap_if}" nomaster 2>/dev/null || true
    fi

    rm -f "${STATE_CELL_IF}" "${STATE_AP_IF}" "${STATE_HOST_REDIRECT}"
    log_info "net_orchestrator" "Network orchestrator stopped and host rules restored."
}

status_orchestrator() {
    load_config
    local cell_if ap_if redirect_state gw_online="false"
    cell_if="$(cat "${STATE_CELL_IF}" 2>/dev/null)"
    [ -z "${cell_if}" ] && cell_if="$(detect_cellular_iface)"
    ap_if="$(cat "${STATE_AP_IF}" 2>/dev/null)"
    [ -z "${ap_if}" ] && ap_if="$(detect_hotspot_iface)"
    redirect_state="$(cat "${STATE_HOST_REDIRECT}" 2>/dev/null || echo "inactive")"

    if ping -c 1 -W 1 "${LAN_GW_IP}" >/dev/null 2>&1; then
        gw_online="true"
    fi

    local clients_count=0
    if [ -f /proc/net/arp ]; then
        clients_count="$(awk -v sub="${LAN_SUBNET}." '$1 ~ ("^" sub) && $1 != "192.168.100.1" && $1 != "192.168.100.2" && $4 != "00:00:00:00:00:00" {c++} END{print c+0}' /proc/net/arp 2>/dev/null)"
    fi

    printf '{"cell_if":"%s","ap_if":"%s","host_redirect":"%s","gw_online":%s,"lan_gw":"%s","lan_clients":%s}\n' \
        "${cell_if}" "${ap_if}" "${redirect_state}" "${gw_online}" "${LAN_GW_IP}" "${clients_count}"
}

case "${1:-}" in
    init-taps)             init_taps ;;
    start)                 start_orchestrator ;;
    stop)                  stop_orchestrator ;;
    reload|sync)           sync_all ;;
    sync-wan)              sync_wan_nat ;;
    sync-hotspot)          sync_hotspot_bridge ;;
    enable-host-redirect)  save_config_kv "HOST_REDIRECT_ENABLED" "1"; enable_host_redirect ;;
    disable-host-redirect) save_config_kv "HOST_REDIRECT_ENABLED" "0"; disable_host_redirect ;;
    watch)                 watch_loop ;;
    status)                status_orchestrator ;;
    *)
        echo "Usage: $0 {init-taps|start|stop|reload|sync-wan|sync-hotspot|enable-host-redirect|disable-host-redirect|status}" >&2
        exit 1
        ;;
esac
