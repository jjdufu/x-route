# 第 9 步：推送与发预发布

## 目标

全部重构完成且第 8 步测试通过后，把本地 `main` 的全部提交快进推到 `origin/main`，由仓库现有 CI 构建并发布预发布 Release。本地没有未提交改动遗留。

## 依赖

- 第 8 步已通过。未通过不得进入本步。
- 工作分支就是已与 `origin/main` 同步后快进累加的本地 `main`。不得 rebase 成需要 force-push 的历史，不得改写已发布提交。

## 具体做法

1. 发版前检查：
   - `git status` 干净，`git log origin/main..HEAD` 即为第 0–8 步（及修复）的全部提交；
   - `module.prop` 的 `version` / `versionCode` 已更新，`id=xrouter`、`name=XRouter`；
   - 发版标题为干净的 `XRouter <version>`，不含 Build 序号与作者；标签仍为 `<version>-build.<run_number>`；
   - `scripts/package-module.sh` 生成的 Release 说明从上次发版标签以来的 feat/fix 提交提取梗概，并附预发布、全新安装、空密码提示和包名，不要长文；
   - README 同样精简，TESTING 与最终行为一致；
   - 仓库中无密钥、ovpn 私钥、VPN 服务器公网地址。
2. 快进推送：`git push origin main`。禁止 `--force`。禁止删除或移动已有标签 `v2.0.1-build.8` 等。
3. 推送 `main` 且变更落在 `openwrt/**`、`module/**`、`scripts/**` 或工作流文件时，`.github/workflows/build-openwrt.yml` 会构建并创建：
   - tag：`v3.0.0-build.<github.run_number>`（run_number 由 GitHub 分配，不必等于 9）
   - prerelease：true
   - 附件风格与现有一致：full/lite zip、Image、rootfs、qcow2、sha256sums
4. 等待 CI 完成，记录 Release 链接与完整标签名。CI 失败则修复后再次快进推送，仍不强推、不删旧 Release。
5. 不在本地手动 `git tag` 抢发，避免与 CI 规则冲突。若 CI 因路径过滤未触发，使用工作流 `workflow_dispatch`（且必须在 `main` 上）发版，不要把发版接到其它分支。

## 验收标准

- `origin/main` 与本地 `main` 一致，包含全部新提交。
- 新预发布存在，名称体现 XRouter 与 v3.0.0，为 prerelease，旧 Release 与旧标签仍在。
- 附件可下载，命名为 `xrouter-sukisu-*`。
- 向父代理的报告包含：方案文件路径、本地提交 hash 与标题、测试内容与结果、是否已推送与发版、Release 链接与版本号、与方案的偏差、若有阻塞则阻塞点。
