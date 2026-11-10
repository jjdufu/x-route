# 第 2 步：彻底的卸载脚本

## 目标

在管理器中卸载模块时，与本模块有关的一切都清掉，包括正在运行的虚拟机、守护进程、通知、网络规则、唤醒锁、数据目录（含日志、配置、镜像、系统盘、运行时文件）。卸载后不留下可再被误用的残留。

不做「保留系统盘」「保留配置」之类选项。

## 依赖

- 依赖第 1 步新路径与新标识。
- 第 3 步会增加守护进程与通知，本步先把卸载框架写完整，并对这些对象预留清理（通知 tag、守护 pid 文件采用本方案约定名）。第 3 步不得改用另一套未在卸载中清理的名字；若第 3 步新增了文件或进程，必须同步补进卸载脚本并在第 3 步提交中改测试。

## 要改的文件

- `module/uninstall.sh`（重写）
- `tests/test-install.sh`（加强卸载断言）
- 如需把清理逻辑复用，可抽到 `module/scripts/common.sh` 或 `module/scripts/cleanup.sh`，但卸载脚本即使在 `xr` 已不可用时也必须能完成清理（ Magisk/KSU 调用 `uninstall.sh` 时模块目录通常还在，但要容错）。

## 具体做法

`uninstall.sh` 按下面顺序执行，每一步都容错（失败继续），最后确保数据目录不存在。不要依赖 WebUI，不要弹出确认（系统卸载本身就是确认）。

1. **停止业务进程**
   - 若存在 `scripts/xr`，调用 `xr stop`（或等价的停止函数）。
   - 读取并结束 `${RUN}` 下记录的 pid：`crosvm.pid`、`fuse_watch.pid`、`net_watch.pid`、`daemon.pid`（第 3 步的守护进程），以及之后新增的同类 pid 文件。
   - 若 sock 还在，尝试 `crosvm stop`。
   - 对仍在运行、命令行含本模块镜像路径或 `${XR_DATA}` 的残留 `crosvm` 发 TERM，等待后再 KILL。
   - 不要 `kill -9 -1`，不要误杀无关进程。

2. **撤销网络**
   - 调用 `net.sh down`（若脚本存在）。
   - 无论脚本是否成功，再做一遍兜底：
     - 删除 `FORWARD`/`POSTROUTING` 上对 `XR_FWD`/`XR_NAT` 的跳转，清空并删除这两条链；
     - `ndc ipfwd disable xrouter`，撤销 `ndc ipfwd add xr0 …`、`ndc network route/interface` 中与 `xr0`、本网段有关的登记；
     - 删除本模块添加的 `ip rule`（pref 4800、4801、18900，以及第 4 步将使用的全局网关 pref，见 04，本步先按约定 pref 清理；未知 pref 不要扫掉系统规则）；
     - 清空 table 97，以及第 4 步约定的网关表；
     - `ip link del xr0`；
     - 若曾备份 `ip_forward`，能恢复则恢复，不能则不要强行改成 0。

3. **通知与唤醒锁**
   - 取消本模块发出的通知（约定 tag：`xrouter`，见第 3 步）。`cmd notification cancel` 或等价命令，失败忽略。
   - `echo xrouter > /sys/power/wake_unlock`，失败忽略。若旧唤醒锁名仍偶发存在，也可顺带解锁（这是清理残留，不是兼容运行）。

4. **删除数据**
   - `rm -rf /data/adb/xrouter`（或 `$XR_DATA`，以实际运行值为准），包括：
     `config/`、`images/`（含 `Image`、系统盘、`openwrt-rootfs.img.gz`）、`log/`（`module.log`、`module.log.1`、`vmm.log`、`vmm.log.1` 等）、`run/`（串口日志、pid、sock、控制通道文件）。
   - 不删除用户主动导出到 Download 的 `xrouter-logs-*.zip`（那是用户文件，不是模块运行时数据）。
   - 模块目录本身由管理器删除，脚本不必 `rm -rf $MODDIR`；可删除 `$MODDIR` 下脚本生成的临时文件。

5. **旧数据残留**
   - 若磁盘上还留有已不再使用的 `/data/adb/x-route`，卸载时一并 `rm -rf`。这是清残留，安装脚本不得读取或迁移其中的配置。
   - 旧唤醒锁名、旧 ipfwd 名若存在，能拆则拆。

6. **同步**
   - 末尾 `sync`。

安装脚本（`customize.sh`）保持全新安装语义：不迁移旧配置。不在安装时自动删除用户尚未卸载的旧数据，以免误伤；卸载才是彻底删除的入口。

## 验收标准

- 单元测试覆盖：在假环境中安装后写入配置、日志、pid 占位文件、镜像，执行 `uninstall.sh`，断言数据目录不存在，并且调用过 stop / net down（可用已有假 `getprop` 环境扩展 `tests/test-install.sh`）。
- 脚本对「xr 失败」「网卡已不存在」「链不存在」「pid 已死」不报致命错误，最终仍删除数据目录，退出码为 0。
- 不删除 `/data/media/0/Download` 下的导出包（测试中可放一个假 zip，断言还在）。
- `sh scripts/lint.sh` 通过。
