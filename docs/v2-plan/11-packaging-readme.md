# 11 打包、README 与 rc1 发布流程

## `scripts/package-module.sh`（重写）

只生成一个 zip，不再区分 lite 和 full。也不再需要 `META-INF`：SukiSU/KernelSU 不需要 update-binary，第 02 步已经删除。

```bash
#!/usr/bin/env bash
set -euo pipefail
REPO="$(cd "$(dirname "$0")/.." && pwd)"; DIST="${REPO}/dist"
TAG="${1:?用法: package-module.sh <tag>}"           # 例如 v2.0.0-rc1
PROP_VER="$(sed -n 's/^version=//p' "${REPO}/module/module.prop")"
[ "${TAG}" = "${PROP_VER}" ] || { echo "tag ${TAG} 与 module.prop version ${PROP_VER} 不一致"; exit 1; }
for f in Image openwrt-rootfs.raw.gz kernel.config; do [ -s "${DIST}/$f" ] || { echo "缺少 dist/$f"; exit 1; }; done

ST="$(mktemp -d)"; trap 'rm -rf "${ST}"' EXIT
cp -a "${REPO}/module/." "${ST}/"
find "${ST}" -name .gitkeep -delete
mkdir -p "${ST}/data"
cp "${DIST}/Image" "${DIST}/openwrt-rootfs.raw.gz" "${ST}/data/"
chmod 0755 "${ST}"/*.sh "${ST}/scripts/"*.sh "${ST}/scripts/xr"
( cd "${ST}" && zip -r9 -q -X "${DIST}/x-route-${TAG}.zip" . )

( cd "${DIST}" && sha256sum Image openwrt-rootfs.raw.gz "x-route-${TAG}.zip" > sha256sums.txt )

cat > "${DIST}/RELEASE_NOTES.md" <<EOF
## x-route ${TAG}

- ImmortalWrt 25.12.2（armsr/armv8 ImageBuilder），内核 6.12.103 + Gunyah pVM 配置
- 仅支持全新安装：安装前请先在管理器里卸载旧版并重启
- VM 登录：root，空密码（上游默认），可在 LuCI 设置密码

### sha256
\`\`\`
$(cat "${DIST}/sha256sums.txt")
\`\`\`
EOF
echo "[+] ${DIST}/x-route-${TAG}.zip"
```

验收：
- `unzip -l dist/x-route-v2.0.0-rc1.zip` 根目录有 `module.prop`、`customize.sh`、`service.sh`、`action.sh`、`uninstall.sh`、`scripts/`、`webroot/`、`config.conf`、`data/Image`、`data/openwrt-rootfs.raw.gz`；没有 `META-INF`、`keepalive`、`.gitkeep`。
- `sha256sum -c dist/sha256sums.txt` 全部 OK。
- tag 与 `module.prop` 版本不一致时脚本退出，返回值非 0。

## README.md（重写，章节如下）

1. **是什么**：在 Gunyah 受保护 VM 里运行 ImmortalWrt 25.12，给手机和热点设备提供 OpenWrt 网关与 OpenVPN 客户端。
2. **要求**：支持 AVF pVM 的骁龙机型（`/apex/com.android.virt/bin/crosvm` 存在）、SukiSU/KernelSU，`/data` 可用空间至少 1.5GB。
3. **安装**：只支持全新安装。有旧版时先卸载旧版并重启，再刷入。安装后默认不自启，在 WebUI 点“启动”。
4. **首次使用**：
   - 访问 `http://172.29.233.254`；
   - 用户 `root`，空密码（上游默认）；
   - 可在 LuCI → 系统 → 管理权设置密码；
   - 热点客户端默认也能访问，所以务必修改。
5. **网络**：
   - 地址规划：手机侧 `.1`，VM `.254`；
   - `XR_EXPOSE` 三档的含义，`all` 需要上级路由器加静态路由；
   - 蜂窝网络默认不放行。
6. **OpenVPN**：
   - 在 LuCI 导入 .ovpn；
   - `VPN_MODE` terminal 和 site 的区别；
   - 在 WebUI 的 `VPN_ROUTES` 里手动填写对端网段（手机才会把这些网段路由进 VM）；
   - 网段的校验规则。
7. **共享**：默认关闭；ro/rw 的区别；默认路径；rw 下 MediaStore 的说明。
8. **安全措施**：
   - 启动熔断：启动后整机复位会关闭自启，需要在 WebUI 确认；
   - 自启需要先手动成功启动一次；
   - 不启用 balloon、swiotlb 64MB、内存 512MB。
9. **命令行**：`sh /data/adb/modules/x_route/scripts/xr <子命令>` 一览。
10. **构建**：`build-kernel.sh`、`build-rootfs.sh`、`package-module.sh` 三步，需要的依赖与 CI 相同。
11. **日志位置**、**卸载**（会删除 `/data/adb/x-route`，包括系统盘）。
12. **已知限制**：引用 13 中对用户可见的条目。

不写“已真机验证”，不写性能数字，除非已经在第 12 步实测过并附上机型和日期。

## rc1 发布流程（每一步都要等用户确认后再执行）

1. 在 `refactor/v2` 分支按 01→11 完成，每一步单独提交。提交前给用户看 diff，用户确认后才提交。
2. 本地执行 `bash scripts/build-kernel.sh && bash scripts/build-rootfs.sh && bash scripts/package-module.sh v2.0.0-rc1`，确认 03、04、11 的验收都通过。
3. 用户确认后：`git push -u origin refactor/v2`，等 `ci.yml` 通过。
4. 用户确认后：`git tag -a v2.0.0-rc1 -m "x-route v2.0.0-rc1"` 并 `git push origin v2.0.0-rc1`，`release.yml` 会生成**草稿** Release（prerelease）。
5. 下载草稿里的 zip，核对 `sha256sums.txt`，按 12 在真机上测试。
6. 测试结果交给用户，由用户决定：发布草稿、修复后改发 `-rc2`，或者合并进 `main`。合并方式和是否删除旧的 v1 Release 也由用户决定。

整个流程中没有任何自动 publish，草稿 Release 不会通知关注者。
