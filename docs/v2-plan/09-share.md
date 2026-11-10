# 09 目录共享：`module/scripts/share.sh`（新增）

默认不共享。WebUI 提供三项设置：开关 `VM_SHARE`、权限 `VM_SHARE_MODE=ro|rw`（默认 `ro`）、路径 `VM_SHARE_PATH`（默认 `/data/media/0`）。guest 内挂载到 `/mnt/sdcard`，virtiofs 标签固定为 `xrshare`。

## 前提

- `VM_SHARE=1`；
- `ce_ready` 为真，即 `getprop sys.user.0.ce_available` 返回 `true`；
- 路径存在，并且是 `/data/media/0` 或它下面的子目录。

任一条件不满足：`share_mode=off`，VM 照常启动，`xr.log` 和 WebUI 给出原因。

只使用底层路径 `/data/media/0`，不用 FUSE 路径 `/storage/emulated/0`。

## `share.sh`

```sh
BB=/data/adb/ksu/bin/busybox
# rw 模式的属主映射参数。在 12 的前置检查里根据设备上的 crosvm 确定；为空表示不支持 rw
RW_MAP_OPT=""      # 候选 A：ugid_map=...（crosvm fs 的 ugid_map 字段）；候选 B："uidmap=0 1023 1:gidmap=0 1023 1"

share_prepare() {
  share_spec=""; share_wrap=""; share_mode=off
  [ "${VM_SHARE}" = 1 ] || return 0
  ce_ready || { log_err share "手机未解锁（CE 不可用），本次不共享"; return 0; }
  P="${VM_SHARE_PATH}"
  case "$P" in /data/media/0|/data/media/0/*) ;; *) log_err share "路径不允许：$P"; return 0;; esac
  [ -d "$P" ] || { log_err share "路径不存在：$P"; return 0; }
  base="${P}:xrshare:type=fs:cache=auto:timeout=1"
  if [ "${VM_SHARE_MODE}" = rw ]; then
    [ -n "${RW_MAP_OPT}" ] || { log_err share "当前 crosvm 不支持属主映射，rw 不可用"; return 0; }
    share_spec="${base}:${RW_MAP_OPT}"; share_mode=rw
  else
    cat > "${RUN}/share-ro.sh" <<EOF
#!/system/bin/sh
${BB} mount --make-rslave /
${BB} mount --bind "$P" "$P"
${BB} mount -o remount,bind,ro,nosuid,nodev,noexec "$P"
exec "\$@"
EOF
    share_spec="${base}"; share_mode=ro
    share_wrap="${BB} unshare -m sh ${RUN}/share-ro.sh"
  fi
  log_info share "共享 ${P} 模式=${share_mode}"
}
```

## ro 模式

- 只读绑定挂载只存在于 crosvm 自己的挂载命名空间里，Android 和其他 App 都看不到。crosvm 退出后自动消失，停止和卸载时不需要清理。
- 共享内容在手机侧由内核强制只读，即使是 root，写入也会返回 `EROFS`。
- guest 侧 `x-route-share` 按 `xr.share=ro` 执行 `mount -o ro`，这只是第二层。
- 不做 uid/gid 映射，不写入任何文件，所以不会影响 MediaStore、配额和权限。

## rw 模式

- 不做只读绑定。属主映射为 media_rw（1023:1023），参数是 `RW_MAP_OPT`。
- 设备上的 crosvm 两种写法都不支持时，`RW_MAP_OPT` 保持为空。此时 `valid_kv VM_SHARE_MODE rw` 返回失败，WebUI 里 rw 选项置灰，并注明原因。
- WebUI 在 rw 选项旁固定显示：
  - VM 写入的文件不会经过 MediaStore，相册、文件管理器需要手动扫描或重启后才能看到；
  - 不要写入 `Android/data`、`Android/obb`。

## 通用

- 共享参数固定为 `cache=auto:timeout=1`，不使用 `posix_acl`、`writeback`、`dax`。
- crosvm 以 root 运行在 KernelSU 的 su 域，访问 `media_rw_data_file` 不会被 SELinux 拒绝；新文件继承父目录的 SELinux 标签。
- VM 能读到整个共享路径（ro 模式下也是）。WebUI 提示可以把路径缩小到 `/data/media/0/Download` 等子目录。
- 共享相关设置包含在 `HASH_KEYS` 里，修改后 `last_good` 失效，自启需要先手动成功启动一次。

## 验收（在真机上）

- `VM_SHARE=0`：启动命令中没有 `--shared-dir`，guest 中没有 `/mnt/sdcard` 挂载。
- ro：
  - guest 里 `mount | grep sdcard` 显示 `virtiofs ... ro`；
  - `touch /mnt/sdcard/x` 返回只读文件系统错误；
  - 把 guest 重新挂载为可写后再 touch，仍然失败（证明手机侧只读生效）；
  - 手机上 `grep /data/media /proc/1/mountinfo` 没有新增挂载；
  - 手机新建一个文件，约 1 秒后在 guest 里能看到。
- rw（仅在 `RW_MAP_OPT` 非空时测）：guest 里新建文件，手机上 `ls -ln` 显示 1023 1023，文件管理器 App 能打开。
- 开机后未解锁时启动 VM：共享被跳过，`xr.log` 有记录，VM 正常运行。
- 读取一个 500MB 文件（`dd if=/mnt/sdcard/<file> of=/dev/null bs=1M`）：整机不重启，记录吞吐。
