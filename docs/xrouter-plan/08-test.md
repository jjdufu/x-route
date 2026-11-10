# 第 8 步：测试并修复

## 目标

在推送和发版之前，把能在本机运行的检查全部跑过，修复失败项，再单独提交测试与修复（若第 7 步提交时测试已全绿且本步无需改代码，则本步只允许补测试或文档中的测试记录，不得空提交）。

不得编造测试结果。无法运行的项目必须如实记录原因。

## 依赖

- 第 1–7 步代码均已本地提交。
- 固件源文件已在第 6 步改完。若要跑 QEMU，需先成功构建 rootfs（内核已有则可复用 `dist/openwrt/Image`）。

## 要做的测试

在仓库根目录执行，并保留输出：

1. `sh scripts/lint.sh`  
   必须在 mksh、busybox sh、dash 可用时跑完全部 `tests/test-*.sh`。本机缺某种 shell 则跳过并记录。
2. `sh scripts/check-config-keys.sh`（lint 已含则不必重复，结果一致即可）。
3. `bash scripts/package-module.sh --lite-only`，检查 zip 内为新模块 id、新脚本路径、无旧路径、无 META-INF。
4. WebUI：`node` 可用时 `tests/test-webui.sh` 必须通过。
5. 若具备 `qemu-system-aarch64` 且能构建或已有**包含第 4–6 步改动**的 `dist/openwrt/Image` 与 `openwrt-rootfs.img.gz`：运行 `sh tests/e2e-qemu.sh`。用旧镜像冒充新固件验收视为无效。构建失败或环境不足则跳过 QEMU，不记为通过。
6. 针对性回归（由单元测试覆盖，失败则修代码）：
   - 卸载后数据目录、规则、通知、pid 不残留（在 mock 环境）；
   - `xr start` 走守护进程 / setsid，不把 crosvm 留在测试进程组（能模拟则必须测）；
   - 运行模式全局 / 旁路的规则差异、未就绪不接管、`iif xr0` 不回灌；
   - VPN 路由来自 guest 文件而非 ovpn 文本；默认路由不同步；
   - 共享 `/sdcard` 规范到 `/data/media/0`；
   - 日志轮转不写到 `/sdcard`；导出文件名。

## 修复原则

- 先修测试揭示的产品缺陷，再改测试去适配已经认同的方案。不得为了变绿而削弱验收（例如删掉防环路断言）。
- 修复属于某一步回归的，提交说明写清原因；本步可以有一个或若干个「测试与修复」commit，但在进入第 9 步前必须全部提交在本地 `main`。

## 验收标准

- `lint.sh` 全绿。
- 本步实际跑过的命令、环境、结果写入提交说明或 `docs/TESTING.md` 的简短更新（不要写工期）。未跑的明确写「未跑」和原因。
- 没有用旧 rootfs 声称第 6 步已在 QEMU 通过。
