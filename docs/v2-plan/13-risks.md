# 13 风险与未决问题

| # | 风险 | 影响 | 应对 |
|---|---|---|---|
| 1 | 1.0.1 整机复位的根因仍未确认 | v2 可能同样复位 | 保留全部安全措施（`--no-balloon`、swiotlb 64、512MB、`ulimit -l`、熔断）；按 12-2 二分；熔断保证不会出现开机循环 |
| 2 | 设备上的 ndc 不支持 `network interface add local` 或 `ipfwd` | 不能用 netd 路由 | 07 里已经实现退回模式（table 97、规则 18900、`ip_forward`）；状态里显示 `mode` |
| 3 | 依赖 netd 规则 `fwmark 0x0/0xffff iif lo lookup <iface>` 的格式 | 识别不到上游，VM 不能上网 | 识别失败时 `up_if` 为空，只保留 VM 与手机互通，并写日志；Android 大版本升级后需要先跑 12-0 |
| 4 | 自编内核和 ImmortalWrt 官方 kmod 的 vermagic 或符号不兼容 | `apk add kmod-*` 后无法加载 | 03 打上 ImmortalWrt 的 generic 和 armsr 补丁，并使用官方 config；12-3 验证；失败时在 03 里对比 `Module.symvers` |
| 5 | 6.12.103 上的补丁冲突 | 构建失败 | `build-kernel.sh` 遇到冲突时报错退出并打印补丁名；只允许在 `openwrt/kernel-patches-skip.txt` 里显式跳过，并在 README 说明 |
| 6 | 只有 ipk、没有 apk 的第三方包 | 25.12 装不了 | 不支持。packages.txt 只使用官方源里的包 |
| 7 | OpenVPN 2.7 的 DNS 处理（dns-updown）与 OpenWrt 不配合 | 推送的 DNS 覆盖 resolv，或不生效 | 08 给出了 `dns-updown disable` 和在 dnsmasq 里手动配置的写法；12-6 记录实际行为 |
| 8 | 6.12 没有 DCO | 吞吐下降 | 接受这个限制；普通 tun 在这个场景下够用 |
| 9 | 在 pVM 上 virtio-fs 没有测过（swiotlb 跳板 + DAX 关闭） | 共享慢或不可用 | 默认关闭；12-5 测吞吐；失败时这一项在 WebUI 中标注为实验性 |
| 10 | crosvm 的 rw 属主映射语法因版本而异，或不支持 | rw 写入的文件属于 root，App 无法读取 | `RW_MAP_OPT` 由 12-0 确定；为空时 rw 置灰，只提供 ro |
| 11 | 第二个 virtio-console 在 pVM 上不可用 | 没有 hvc0 | `x-route-ready` 退回 ttyS0；`ready_seen` 两个文件都检查。如果添加第二个 `--serial` 就导致启动失败，需要把它从 06 的命令中删除（只改一行） |
| 12 | 上游默认 root 空密码，热点客户端可直接登录 | 同一热点下的人能登录 VM | README、WebUI 和 RELEASE_NOTES 都提示在 LuCI 设置密码；`XR_EXPOSE=off` 时只有手机自己能访问；dropbear 只监听 lan |
| 13 | `172.29.233.0/24` 和用户现有网络或 VPN 网段冲突 | 路由冲突 | 可以修改 `XR_NET_PREFIX`；`routes_valid` 拒绝与 VM 网段重叠的 VPN 网段；12-0 检查 |
| 14 | `/data/media/0` 和 MediaStore、FUSE 的一致性问题 | rw 写入后相册看不到，或缓存过期 | 用 `cache=auto:timeout=1`；在 WebUI 说明；默认 ro |
| 15 | `VPN_ROUTES` 只能手动维护 | 对端网段变化后需要手动修改 | 这是有意的设计（已移除 reporter）；WebUI 显示已生效和被拒绝的网段，方便核对 |
