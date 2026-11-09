#!/system/bin/sh
# ==============================================================================
# x-route: SuKiSU Action Button & CLI Controller (action.sh)
# Clicking "Action" in SuKiSU Manager toggles the OpenWrt Soft Router on/off
# and prints a live status summary. Also supports CLI subcommands for WebUI.
# ==============================================================================

MODDIR="${0%/*}"
export MODDIR
# shellcheck disable=SC1091
. "${MODDIR}/scripts/common.sh"

load_config

start_all() {
    echo "[*] 正在初始化虚拟网卡与网桥 (tap-wan / br-lan)..."
    "${MODDIR}/scripts/net_orchestrator.sh" init-taps
    echo "[*] 正在启动 OpenWrt 虚拟机..."
    "${MODDIR}/scripts/vm_manager.sh" start
    echo "[*] 正在启动网络编排与热点监听守护进程..."
    "${MODDIR}/scripts/net_orchestrator.sh" start
    if [ "${KEEPALIVE_ENABLED:-1}" = "1" ]; then
        echo "[*] 正在确保四重内核级应用保活运行..."
        "${MODDIR}/scripts/keepalive_daemon.sh" start
    fi
    echo "[+] x-route 软路由已启动！管理后台: http://${LAN_GW_IP:-192.168.100.1}"
}

stop_all() {
    echo "[*] 正在卸载策略路由并恢复原生网络..."
    "${MODDIR}/scripts/net_orchestrator.sh" stop
    echo "[*] 正在停止 OpenWrt 虚拟机..."
    "${MODDIR}/scripts/vm_manager.sh" stop
    echo "[+] x-route 软路由已安全停止，手机已恢复原生网络状态。"
}

status_json() {
    local vm_json net_json ka_json
    vm_json="$("${MODDIR}/scripts/vm_manager.sh" status 2>/dev/null || echo '{"running":false}')"
    net_json="$("${MODDIR}/scripts/net_orchestrator.sh" status 2>/dev/null || echo '{}')"
    ka_json="$("${MODDIR}/scripts/keepalive_daemon.sh" status 2>/dev/null || echo '{}')"
    printf '{"vm":%s,"net":%s,"keepalive":%s}\n' "${vm_json}" "${net_json}" "${ka_json}"
}

case "${1:-toggle}" in
    start)
        start_all
        ;;
    stop)
        stop_all
        ;;
    restart)
        stop_all
        sleep 1
        start_all
        ;;
    status-json)
        status_json
        ;;
    toggle|*)
        echo "===================================================="
        echo "  x-route 一键控制 (OnePlus 13 SuKiSU Action)"
        echo "===================================================="
        if is_pid_running "${RUN_DIR}/vm.pid"; then
            echo "[!] 检测到 OpenWrt 软路由当前正在运行，即将执行【安全停止】..."
            stop_all
        else
            echo "[!] 检测到 OpenWrt 软路由当前未运行，即将执行【一键启动】..."
            start_all
        fi
        echo "----------------------------------------------------"
        echo "当前状态摘要:"
        status_json
        echo "===================================================="
        ;;
esac
