# 第 7 步：模块 WebUI（Vite + Material Web）

## 架构

- 源码在 `webui/`（`npm`，`package-lock.json` 锁定），参考 bindhosts 与 Tricky Addon：Vite 构建，`@material/web`（Lit）组件，`kernelsu-alt` 的 `exec` 调用 `xr`。
- 构建输出到 `module/webroot`（不入库）；`scripts/package-module.sh` 打包前执行 `npm ci`（无 `node_modules` 时）与 `npm run build`，CI 两个工作流都先安装 Node 与依赖。
- `public/config.json` 供 WebUI X：`title` `icon` `windowResize:false` `exitConfirm:false` `backHandler:true` `backInterceptor:"native"`。
- 图标：Material Symbols Rounded 按 `scripts/icons.txt` 裁剪为本地 `src/assets/symbols.woff2`（`scripts/subset-icons.py`），不走 CDN。
- Material 3 深色配色（`--md-sys-color-*`），不使用系统 `select` / `confirm` / 文件选择。

## 布局

- 顶部固定标题栏：左 XRouter，右状态（已停止 / 运行中 / 异常）。下面是带图标的 Tab：主页、组网、设置；点击或左右滑动只切换内容。
- 主页：圆形电源按钮即状态（停止灰、启动中外圈转动并显示「启动虚拟机 → 网络就绪 → 运行中」、运行中只变绿、失败红色并显示原因）；整行可点的「开机自启」；运行时长、上游网络、打开 LuCI。熔断时显示「整机复位后自启已关闭」与确认。
- 组网：OpenVPN 卡片：连接状态、路由网段、导入配置（文件浏览只显示 `.ovpn` / `.conf`）。
- 日志：不单独成页、不分类，只分普通 / 调试。设置里的「日志」分组：调试模式开关、自动关闭时间、导出按钮（调试关闭时「日志导出」，开启时「诊断导出」，均导出为带日期时间的 .tar.gz 到 Download）。
- 设置：网络（运行模式、管理访问、IPv6）、存储共享（开关、只读 / 读写、目录）、虚拟机资源（CPU、内存）、系统盘（容量、扩容、重置）。

## 交互

- 需要确认：切换运行模式、停止虚拟机、扩容系统盘（选择目标大小）、重置系统盘。其他设置立即生效；需重启虚拟机才生效的项提示「重启虚拟机后生效」。
- 读写共享在 crosvm 不支持时置灰。IPv6 开关读取虚拟机实际状态（`status-json` 的 `ipv6`），虚拟机未就绪时置灰。
- 目录浏览经 `xr ls`（限定内部存储，隐藏项不列），从「内部存储」开始，路径可点击跳转，底部「选择此文件夹」。

## 后端

`xr` 为界面新增：`ls`、`vpn-import`、`log-clear`、`luci`、`ipv6`；`vm.status` 增加 `error`（启动失败原因），`net.status` 增加 `gw`（`off` / `block` / `v6`）。

## 验收

`tests/test-webui.sh`：Tab 与文案、旧文案不残留、可见文案无内部变量、取值与 `valid_kv` 一致、确认范围、元素 id / 图标 / `xr` 子命令一致、纯函数（状态推导、路径、扩容选项）、Vite 构建产物（含 `config.json`、`icon.png`、本地字体）。
