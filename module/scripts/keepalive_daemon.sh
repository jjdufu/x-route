#!/system/bin/sh
# ==============================================================================
# x-route: 4-Layer Kernel Keep-Alive Engine & OnePlus 13 Power Guard
# Specifically engineered for Android 15/16 (ColorOS 15/16, GKI 6.6):
#   Layer 1: Doze + Netpolicy + AppOps Whitelist + Phantom Process Killer Disable
#   Layer 2: Linux Kernel OOM Immunity (oom_score_adj = -1000 for app & children)
#   Layer 3: Cgroup v2 Freezer (cgroup.freeze = 0) + Root Cgroup Migration + SIGCONT
#   Layer 4: Process Watchdog & Silent Auto-Resurrection
#   Bonus  : OnePlus 13 Battery Bypass / Threshold Charging Control (60%~70%)
# ==============================================================================

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
MODDIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
# shellcheck disable=SC1091
. "${SCRIPT_DIR}/common.sh"

KEEPALIVE_PID_FILE="${RUN_DIR}/keepalive.pid"
WHITELISTED_CACHE="${RUN_DIR}/whitelisted_pkgs"

disable_phantom_process_killer() {
    /system/bin/device_config set_sync_disabled_for_tests persistent >/dev/null 2>&1 || true
    /system/bin/device_config put activity_manager max_phantom_processes 2147483647 >/dev/null 2>&1 || true
    /system/bin/settings put global settings_enable_monitor_phantom_procs false >/dev/null 2>&1 || true
}

get_pkg_uid() {
    local pkg="$1"
    pm list packages -U "${pkg}" 2>/dev/null | awk -F'uid:' -v p="package:${pkg} " '$0 ~ p {print $2; exit}' | tr -d '\r '
}

get_pkg_pids() {
    local pkg="$1"
    local main_pids all_pids=""
    main_pids="$(pidof "${pkg}" 2>/dev/null || pgrep -f "^${pkg}(:|$)" 2>/dev/null)"
    for mp in ${main_pids}; do
        all_pids="${all_pids} ${mp}"
        # Also include child processes (e.g., Alist Go binary spawned by com.leohao.android.alistlite)
        local children
        children="$(pgrep -P "${mp}" 2>/dev/null || true)"
        [ -n "${children}" ] && all_pids="${all_pids} ${children}"
    done
    echo "${all_pids}" | xargs echo 2>/dev/null
}

apply_layer1_whitelist() {
    local pkg="$1"
    touch "${WHITELISTED_CACHE}"
    if grep -qx "${pkg}" "${WHITELISTED_CACHE}" 2>/dev/null; then
        return 0
    fi

    local uid
    uid="$(get_pkg_uid "${pkg}")"
    dumpsys deviceidle whitelist "+${pkg}" >/dev/null 2>&1 || true
    cmd appops set "${pkg}" RUN_IN_BACKGROUND allow >/dev/null 2>&1 || true
    cmd appops set "${pkg}" RUN_ANY_IN_BACKGROUND allow >/dev/null 2>&1 || true
    cmd appops set "${pkg}" WAKE_LOCK allow >/dev/null 2>&1 || true
    cmd appops set "${pkg}" SYSTEM_ALERT_WINDOW allow >/dev/null 2>&1 || true

    if [ -n "${uid}" ]; then
        cmd netpolicy add restrict-background-whitelist "${uid}" >/dev/null 2>&1 || true
        cmd netpolicy add app-idle-whitelist "${uid}" >/dev/null 2>&1 || true
    fi

    echo "${pkg}" >> "${WHITELISTED_CACHE}"
    log_info "keepalive" "[Layer 1] Applied Doze/Netpolicy/AppOps whitelist for ${pkg} (UID=${uid:-unknown})"
}

resurrect_package() {
    local pkg="$1"
    log_warn "keepalive" "[Layer 4] Process '${pkg}' not running! Triggering silent resurrection..."
    # Special handling for known NAS service apps (e.g., Alist Lite)
    if [ "${pkg}" = "com.leohao.android.alistlite" ]; then
        am start-foreground-service -n "${pkg}/.service.AlistService" >/dev/null 2>&1 || \
        am startservice -n "${pkg}/.service.AlistService" >/dev/null 2>&1 || \
        monkey -p "${pkg}" -c android.intent.category.LAUNCHER 1 >/dev/null 2>&1 || true
    else
        # Generic wake via broadcast + launcher intent
        monkey -p "${pkg}" -c android.intent.category.LAUNCHER 1 >/dev/null 2>&1 || true
    fi
    sleep 2
}

protect_single_package() {
    local pkg="$1"
    [ -z "${pkg}" ] && return 0

    # Check if package is installed on device
    if ! pm path "${pkg}" >/dev/null 2>&1; then
        return 0
    fi

    # Layer 1: System Whitelists
    apply_layer1_whitelist "${pkg}"

    # Layer 4: Watchdog Check & Auto-Resurrection
    local pids
    pids="$(get_pkg_pids "${pkg}")"
    if [ -z "${pids}" ]; then
        resurrect_package "${pkg}"
        pids="$(get_pkg_pids "${pkg}")"
    fi

    local uid
    uid="$(get_pkg_uid "${pkg}")"

    # Layer 2 & Layer 3: OOM Lock (-1000) & Cgroup v2 / ColorOS Hans Unfreeze
    for pid in ${pids}; do
        [ -d "/proc/${pid}" ] || continue

        # Layer 2: Immune to LowMemoryKiller / Kernel OOM
        if [ -w "/proc/${pid}/oom_score_adj" ]; then
            echo "-1000" > "/proc/${pid}/oom_score_adj" 2>/dev/null || true
        fi
        if [ -w "/proc/${pid}/oom_adj" ]; then
            echo "-17" > "/proc/${pid}/oom_adj" 2>/dev/null || true
        fi

        # Layer 3a: Unfreeze Cgroup v2 cgroup.freeze node if frozen by Android 15/16 or ColorOS
        if [ -n "${uid}" ] && [ -w "/sys/fs/cgroup/uid_${uid}/pid_${pid}/cgroup.freeze" ]; then
            echo "0" > "/sys/fs/cgroup/uid_${uid}/pid_${pid}/cgroup.freeze" 2>/dev/null || true
        fi
        for freeze_node in "/sys/fs/cgroup/uid_${uid}/cgroup.freeze" "/sys/fs/cgroup/freezer/perf/frozen/cgroup.procs"; do
            if [ -w "${freeze_node}" ] && echo "${freeze_node}" | grep -q "cgroup.freeze"; then
                echo "0" > "${freeze_node}" 2>/dev/null || true
            fi
        done

        # Layer 3b: Migrate process & all its threads to root cgroup to escape ColorOS Hans Freezer
        if [ -w "/sys/fs/cgroup/cgroup.procs" ]; then
            echo "${pid}" > /sys/fs/cgroup/cgroup.procs 2>/dev/null || true
            for tid_dir in "/proc/${pid}/task/"*; do
                local tid
                tid="$(basename "${tid_dir}")"
                echo "${tid}" > /sys/fs/cgroup/cgroup.threads 2>/dev/null || \
                echo "${tid}" > /sys/fs/cgroup/cgroup.procs 2>/dev/null || true
            done
        fi

        # Layer 3c: Send SIGCONT to resume any stopped threads
        kill -CONT "${pid}" 2>/dev/null || true
    done
}

set_charging_switch() {
    local enable="$1" # 1 = enable charging, 0 = suspend/bypass battery charging
    # OnePlus / OPlus specific charging control nodes (ColorOS 15/16)
    for node in \
        "/sys/class/oplus_chg/battery/mmi_charging_enable" \
        "/sys/class/power_supply/battery/mmi_charging_enable" \
        "/sys/class/power_supply/battery/charging_enabled" \
        "/sys/class/power_supply/battery/battery_charging_enabled"; do
        if [ -w "${node}" ]; then
            echo "${enable}" > "${node}" 2>/dev/null || true
        fi
    done
}

manage_power_and_charging() {
    load_config
    if [ "${WAKELOCK_ENABLED:-1}" = "1" ]; then
        acquire_wakelock
    fi

    if [ "${BYPASS_CHARGE_ENABLED:-0}" != "1" ]; then
        return 0
    fi

    local cap=50
    if [ -r /sys/class/power_supply/battery/capacity ]; then
        cap="$(cat /sys/class/power_supply/battery/capacity 2>/dev/null || echo 50)"
    fi

    local stop_lvl="${BYPASS_CHARGE_STOP_LEVEL:-70}"
    local start_lvl="${BYPASS_CHARGE_START_LEVEL:-60}"

    if [ "${cap}" -ge "${stop_lvl}" ]; then
        set_charging_switch 0
    elif [ "${cap}" -le "${start_lvl}" ]; then
        set_charging_switch 1
    fi
}

run_once() {
    load_config
    disable_phantom_process_killer
    manage_power_and_charging

    if [ "${KEEPALIVE_ENABLED:-1}" != "1" ]; then
        return 0
    fi

    local pkg_list
    pkg_list="$(echo "${KEEPALIVE_PACKAGES:-}" | tr ',' ' ')"
    for pkg in ${pkg_list}; do
        protect_single_package "${pkg}"
    done
}

daemon_loop() {
    log_info "keepalive" "Starting 4-Layer Kernel Keep-Alive Daemon..."
    rm -f "${WHITELISTED_CACHE}"
    disable_phantom_process_killer

    while true; do
        load_config
        run_once
        sleep "${KEEPALIVE_INTERVAL_SEC:-25}"
    done
}

start_daemon() {
    ensure_dirs
    if is_pid_running "${KEEPALIVE_PID_FILE}"; then
        return 0
    fi
    nohup "$0" loop >> "${LOG_DIR}/x-route.log" 2>&1 &
    echo "$!" > "${KEEPALIVE_PID_FILE}"
}

stop_daemon() {
    if is_pid_running "${KEEPALIVE_PID_FILE}"; then
        kill -TERM "$(cat "${KEEPALIVE_PID_FILE}")" 2>/dev/null || true
        rm -f "${KEEPALIVE_PID_FILE}"
    fi
    set_charging_switch 1
    rm -f "${WHITELISTED_CACHE}"
    log_info "keepalive" "Keep-Alive daemon stopped."
}

status_daemon() {
    load_config
    local running="false"
    local daemon_pid=""
    if is_pid_running "${KEEPALIVE_PID_FILE}"; then
        running="true"
        daemon_pid="$(cat "${KEEPALIVE_PID_FILE}")"
    fi

    local bat_cap="0"
    [ -r /sys/class/power_supply/battery/capacity ] && bat_cap="$(cat /sys/class/power_supply/battery/capacity 2>/dev/null || echo 0)"
    local bat_status="Unknown"
    [ -r /sys/class/power_supply/battery/status ] && bat_status="$(cat /sys/class/power_supply/battery/status 2>/dev/null || echo Unknown)"

    printf '{"daemon_running":%s,"daemon_pid":"%s","enabled":%s,"battery_cap":%s,"battery_status":"%s","apps":[' \
        "${running}" "${daemon_pid}" "$([ "${KEEPALIVE_ENABLED:-1}" = "1" ] && echo true || echo false)" "${bat_cap}" "${bat_status}"

    local first=1
    local pkg_list
    pkg_list="$(echo "${KEEPALIVE_PACKAGES:-}" | tr ',' ' ')"
    for pkg in ${pkg_list}; do
        [ -z "${pkg}" ] && continue
        local installed="false"
        pm path "${pkg}" >/dev/null 2>&1 && installed="true"
        local pids oom_val="N/A" first_pid=""
        pids="$(get_pkg_pids "${pkg}")"
        first_pid="$(echo "${pids}" | awk '{print $1}')"
        if [ -n "${first_pid}" ] && [ -r "/proc/${first_pid}/oom_score_adj" ]; then
            oom_val="$(cat "/proc/${first_pid}/oom_score_adj" 2>/dev/null)"
        fi
        [ "${first}" -eq 0 ] && printf ','
        first=0
        printf '{"pkg":"%s","installed":%s,"running":%s,"pids":"%s","oom_score_adj":"%s"}' \
            "${pkg}" "${installed}" "$([ -n "${first_pid}" ] && echo true || echo false)" "${pids}" "${oom_val}"
    done
    printf ']}\n'
}

case "${1:-}" in
    start)    start_daemon ;;
    stop)     stop_daemon ;;
    restart)  stop_daemon; sleep 1; start_daemon ;;
    run-once) run_once ;;
    loop)     daemon_loop ;;
    status)   status_daemon ;;
    *)
        echo "Usage: $0 {start|stop|restart|run-once|status}" >&2
        exit 1
        ;;
esac
