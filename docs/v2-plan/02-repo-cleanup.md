# 02 仓库清理与 v2 目录骨架

## 删除

```sh
git rm -r \
  module/META-INF \
  module/scripts/keepalive_daemon.sh \
  module/scripts/net_orchestrator.sh \
  module/scripts/vm_manager.sh \
  openwrt-config/armsr-armv8.diffconfig \
  openwrt-config/files/etc/init.d/x-route-nas \
  openwrt-config/files/etc/init.d/x-route-netmap \
  openwrt-config/files/etc/nftables.d/99-x-route-ttl.nft \
  openwrt-config/files/etc/openvpn/README.ovpn.txt \
  openwrt-config/files/etc/uci-defaults/99-x-route-init \
  scripts/build-openwrt.sh \
  docs/PROPOSAL.md docs/FEASIBILITY_ANALYSIS.md docs/ARCHITECTURE.md docs/USER_MANUAL.md \
  docs/references
git mv openwrt-config/kernel-gunyah-pvm.config openwrt/kernel-gunyah-pvm.config   # 先 mkdir -p openwrt
git rm -r openwrt-config
```

以下文件不删，在后续步骤里整体重写：

| 文件 | 在哪一步重写 |
|---|---|
| `module/module.prop`、`customize.sh`、`service.sh`、`action.sh`、`uninstall.sh` | 05 |
| `module/scripts/common.sh`、`module/config/config.conf` | 05 |
| `module/webroot/index.html`、`app.js`、`style.css` | 10 |
| `scripts/package-module.sh`、`README.md` | 11 |

## 目标目录树

```
.github/workflows/ci.yml
.github/workflows/release.yml
README.md
docs/TESTING.md                     # 内容取自 12
docs/v2-plan/                       # 本文档
module/
  module.prop
  customize.sh
  service.sh
  action.sh
  uninstall.sh
  config/config.conf
  scripts/common.sh
  scripts/vm.sh
  scripts/net.sh
  scripts/vpnroutes.sh
  scripts/share.sh
  scripts/xr
  webroot/index.html
  webroot/app.js
  webroot/style.css
openwrt/
  kernel-gunyah-pvm.config
  packages.txt
  files/etc/inittab
  files/etc/uci-defaults/10-x-route
  files/etc/init.d/x-route-net
  files/etc/init.d/x-route-ready
  files/etc/init.d/x-route-share
scripts/
  build-kernel.sh
  build-rootfs.sh
  package-module.sh
  check-config-keys.sh
```

## 禁止残留

清理后在仓库根目录执行，结果都必须为空：

```sh
grep -rnE "opkg|\.ipk|24\.10|diffconfig|iptables-nft|openlist|alist|5244|keepalive|KEEPALIVE|BYPASS|HOTSPOT_BRIDGE|HOST_REDIRECT|NAS_|qemu|droidvm|termux|192\.168\.100|172\.16\.1|ROUTER_ENABLED|CONFIG_SCHEMA_VER|prepare-lend-mthp|hugepages" \
  --exclude-dir=.git --exclude-dir=v2-plan .
```

## 验收

- 上面的 grep 没有输出（README 在 11 步重写，这一步允许暂时保留旧 README）。
- 仓库目录结构与目标树一致，尚未实现的文件可以暂时没有。
