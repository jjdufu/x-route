# 第 1 步：重命名为 XRouter

## 目标

把产品对外名称、内部标识、路径、脚本注释、文档、打包与 CI 产物名全部改为 XRouter / `xrouter`。不做旧 id、旧路径的别名、软链接或迁移。

本步只做更名与由此必须修改的测试 / 文案，不在本步实现全局网关、OpenVPN 自动同步或新 WebUI。

## 依赖

- 依赖第 0 步文档已入库。
- 本步完成后，第 2 步及之后的脚本一律只认新路径。

## 要改的文件

全局检索并替换（含脚本、WebUI、测试、文档、CI、打包、固件 `files/` 中的标识），至少包括：

- `module/module.prop`
- `module/customize.sh`、`module/service.sh`、`module/action.sh`、`module/uninstall.sh`
- `module/config/config.conf`
- `module/scripts/common.sh`、`vm.sh`、`net.sh`、`share.sh`、`vpnroutes.sh`、`xr`
- `module/webroot/*`（本步只改名称与路径，不重做页面结构）
- `openwrt/packages.txt`、`openwrt/files/etc/**`
- `scripts/*.sh`
- `tests/**`
- `.github/workflows/*.yml`
- `README.md`、`docs/TESTING.md`

检索关键词：`x_route`、`x-route`、`x-route-net`、`x-route-ready`、`x-route-share`、`10-x-route`、`/data/adb/x-route`、`id=x_route`、`ndc ipfwd enable xroute`、唤醒锁 `x-route`。

## 具体做法

### 1. 模块元数据

`module.prop`：

```
id=xrouter
name=XRouter
version=v3.0.0
versionCode=300
author=jjdufu
description=在手机虚拟机中运行 OpenWrt。
```

description 保持一句、简洁，不写长功能清单。version 必须仍符合 `v主.次.修`，以便现有打包脚本校验。

### 2. 路径与默认值

`common.sh`（及所有写死旧路径的地方）：

- `MODDIR` 默认 `/data/adb/modules/xrouter`
- `XR_DATA` 默认 `/data/adb/xrouter`
- `LOG` 改为 `${XR_DATA}/log/module.log`（轮转仍为超过 512KB 则改为 `module.log.1`）
- 唤醒锁写入 `/sys/power/wake_lock` 的名字改为 `xrouter`
- WebUI 调用改为 `sh /data/adb/modules/xrouter/scripts/xr`

`customize.sh`：

- 安装提示改为「XRouter」，去掉过长说明。
- 数据目录用新路径。Lite 包镜像目录改为 `/data/adb/xrouter/images/`。
- 不读取、不迁移 `/data/adb/x-route`。
- 默认仍不自启。

`service.sh`、`uninstall.sh`：只使用新路径（卸载彻底性在第 2 步加强，本步先改路径以免后续脚本写错位置）。

### 3. 网络内部短名

- tap 仍为 `xr0`，链名仍为 `XR_FWD` / `XR_NAT`。
- `ndc ipfwd enable xrouter`（以及 disable、对账时用同一名字）。
- `net.sh`、测试夹具、文档同步。

### 4. 固件 overlay 文件名

- `openwrt/files/etc/uci-defaults/10-x-route` → `10-xrouter`
- `openwrt/files/etc/init.d/x-route-net` → `xrouter-net`
- `openwrt/files/etc/init.d/x-route-ready` → `xrouter-ready`
- `openwrt/files/etc/init.d/x-route-share` → `xrouter-share`

脚本内 `logger -t`、注释、`build-rootfs.sh` 的存在性检查路径一并改。主机名与型号留到第 6 步再改（本步若顺便改文件内 `hostname='x-route'` 也可以，但第 6 步会正式验收为 `XRouter`）。推荐本步就把 uci-defaults 里的 `hostname` 改为 `XRouter`，避免中间状态。设备型号本步可先不改。

### 5. 打包与 CI

`scripts/package-module.sh`：

- 产物：`dist/xrouter-sukisu-full-v3.0.0.zip`、`dist/xrouter-sukisu-lite-v3.0.0.zip`
- Lite 说明中的镜像路径改为 `/data/adb/xrouter/images`
- `RELEASE_NOTES.md` 按新产品名与 v3 来写，中文，并写明预发布、尚未真机测试。本步可先出一版占位，第 9 步发版前再按最终行为修订。

工作流：

- `package-module.yml` 的 artifact 名与 `path` 改为 `xrouter-sukisu-lite-*.zip`
- `build-openwrt.yml` 的 artifact `path`、Release `files` 改为 `dist/xrouter-sukisu-*.zip`；工作流显示名去掉过时产品名，保留技术含义即可
- 标签规则不变：`<module.prop 的 version>-build.<run_number>`，例如 `v3.0.0-build.<N>`
- 仍仅在 `main` 上发预发布

### 6. 测试与文档

- 所有测试里的 `x_route`、`/data/adb/x-route`、`x-route-sukisu`、`xr.log`、ipfwd 名、日志断言同步更新。
- `test-xr.sh` 中 `xr get` 的键数量按本步实际键数断言（若本步尚未删除已废弃键，可暂时保持，在第 4、5 步改键时再改断言）。本步不强制删键，以免与尚未实现的功能纠缠；但新路径必须改完。
- `README.md`、`docs/TESTING.md`：产品名、路径、包名改为 XRouter / `xrouter`。不写旧名别名对照表。说明改为全新安装，先卸载旧版。

### 7. 替换时注意

- 不要误伤 `xr0`、`XR_FWD`、`XR_NET_PREFIX`、CLI 脚本文件名 `xr`。
- 不要改 Git 远程 URL 或仓库名。
- 不要在文档中写兼容说明，例如「仍可使用 /data/adb/x-route」。

## 验收标准

- `grep -R` 在 `module/`、`scripts/`、`tests/`、`openwrt/files/`、`.github/`、`README.md`、`docs/TESTING.md` 中，不再出现模块 id `x_route`、数据路径 `/data/adb/x-route`、包名 `x-route-sukisu`、日志 `xr.log`、`ndc ipfwd enable xroute`、唤醒锁名字 `x-route`（注释里提及「旧版须先卸载」可以只说「旧版」，不必写出旧路径；若必须提到卸载旧数据，用「先在管理器卸载并重启」即可）。
- `module.prop` 为 `id=xrouter`、`name=XRouter`、`version=v3.0.0`、`versionCode=300`。
- `sh scripts/lint.sh` 通过（shellcheck、语法、当时仍存在的单元测试）。
- `bash scripts/package-module.sh --lite-only` 产出 `dist/xrouter-sukisu-lite-v3.0.0.zip`。
- 不提交 `/build/`、`/dist/` 构建产物。
