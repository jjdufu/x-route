#!/system/bin/sh
# ==============================================================================
# x-route: OpenWrt Virtual Machine Manager (Gunyah crosvm & QEMU Dual-Backend)
# Supports:
#   1. Qualcomm Gunyah Hypervisor (/dev/gunyah) via crosvm (Protected VM)
#   2. Linux KVM (/dev/kvm) or Multi-Threaded TCG via qemu-system-aarch64
#   3. Host Physical NAS Storage Passthrough (/data/media/0/NAS -> host_nas)
# ==============================================================================

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
MODDIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
# shellcheck disable=SC1091
. "${SCRIPT_DIR}/common.sh"

VM_PID_FILE="${RUN_DIR}/vm.pid"
VM_BACKEND_FILE="${RUN_DIR}/vm.backend"
CROSVM_SOCK="${RUN_DIR}/crosvm.sock"
CONSOLE_LOG="${LOG_DIR}/vm-console.log"

find_crosvm_bin() {
    for candidate in \
        "${BIN_DIR}/crosvm" \
        "${MODDIR}/bin/crosvm" \
        "/data/data/cn.classfun.droidvm/usr/bin/crosvm" \
        "/apex/com.android.virt/bin/crosvm"; do
        if [ -x "${candidate}" ]; then
            echo "${candidate}"
            return 0
        fi
    done
    return 1
}

find_qemu_bin() {
    for candidate in \
        "${BIN_DIR}/qemu-system-aarch64" \
        "${MODDIR}/bin/qemu-system-aarch64" \
        "/data/data/com.termux/files/usr/bin/qemu-system-aarch64"; do
        if [ -x "${candidate}" ]; then
            echo "${candidate}"
            return 0
        fi
    done
    command -v qemu-system-aarch64 2>/dev/null && return 0
    return 1
}

select_backend() {
    load_config
    local req="${VM_BACKEND:-auto}"
    if [ "${req}" = "gunyah" ]; then
        echo "gunyah"
        return 0
    elif [ "${req}" = "qemu" ]; then
        echo "qemu"
        return 0
    fi

    # Auto-detect best available hypervisor on OnePlus 13 (SM8750)
    if [ -e /dev/gunyah ] && find_crosvm_bin >/dev/null 2>&1; then
        echo "gunyah"
    elif find_qemu_bin >/dev/null 2>&1; then
        echo "qemu"
    elif find_crosvm_bin >/dev/null 2>&1; then
        echo "gunyah"
    else
        echo "none"
    fi
}

prepare_vm_images() {
    ensure_dirs
    # Copy bundled images from module package if not yet in persistent directory
    if [ ! -f "${IMG_DIR}/Image" ] && [ -f "${MODDIR}/data/Image" ]; then
        log_info "vm_manager" "Copying bundled kernel Image to ${IMG_DIR}/Image"
        cp -f "${MODDIR}/data/Image" "${IMG_DIR}/Image"
    fi

    if [ ! -f "${IMG_DIR}/openwrt-rootfs.raw" ]; then
        if [ -f "${IMG_DIR}/openwrt-rootfs.img.gz" ]; then
            log_info "vm_manager" "Decompressing ${IMG_DIR}/openwrt-rootfs.img.gz -> openwrt-rootfs.raw"
            gzip -dc "${IMG_DIR}/openwrt-rootfs.img.gz" > "${IMG_DIR}/openwrt-rootfs.raw"
        elif [ -f "${MODDIR}/data/openwrt-rootfs.img.gz" ]; then
            log_info "vm_manager" "Decompressing bundled openwrt-rootfs.img.gz -> ${IMG_DIR}/openwrt-rootfs.raw"
            gzip -dc "${MODDIR}/data/openwrt-rootfs.img.gz" > "${IMG_DIR}/openwrt-rootfs.raw"
        elif [ -f "${MODDIR}/data/openwrt-rootfs.raw" ]; then
            cp -f "${MODDIR}/data/openwrt-rootfs.raw" "${IMG_DIR}/openwrt-rootfs.raw"
        fi
    fi

    if [ ! -f "${IMG_DIR}/Image" ]; then
        log_err "vm_manager" "Missing OpenWrt kernel at ${IMG_DIR}/Image"
        return 1
    fi
    if [ ! -f "${IMG_DIR}/openwrt-rootfs.raw" ] && [ ! -f "${IMG_DIR}/openwrt.qcow2" ]; then
        log_err "vm_manager" "Missing OpenWrt rootfs at ${IMG_DIR}/openwrt-rootfs.raw"
        return 1
    fi
    return 0
}

start_vm_gunyah() {
    local crosvm_bin
    crosvm_bin="$(find_crosvm_bin)" || {
        log_err "vm_manager" "crosvm binary not found"
        return 1
    }

    rm -f "${CROSVM_SOCK}"
    local nas_args=""
    if [ "${NAS_MOUNT_ENABLED:-1}" = "1" ] && [ -n "${NAS_HOST_PATH}" ]; then
        mkdir -p "${NAS_HOST_PATH}" 2>/dev/null || true
        if [ -d "${NAS_HOST_PATH}" ]; then
            nas_args="--shared-dir ${NAS_HOST_PATH}:host_nas:type=fs:cache=always"
            log_info "vm_manager" "Enabling virtio-fs passthrough: ${NAS_HOST_PATH} -> host_nas"
        fi
    fi

    # Check if gh_hugepage_reserve kernel module is active for custom crosvm flags
    local hp_args=""
    if [ -d /sys/module/gh_hugepage_reserve ] && "${crosvm_bin}" run --help 2>&1 | grep -q "prepare-lend-mthp"; then
        hp_args="--hugepages --prepare-lend-mthp"
    fi

    log_info "vm_manager" "Starting OpenWrt VM via Gunyah (${crosvm_bin}, vCPUs=${VM_CPUS:-2}, RAM=${VM_MEM_MB:-256}MB)"
    # Launch crosvm in background
    # shellcheck disable=SC2086
    nohup "${crosvm_bin}" --log-level=info run \
        --hypervisor gunyah \
        --protected-vm-without-firmware \
        --disable-sandbox \
        --cpus "${VM_CPUS:-2}" \
        --mem "${VM_MEM_MB:-256}" \
        --net "tap-name=${WAN_TAP},mac=${WAN_MAC}" \
        --net "tap-name=${LAN_TAP},mac=${LAN_MAC}" \
        --block "${IMG_DIR}/openwrt-rootfs.raw,lock=false,root=true" \
        --params "root=/dev/vda rw rootwait console=ttyS0,115200 earlycon panic=5" \
        --serial "type=file,path=${CONSOLE_LOG},hardware=serial,num=1,console=true,earlycon=true" \
        --socket "${CROSVM_SOCK}" \
        ${nas_args} ${hp_args} \
        "${IMG_DIR}/Image" >> "${LOG_DIR}/vmm-runtime.log" 2>&1 &

    local pid=$!
    echo "${pid}" > "${VM_PID_FILE}"
    echo "gunyah" > "${VM_BACKEND_FILE}"
    sleep 2
    if kill -0 "${pid}" 2>/dev/null; then
        log_info "vm_manager" "OpenWrt Gunyah VM started successfully (PID=${pid})"
        return 0
    else
        log_err "vm_manager" "crosvm exited prematurely. Check ${LOG_DIR}/vmm-runtime.log"
        rm -f "${VM_PID_FILE}"
        return 1
    fi
}

start_vm_qemu() {
    local qemu_bin
    qemu_bin="$(find_qemu_bin)" || {
        log_err "vm_manager" "qemu-system-aarch64 binary not found"
        return 1
    }

    local accel_args="-M virt,accel=tcg,thread=multi -cpu cortex-a76"
    if [ -e /dev/kvm ]; then
        accel_args="-M virt,accel=kvm -cpu host"
    fi

    local disk_path="${IMG_DIR}/openwrt-rootfs.raw"
    local disk_fmt="raw"
    if [ -f "${IMG_DIR}/openwrt.qcow2" ]; then
        disk_path="${IMG_DIR}/openwrt.qcow2"
        disk_fmt="qcow2"
    fi

    local nas_args=""
    if [ "${NAS_MOUNT_ENABLED:-1}" = "1" ] && [ -n "${NAS_HOST_PATH}" ]; then
        mkdir -p "${NAS_HOST_PATH}" 2>/dev/null || true
        if [ -d "${NAS_HOST_PATH}" ]; then
            nas_args="-fsdev local,path=${NAS_HOST_PATH},security_model=none,id=fs_nas -device virtio-9p-pci,fsdev=fs_nas,mount_tag=host_nas"
            log_info "vm_manager" "Enabling virtio-9p passthrough: ${NAS_HOST_PATH} -> host_nas"
        fi
    fi

    # Ensure dynamic library path if using Termux QEMU binary
    if echo "${qemu_bin}" | grep -q "com.termux"; then
        export LD_LIBRARY_PATH="/data/data/com.termux/files/usr/lib:${LD_LIBRARY_PATH:-}"
    fi

    log_info "vm_manager" "Starting OpenWrt VM via QEMU (${qemu_bin}, vCPUs=${VM_CPUS:-2}, RAM=${VM_MEM_MB:-256}MB)"
    # shellcheck disable=SC2086
    nohup "${qemu_bin}" \
        ${accel_args} \
        -smp "${VM_CPUS:-2}" \
        -m "${VM_MEM_MB:-256}M" \
        -kernel "${IMG_DIR}/Image" \
        -drive "file=${disk_path},if=virtio,format=${disk_fmt}" \
        -netdev "tap,id=wan0,ifname=${WAN_TAP},script=no,downscript=no" \
        -device "virtio-net-pci,netdev=wan0,mac=${WAN_MAC}" \
        -netdev "tap,id=lan0,ifname=${LAN_TAP},script=no,downscript=no" \
        -device "virtio-net-pci,netdev=lan0,mac=${LAN_MAC}" \
        ${nas_args} \
        -nographic \
        -serial "file:${CONSOLE_LOG}" \
        -monitor none \
        -append "root=/dev/vda rw rootwait console=ttyAMA0,115200 earlycon panic=5" \
        >> "${LOG_DIR}/vmm-runtime.log" 2>&1 &

    local pid=$!
    echo "${pid}" > "${VM_PID_FILE}"
    echo "qemu" > "${VM_BACKEND_FILE}"
    sleep 2
    if kill -0 "${pid}" 2>/dev/null; then
        log_info "vm_manager" "OpenWrt QEMU VM started successfully (PID=${pid})"
        return 0
    else
        log_err "vm_manager" "QEMU exited prematurely. Check ${LOG_DIR}/vmm-runtime.log"
        rm -f "${VM_PID_FILE}"
        return 1
    fi
}

start_vm() {
    load_config
    if is_pid_running "${VM_PID_FILE}"; then
        log_info "vm_manager" "OpenWrt VM is already running (PID=$(cat "${VM_PID_FILE}"))"
        return 0
    fi

    prepare_vm_images || return 1

    # Ensure TAP interfaces exist before binding VMM
    if ! ip link show "${WAN_TAP}" >/dev/null 2>&1 || ! ip link show "${LAN_TAP}" >/dev/null 2>&1; then
        "${SCRIPT_DIR}/net_orchestrator.sh" init-taps
    fi

    if [ "${WAKELOCK_ENABLED:-1}" = "1" ]; then
        acquire_wakelock
    fi

    : > "${CONSOLE_LOG}"
    local backend
    backend="$(select_backend)"
    case "${backend}" in
        gunyah)
            if ! start_vm_gunyah; then
                log_warn "vm_manager" "Gunyah crosvm start failed; attempting QEMU fallback if available..."
                if find_qemu_bin >/dev/null 2>&1; then
                    start_vm_qemu
                else
                    return 1
                fi
            fi
            ;;
        qemu)
            start_vm_qemu
            ;;
        *)
            log_err "vm_manager" "No supported hypervisor binary (crosvm or qemu-system-aarch64) found!"
            return 1
            ;;
    esac
}

stop_vm() {
    load_config
    if [ -S "${CROSVM_SOCK}" ]; then
        local crosvm_bin
        if crosvm_bin="$(find_crosvm_bin)"; then
            "${crosvm_bin}" stop "${CROSVM_SOCK}" 2>/dev/null || true
            sleep 1
        fi
    fi

    if is_pid_running "${VM_PID_FILE}"; then
        local pid
        pid="$(cat "${VM_PID_FILE}")"
        log_info "vm_manager" "Stopping OpenWrt VM (PID=${pid})..."
        kill -TERM "${pid}" 2>/dev/null || true
        sleep 2
        if kill -0 "${pid}" 2>/dev/null; then
            kill -KILL "${pid}" 2>/dev/null || true
        fi
    fi
    rm -f "${VM_PID_FILE}" "${CROSVM_SOCK}"
    release_wakelock
    log_info "vm_manager" "OpenWrt VM stopped."
}

status_vm() {
    load_config
    local running="false"
    local pid=""
    local backend="none"
    [ -f "${VM_BACKEND_FILE}" ] && backend="$(cat "${VM_BACKEND_FILE}" 2>/dev/null)"
    if is_pid_running "${VM_PID_FILE}"; then
        running="true"
        pid="$(cat "${VM_PID_FILE}")"
    fi
    printf '{"running":%s,"pid":"%s","backend":"%s","cpus":%s,"mem_mb":%s}\n' \
        "${running}" "${pid}" "${backend}" "${VM_CPUS:-2}" "${VM_MEM_MB:-256}"
}

case "${1:-}" in
    start)   start_vm ;;
    stop)    stop_vm ;;
    restart) stop_vm; sleep 1; start_vm ;;
    status)  status_vm ;;
    *)
        echo "Usage: $0 {start|stop|restart|status}" >&2
        exit 1
        ;;
esac
