#!/usr/bin/env bash
# ==============================================================================
# XRouter: 打包 SukiSU / KernelSU 模块（docs/v2-plan/11-packaging-readme.md）
#   bash scripts/package-module.sh              -> full + lite + sha256sums + RELEASE_NOTES
#   bash scripts/package-module.sh --lite-only  -> 只打 lite（不需要固件，供分支 CI 检查）
# 产物：
#   dist/xrouter-sukisu-full-<ver>.zip   模块 + data/Image + data/openwrt-rootfs.img.gz
#   dist/xrouter-sukisu-lite-<ver>.zip   只有模块脚本与 WebUI（使用 /data/adb/xrouter/images 中已有镜像）
#   dist/openwrt/sha256sums.txt          所有发布文件的 sha256
#   dist/RELEASE_NOTES.md                Release 正文（与历史发版格式一致）
# ==============================================================================
set -euo pipefail
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DIST="${REPO}/dist"; OW="${DIST}/openwrt"
LITE_ONLY=0; [ "${1:-}" = "--lite-only" ] && LITE_ONLY=1

prop() { sed -n "s/^$1=//p" "${REPO}/module/module.prop" | tr -d '\r'; }
VER="$(prop version)"; NAME="$(prop name)"; AUTHOR="$(prop author)"
[[ "${VER}" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "module.prop version 不合法：${VER}"; exit 1; }
mkdir -p "${DIST}"

# WebUI：webui/（Vite + Material Web）构建到 module/webroot，产物不入库
command -v npm >/dev/null 2>&1 || { echo "需要 Node.js / npm 构建 WebUI"; exit 1; }
( cd "${REPO}/webui" && { [ -d node_modules ] || npm ci --no-fund --no-audit; } && npm run build )
for f in index.html config.json icon.png; do
  [ -s "${REPO}/module/webroot/${f}" ] || { echo "WebUI 构建产物缺少 ${f}"; exit 1; }
done

ST="$(mktemp -d)"; trap 'rm -rf "${ST}"' EXIT
cp -a "${REPO}/module/." "${ST}/"
find "${ST}" -name .gitkeep -delete
rm -rf "${ST}/data"
chmod 0755 "${ST}"/*.sh "${ST}/scripts/"*.sh "${ST}/scripts/xr"
[ ! -e "${ST}/META-INF" ] || { echo "不应包含 META-INF"; exit 1; }

zipdir() { ( cd "$1" && rm -f "$2" && find . -type f | LC_ALL=C sort | sed 's#^\./##' | zip -9 -q -X "$2" -@ ); }

LITE="${DIST}/xrouter-sukisu-lite-${VER}.zip"
zipdir "${ST}" "${LITE}"
echo "[+] ${LITE}"
[ "${LITE_ONLY}" = 1 ] && exit 0

for f in Image openwrt-rootfs.img.gz openwrt.qcow2 kernel.config; do
  [ -s "${OW}/$f" ] || { echo "缺少 ${OW}/$f（先运行 build-kernel.sh 与 build-rootfs.sh）"; exit 1; }
done
FULL="${DIST}/xrouter-sukisu-full-${VER}.zip"
mkdir -p "${ST}/data"
cp "${OW}/Image" "${OW}/openwrt-rootfs.img.gz" "${ST}/data/"
zipdir "${ST}" "${FULL}"
echo "[+] ${FULL}"

( cd "${OW}" && sha256sum Image openwrt-rootfs.img.gz openwrt.qcow2 ) > "${OW}/sha256sums.txt"
( cd "${DIST}" && sha256sum "${FULL##*/}" "${LITE##*/}" ) >> "${OW}/sha256sums.txt"

KREL="$(sed -n 's/^# Linux\/arm64 \([0-9.]*\) Kernel Configuration$/\1/p' "${OW}/kernel.config" | head -1)"
cat > "${DIST}/RELEASE_NOTES.md" <<EOF
### ${NAME} \`${VER}\`（作者：\`${AUTHOR}\`）
- **预发布，尚未真机测试**：这是破坏性更新（产品更名为 XRouter），目前只通过了本机单元测试（mksh / busybox sh / dash）和 QEMU \`virt\` 启动冒烟测试，真机验证仍待进行（见 \`docs/TESTING.md\`）。
- **完整模块**：\`${FULL##*/}\`（含 WebUI、\`xr\` 命令行，以及预置 \`Image\` 与 \`openwrt-rootfs.img.gz\`）
- **精简模块**：\`${LITE##*/}\`（仅脚本和 WebUI，使用 \`/data/adb/xrouter/images\` 中已有镜像）
- **固件**：ImmortalWrt 25.12.2（\`armsr/armv8\` ImageBuilder，\`apk\`），Linux \`${KREL:-6.12.103}\` + Gunyah pVM；官方 25.12 的 \`kmod-*\` 可用 \`apk\` 安装。主机名 \`XRouter\`，概览型号为 \`AMD Ryzen Threadripper PRO 9995WX\`。预装包管理简体中文语言包。
- **安全默认**：
  - 安装后不自启（\`AUTOSTART="0"\`）；需用同一配置手动启动成功一次后，才允许打开自启
  - \`--no-balloon --swiotlb 64 --mem 512\`，\`ulimit -l unlimited\`，以及启动熔断（启动过程中若整机复位，将关闭自启，需在 WebUI 确认后才能再启动）
- **WebUI**：Vite + Material Web 重做；顶部标题栏固定，概览 / 组网 / 日志 / 设置四个 Tab 可点可滑；圆形电源按钮即状态；运行模式「全局接管 / 旁路网关」，管理访问「本机 / 内网 / 所有」（默认内网）；目录与 OpenVPN 配置通过内置文件夹浏览选择
- **IPv6**：固件默认关闭（组件保留）；设置中打开后立即生效，全局接管下手机与热点的 IPv6 同样经虚拟机出网，关闭时直接拒绝、应用立即回落 IPv4
- **网络**：\`172.29.233.0/24\`（手机 \`xr0\` 为 \`.1\`，虚拟机 \`eth0\` 为 \`.254\`）；LuCI \`http://172.29.233.254\`
- **运行模式**：全局时，手机本机和热点上网先经 OpenWrt 再从当时 WAN 出去；旁路不接管。虚拟机未就绪时不接管。
- **WAN**：手机当前上游（Wi-Fi 或蜂窝，含 464xlat 的 \`v4-*\`）作为虚拟机的 WAN：放行出站与回包，丢弃主动入站；不改写虚拟机发出的包；仅对 \`172.29.233.0/24\` 做 MASQUERADE
- **OpenVPN**：可在组网页或 LuCI 导入启用（导入不重启虚拟机），连上后自动同步服务器 push 的路由（不同步默认路由）；本网段出隧道不做 NAT
- **透明代理内核**：netfilter / nftables / TUN / inet_diag 划分与官方 25.12.2 内核一致（\`=m\`）；预装 \`kmod-nft-tproxy\`、\`kmod-nft-socket\`、\`kmod-nft-fullcone\`、\`kmod-inet-diag\`
- **登录**：用户 \`root\`，**空密码**（上游默认），请在 LuCI → 系统 → 管理权 中设置
- **仅支持全新安装**：请先在管理器卸载旧版并重启，不做旧配置迁移
EOF
echo "[+] ${DIST}/RELEASE_NOTES.md"
