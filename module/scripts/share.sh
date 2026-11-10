#!/system/bin/sh
# shellcheck disable=SC2034  # share_* 变量由 vm.sh 使用
# ==============================================================================
# XRouter: 目录共享（docs/v2-plan/09-share.md），被 vm.sh 引入。
# share_prepare 设置三个变量供 vm_start 使用：
#   share_mode  off|ro|rw          -> 传给 guest 的 xr.share=
#   share_spec  crosvm --shared-dir 参数（空表示不共享）
#   share_wrap  启动 crosvm 的前缀命令（ro 模式在私有挂载命名空间里做只读绑定）
# 任一前提不满足都只是不共享，VM 照常启动。RW_MAP_OPT 定义在 common.sh。
# ==============================================================================

share_prepare() {
  share_spec=""; share_wrap=""; share_mode=off
  [ "${VM_SHARE}" = 1 ] || return 0
  ce_ready || { log_err share "手机未解锁（CE 不可用），本次不共享"; return 0; }
  P="${VM_SHARE_PATH}"
  valid_kv VM_SHARE_PATH "$P" || { log_err share "路径不允许或不存在：$P（只能是 ${MEDIA} 或其子目录）"; return 0; }
  base="${P}:xrshare:type=fs:cache=auto:timeout=1"
  if [ "${VM_SHARE_MODE}" = rw ]; then
    [ -n "${RW_MAP_OPT}" ] || { log_err share "当前 crosvm 不支持属主映射，rw 不可用，本次不共享"; return 0; }
    share_spec="${base}:${RW_MAP_OPT}"; share_mode=rw
  else
    # 只读绑定只存在于 crosvm 自己的挂载命名空间里：Android 看不到，crosvm 退出即消失
    cat > "${RUN}/share-ro.sh" <<EOS
#!/system/bin/sh
${BB} mount --make-rslave / || exit 1
${BB} mount --bind "$P" "$P" || exit 1
${BB} mount -o remount,bind,ro,nosuid,nodev,noexec "$P" || exit 1
exec "\$@"
EOS
    chmod 0700 "${RUN}/share-ro.sh"
    share_spec="${base}"; share_mode=ro
    share_wrap="${BB} unshare -m sh ${RUN}/share-ro.sh"
  fi
  log_info share "共享 ${P} 模式=${share_mode}"
}
