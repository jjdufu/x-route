# 第 3 步：虚拟机与 WebUI 解绑、独立保活

## 目标

页面只负责发出启动 / 停止和查看状态。关掉 WebUI、上滑离开、管理器进程被回收后，虚拟机必须继续运行。启动进程不得挂在 KernelSU WebUI 的 `ksu.exec` 进程组上。

保活由独立守护进程承担，并在虚拟机运行期间常驻一条前台风格的通知（观感接近常见的模块常驻通知：持续显示、可点开管理器，不是系统默认的一次性 toast）。

## 依赖

- 依赖第 1 步路径与第 2 步卸载清理（本步若新增 pid / 通知，必须写入卸载脚本）。
- 本步不实现全局网关，也不改 WebUI 分页。

## 背景与约束

当前 `vm_start` 用 `nohup crosvm … &` 在调用方 shell 里拉起进程。WebUI 通过 `ksu.exec` 运行 `xr start`。部分管理器会在页面销毁时结束该 exec 的进程组，子进程会被一起带走。日志已表明「离开页面时 WebUI 代码不会主动调 stop」，因此要防的是被外部一并清掉，而不是页面里误点停止。

本模块没有独立 APK，做不到系统意义上的 `startForegroundService`。采用「独立 session 的守护进程 + 持续通知 + 已有唤醒锁」达到同等保活目标，并在实现注释中如实写明。

## 要改的文件

- 新增 `module/scripts/daemon.sh`（守护进程）
- `module/scripts/vm.sh`、`module/scripts/xr`、`module/service.sh`、`module/action.sh`
- `module/uninstall.sh`（补齐守护进程与通知）
- 测试：`tests/test-vm.sh`、`tests/test-xr.sh`、`tests/test-install.sh`，必要时新增 `tests/test-daemon.sh`

## 具体做法

### 1. 守护进程

`service.sh` 在开机完成、模块未被 disable/remove 后，**无论 AUTOSTART 是否为 1**，都以新会话拉起守护进程（若已在运行则不重复拉起）。熔断逻辑仍在服务脚本里：若存在 fuse 则置 tripped、关自启、不自动 `start`，但守护进程本身要起来，以便用户稍后从页面手动启动。

拉起方式必须脱离调用方进程组，例如：

```sh
setsid nohup sh "${MODDIR}/scripts/daemon.sh" >/dev/null 2>&1 < /dev/null &
```

把守护进程 pid 写入 `${RUN}/daemon.pid`。守护进程启动后立即把自身 pid 再写一次，并忽略 SIGHUP。

守护进程职责：

- 循环等待 `${RUN}/daemon.cmd`（或 FIFO）。命令为 `start`、`stop`、`ping`。
- `start` / `stop` 在守护进程内调用 `vm_start` / `vm_stop`，因此 crosvm、`fuse_watch`、`net_watch` 都是守护进程的子进程，与 WebUI 无关。
- 定期检测自身拉起的 crosvm 是否还在；若 VM 在运行则保持通知为「运行中」，否则「已停止」或撤掉运行态文案。
- 守护进程自己挂了时，不自动把 VM 杀掉（若 VM 是独立 session 也可；为简单起见，crosvm 作为守护进程子进程时，守护进程应尽量不退出）。`service.sh` 不做复杂的 systemd 式重启即可，但 `xr start` 发现守护进程不在时必须先用 `setsid` 拉起再下发 `start`。

### 2. CLI 与 WebUI 的调用方式

`xr start`：

1. 若守护进程未运行，用 `setsid` 拉起。
2. 向 `daemon.cmd` 写入 `start` 并通知守护进程（如 `kill -USR1` 或 FIFO）。
3. 立即返回，不要在 `ksu.exec` 的 shell 里直接 `nohup crosvm`。
4. 等待短暂时间，确认 crosvm pid 已出现或启动失败原因已写入日志，再以原有退出码语义返回（已在运行视为成功）。

`xr stop`：同样只向守护进程下发 `stop`，由守护进程执行 `vm_stop`。若守护进程不在但 crosvm 仍在，CLI 做一次兜底停止。

`action.sh` 只调用 `xr`，不直接起 crosvm。

`vm.sh` 若被直接执行 `start`，也应走到同一套「交给守护进程」的路径，避免测试与手工调用再出现两套生命周期。单元测试里若用假 crosvm，可为测试提供「前台模式」环境变量（例如 `XR_DAEMON_FOREGROUND=1` 时不 setsid、同步执行），但默认路径必须是守护进程。不要引入长期兼容旧的「直接 nohup」生产路径。

### 3. 启动进程的隔离

在守护进程内启动 crosvm 时仍使用：

- `setsid`（或已在新 session 中则不必重复，但要保证不继承 WebUI 的 pgid）
- `nohup` 与重定向
- 已有的 `ulimit -l unlimited` 与唤醒锁

禁止：从 WebUI 调用链上的 shell 直接后台起 crosvm 而不切 session。

### 4. 常驻通知

在 Android 上用 `cmd notification post`（或设备上实际可用的等价命令）：

- tag 固定为 `xrouter`（卸载时按此取消）
- `--ongoing`（若该系统支持）表示持续通知
- 标题：`XRouter`
- 运行中正文：`运行中`
- 已停止且守护仍在：可不显示，或显示 `已停止`；虚拟机运行期间必须在。推荐：仅在 VM 运行时发送持续通知，停止后取消，避免平时占状态栏。
- 不要用系统 `confirm` / toast 代替这条通知。
- 图标若命令支持再设，不依赖自定义 APK 资源。
- 命令不存在或失败时记一条日志，不影响 VM 运行。

### 5. 开机自启

AUTOSTART=1 且通过熔断 / last_good 检查时，`service.sh` 在延迟（固定 60 秒，若配置键仍在则本步可继续读取，第 4 步删键后改硬编码）且 CE 就绪后，向守护进程发 `start`，而不是自己再起一套 vm_start。

### 6. 卸载

停止守护进程，取消 `xrouter` 通知，再走第 2 步的其余清理。

## 验收标准

- 单元测试：`xr start` 会拉起守护进程（或向已有守护进程发命令），测试进程本身不是 crosvm 的父进程；`xr stop` 后 crosvm 与通知取消逻辑被调用。
- 用假 crosvm 验证：启动后杀死「模拟 ksu.exec 的父进程 / 进程组」时，crosvm 仍在（在 Linux 测试机上用 setsid 场景模拟，能测则必须测）。
- 守护 pid 写入 `daemon.pid`；卸载测试断言会杀掉该 pid 并取消通知。
- `vm.sh` 原有熔断、就绪、稳定、扩容、重置行为不变。
- `sh scripts/lint.sh` 通过。
