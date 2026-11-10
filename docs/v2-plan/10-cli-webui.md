# 10 `xr` 命令行与 WebUI

## `module/scripts/xr`（新增，WebUI 只调用它）

```sh
#!/system/bin/sh
MODDIR="${MODDIR:-/data/adb/modules/x_route}"; . "${MODDIR}/scripts/common.sh"; ensure_dirs
. "${MODDIR}/scripts/vm.sh"   # vm.sh 只定义函数；单独运行时末尾的 case 由 [ "${0##*/}" = vm.sh ] 判断
cmd="$1"; shift
case "$cmd" in
  start)       vm_start ;;
  stop)        vm_stop ;;
  running)     vm_running ;;
  status-json) status_json ;;
  status-line) status_line ;;
  get)         load_config; for k in ${KEYS}; do eval "printf '%s=%s\n' $k \"\${$k}\""; done ;;
  set)         set_kv "$1" "$2" || { echo '{"ok":false}'; exit 1; }; after_set "$1"; echo "{\"ok\":true,\"restart\":$(needs_restart "$1")}" ;;
  ack-trip)    rm -f "${TRIPPED}"; sync ;;
  expand)      disk_expand "$1" ;;
  reset-disk)  disk_reset ;;
  sync)        vm_running && sh "${MODDIR}/scripts/net.sh" sync ;;
  log)         f=xr; case "$1" in console) f="${RUN}/console.log";; ctl) f="${RUN}/vm-ctl.log";; vmm) f="${XR_DATA}/log/vmm.log";; *) f="${LOG}";; esac
               tail -n "${2:-200}" "$f" 2>/dev/null ;;
  *) echo "usage: xr start|stop|running|status-json|status-line|get|set K V|ack-trip|expand MB|reset-disk|sync|log [xr|console|ctl|vmm] [N]"; exit 2 ;;
esac
```

### 辅助函数

| 函数 | 行为 |
|---|---|
| `after_set K` | `XR_EXPOSE`、`VPN_ROUTES`：VM 在运行时执行 `net.sh sync`；`AUTOSTART=1`：如果 `last_good` 与当前 `config_hash` 不一致，改回 0 并返回错误“需先手动启动成功一次” |
| `needs_restart K` | 键在 `XR_NET_PREFIX XR_VM_MAC XR_DNS VM_CPUS VM_MEM_MB VM_SWIOTLB_MB VM_NET VM_DISK_RW VM_SHARE VM_SHARE_MODE VM_SHARE_PATH VPN_MODE` 中，并且 VM 在运行时输出 `true`，否则 `false` |
| `status_line` | 一行文字，例如：`运行中 / 已就绪 / 172.29.233.254 / 暴露=hotspot / VPN网段=2` |
| `status_json` | 把 `vm_status` 的输出、`${RUN}/net.*` 状态文件、`routes.active` 和配置拼成一个 JSON，见下表 |

### `status_json` 字段

```json
{
  "vm": {"running":true,"pid":1234,"state":"stable","uptime_s":812,"tripped":null,"last_good_match":true,"ready_channel":"hvc"},
  "net": {"mode":"ndc","up_if":"wlan0","expose":"hotspot","expose_if":["wlan1"],"vm_ip":"172.29.233.254","host_ip":"172.29.233.1"},
  "vpn": {"mode":"terminal","routes_active":["10.20.0.0/16"],"routes_rejected":["8.8.8.0/24"]},
  "share": {"enabled":false,"mode":"off","path":"/data/media/0","ce_ready":true,"rw_supported":false},
  "config": {"AUTOSTART":"0","...":"..."}
}
```

`routes_rejected` 等于 `VPN_ROUTES` 去掉 `routes.active` 后剩下的部分。`rw_supported` 取决于 `share.sh` 里的 `RW_MAP_OPT` 是否非空。

JSON 用 `printf` 拼接，字符串值只来自已经校验过的配置和网卡名，不需要转义。

## WebUI（重写 `module/webroot/index.html`、`app.js`、`style.css`）

- 调用方式：`ksu.exec("sh /data/adb/modules/x_route/scripts/xr <args>")`。参数在前端做白名单校验，值用单引号包起来，并把值里的 `'` 转义成 `'\''`。
- 每 5 秒刷新一次 `status-json`，页面在后台时暂停刷新。
- 不再直接 `cat` 或写配置文件；删除旧的 heredoc 整文件写法。

### 页面区块

| 区块 | 内容 |
|---|---|
| 状态 | VM 运行状态、就绪状态、运行时长；网络模式（ndc / 退回模式）；上游网卡；放行的网卡；VPN 已生效和被拒绝的网段；共享状态 |
| 熔断条 | `tripped` 非空时置顶显示“上次启动后整机复位（时间）”和“确认”按钮（调用 `ack-trip`）。未确认时“启动”按钮置灰 |
| 操作 | 启动 / 停止；查看日志（xr、console、ctl、vmm 切换，显示最后 200 行）；扩容（输入 MB）；重置系统盘（两次确认，提示 VM 内数据全部丢失） |
| 访问 | `http://172.29.233.254`（LuCI）、`http://172.29.233.254:7681`（ttyd）、`ssh root@172.29.233.254`；固定提示文字：“默认用户 root，空密码（上游默认），可在 LuCI 系统 → 管理权 中设置密码。” |
| 网络 | `XR_EXPOSE` 三选一；选 `all` 时显示“需要在上级路由器上添加静态路由 `172.29.233.0/24 via <手机 IP>`” |
| OpenVPN | `VPN_MODE` 二选一，注明改后需重启 VM；`VPN_ROUTES` 多行文本框，每行一个 CIDR，前端校验后用逗号拼接提交；显示已生效和被拒绝的网段；导入说明“在 LuCI → VPN → OpenVPN 上传 rtvpn 签发的 .ovpn，需单独为本设备签发” |
| 共享 | 开关；权限 ro/rw（`rw_supported=false` 时 rw 置灰并说明原因）；路径；rw 选项旁固定显示 MediaStore 说明 |
| 启动 | 开机自启开关（`last_good_match=false` 或存在熔断时置灰并说明原因）；自启延迟 |
| 诊断 | CPU、内存、swiotlb（32/64/128）、网络开关、磁盘读写开关，就绪超时、稳定期时长；注明“修改后需重启 VM，并重新经过一次手动成功启动才能自启” |

### 删除的旧 UI

保活面板、热点桥接、主机流量重定向、NAS、`:5244` 链接、旁路充电，以及所有写死 `192.168.100.1` 的地方。

## 验收

- WebUI 每个设置项都调用 `xr set`，返回 `ok:false` 时页面显示错误，配置文件内容不变。
- 修改 `VPN_ROUTES` 或 `XR_EXPOSE` 后，几秒内状态区和手机路由同步变化，VM 不需要重启。
- 修改需要重启的键时，页面提示“需重启 VM”。
- 存在熔断时“启动”按钮不可用，点“确认”后恢复可用。
- `grep -nE "192\.168\.100|5244|keepalive|alist|cat <<" module/webroot/*` 没有输出。
