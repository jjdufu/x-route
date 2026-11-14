//! 运行环境：模块目录、数据目录、系统命令与 `/proc` 等路径的来源（环境变量或默认值），以及由数据目录派生的各文件路径。
//!
//! 测试通过环境变量把这些路径指向临时目录，不碰真实系统。

use std::env;
use std::path::{Path, PathBuf};

use crate::traffic::SNAP_DEFAULT;

#[cfg(test)]
mod tests;

/// `xr0` 上客机 `IPv6` ULA 地址的固定前缀（`fd7a:7872:e9`）。
pub(crate) const XR_NET6: &str = "fd7a:7872:e9";
/// 手机与虚拟机之间的主网卡（tap）名。
pub(crate) const XR_IF: &str = "xr0";
/// 热点桥接时虚拟机侧使用的第二块 tap 网卡名。
pub(crate) const HS_TAP: &str = "xr1";
/// 热点接管时创建的网桥名。
pub(crate) const HS_BR: &str = "xrbr1";

/// 一次 `xr` 调用的运行环境：所有外部路径、等待时长与进程角色。
///
/// 由 [`Env::from_process`] 从环境变量构造；字段公开，便于集成测试直接改写成临时目录。
#[derive(Debug, Clone)]
pub struct Env {
    /// 模块安装目录（`MODDIR`，默认 `/data/adb/modules/xrouter`）。
    pub moddir: PathBuf,
    /// 数据目录（`XR_DATA`，默认 `/data/adb/xrouter`），其下有 `config`、`run`、`log`、`images`。
    pub xr_data: PathBuf,
    /// `busybox` 路径（`BB`，默认 `/data/adb/ksu/bin/busybox`）。
    pub bb: PathBuf,
    /// 内部存储根目录（`XR_MEDIA` 或 `MEDIA`，默认 `/data/media/0`）。
    pub media: PathBuf,
    /// 共享目录读写模式下附加给 `crosvm --shared-dir` 的 uid/gid 映射选项（`RW_MAP_OPT`，空表示不支持读写共享）。
    pub rw_map_opt: String,
    /// 系统命令目录（`XR_SYSBIN`，默认 `/system/bin`），`ip`、`iptables`、`ndc` 等从这里取。
    pub sysbin: PathBuf,
    /// 写 `sys/net/...` 内核参数时使用的 `/proc` 根（`XR_PROC`，默认 `/proc`）。
    pub proc_dir: PathBuf,
    /// 读进程信息（`<pid>/stat`、`cmdline`、`cgroup`）时使用的 `/proc` 根（`XR_PROC_BASE`，默认 `/proc`）。
    pub proc_base: PathBuf,
    /// 系统运行时长文件（`XR_PROC_UPTIME`，默认 `/proc/uptime`）。
    pub proc_uptime: PathBuf,
    /// 挂载表文件（`XR_PROC_MOUNTS`，默认 `/proc/mounts`）。
    pub proc_mounts: PathBuf,
    /// 网卡信息目录（`XR_SYSNET`，默认 `/sys/class/net`）。
    pub sysnet: PathBuf,
    /// cgroup 根目录（`XR_CGROUP_ROOT`，默认 `/sys/fs/cgroup`）。
    pub cgroup_root: PathBuf,
    /// `crosvm` 可执行文件（`CROSVM`，默认 `/apex/com.android.virt/bin/crosvm`）。
    pub crosvm: PathBuf,
    /// Java 通知器 jar（`XR_NOTIFIER_JAR`，默认 `<模块目录>/bin/xrnotify.jar`）。
    pub notifier_jar: PathBuf,
    /// 运行通知器用的 `app_process`（`XR_APP_PROCESS`，默认 `<系统命令目录>/app_process`）。
    pub app_process: PathBuf,
    /// 测试用：覆盖屏幕亮灭状态（`XR_SCREEN`，`on` 为亮屏），未设置时读 `dumpsys power`。
    pub screen_override: Option<String>,
    /// 测试用：覆盖导出文件的下载目录（`XR_DOWNLOAD`），未设置时用内部存储的 `Download`。
    pub download_override: Option<PathBuf>,
    /// 模块更新暂存目录（`XR_MODULES_UPDATE`，默认 `/data/adb/modules_update`）。
    pub modules_update: PathBuf,
    /// 卸载暂存目录的首选父目录（`XR_TMPDIR`），未设置或不可用时退回 `/data/local/tmp`。
    pub tmpdir: Option<PathBuf>,
    /// 设置 `XR_HS_ANY` 时允许把非 Wi-Fi 接口也当作热点上游接管（测试用）。
    pub hs_any: bool,
    /// 等待上游 `IPv6` 地址的秒数（`XR_IPV6_WAIT`，默认 20）。
    pub ipv6_wait: u64,
    /// 等待客机 VPN 就绪的秒数（`XR_VPN_WAIT`，默认 20）。
    pub vpn_wait: u64,
    /// 等待通知器应答的秒数（`XR_NOTIFY_WAIT`，默认 25）。
    pub notify_wait: u64,
    /// 通知器轮询间隔秒数（`XR_NOTIFY_INTERVAL`，默认 10）。
    pub notify_interval: u64,
    /// 看门狗检查间隔秒数（`XR_WATCHDOG_INTERVAL`，默认 10）。
    pub watchdog_interval: u64,
    /// 流量快照落盘的增量阈值字节数（`XR_SNAP_MIN_BYTES`，默认 64 MiB；仅供测试覆盖）。
    pub snap_min_bytes: u64,
    /// 流量快照有变化时的最长落盘间隔秒数（`XR_SNAP_MAX_AGE`，默认 1800；仅供测试覆盖）。
    pub snap_max_age: u64,
    /// 本进程角色（由 `XR_IN_DAEMON`、`XR_IN_WATCHDOG` 得出）。
    pub role: ProcRole,
    /// 守护进程运行方式（由 `XR_DAEMON_FOREGROUND` 得出）。
    pub daemon_mode: DaemonMode,
    /// 测试用：`XR_FAKE_DATE_FAIL=1` 时模拟取本地时间失败，走时间戳的退路分支。
    pub fake_date_fail: bool,
}

/// 当前 `xr` 进程的角色：普通命令行、守护进程或看门狗（三者互斥）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcRole {
    /// 普通命令行调用。
    Cli,
    /// `xr internal daemon` 守护进程。
    Daemon,
    /// `xr internal watchdog` 看门狗。
    Watchdog,
}

impl ProcRole {
    /// 由 `XR_IN_DAEMON`、`XR_IN_WATCHDOG` 两个环境变量的值得到角色：值为 `"1"` 视为设置；
    /// 两者都设置时取守护进程（与旧代码先判断守护进程标志一致）。
    #[must_use]
    pub(crate) fn from_values(in_daemon: Option<&str>, in_watchdog: Option<&str>) -> Self {
        match (in_daemon == Some("1"), in_watchdog == Some("1")) {
            (true, _) => Self::Daemon,
            (false, true) => Self::Watchdog,
            (false, false) => Self::Cli,
        }
    }

    /// 是否为守护进程。
    #[must_use]
    pub(crate) const fn is_daemon(self) -> bool {
        matches!(self, Self::Daemon)
    }

    /// 是否为看门狗。
    #[must_use]
    pub(crate) const fn is_watchdog(self) -> bool {
        matches!(self, Self::Watchdog)
    }
}

/// 守护进程的运行方式：`XR_DAEMON_FOREGROUND=1`（测试用）时在前台同步执行，否则交给后台守护进程。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonMode {
    /// 正常部署：命令交给后台守护进程。
    Detached,
    /// 前台模式：在当前进程内直接执行。
    Foreground,
}

impl DaemonMode {
    /// 由 `XR_DAEMON_FOREGROUND` 的值得到运行方式：`"1"` 为前台，其余（含未设置）为后台。
    #[must_use]
    pub(crate) fn from_value(foreground: Option<&str>) -> Self {
        if foreground == Some("1") {
            Self::Foreground
        } else {
            Self::Detached
        }
    }

    /// 是否为前台模式。
    #[must_use]
    pub(crate) const fn is_foreground(self) -> bool {
        matches!(self, Self::Foreground)
    }
}

/// 读环境变量 `key`，未设置或为空时返回 `None`。
fn var_nonempty(key: &str) -> Option<String> {
    env::var(key).ok().filter(|value| !value.is_empty())
}

/// 读环境变量 `key` 并解析为 `u64`，未设置、为空或解析失败时返回 `default`。
fn var_u64(key: &str, default: u64) -> u64 {
    var_nonempty(key)
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(default)
}

/// 读环境变量 `key` 作为路径，未设置或为空时取 `default`。
fn path_var(key: &str, default: &str) -> PathBuf {
    PathBuf::from(var_nonempty(key).unwrap_or_else(|| default.to_owned()))
}

impl Env {
    /// 从当前进程的环境变量构造运行环境，未设置的项取真机默认路径。
    pub fn from_process() -> Self {
        let moddir = path_var("MODDIR", "/data/adb/modules/xrouter");
        let sysbin = path_var("XR_SYSBIN", "/system/bin");
        let notifier_jar = var_nonempty("XR_NOTIFIER_JAR")
            .map_or_else(|| moddir.join("bin/xrnotify.jar"), PathBuf::from);
        let app_process = var_nonempty("XR_APP_PROCESS")
            .map_or_else(|| sysbin.join("app_process"), PathBuf::from);

        Self {
            moddir,
            xr_data: path_var("XR_DATA", "/data/adb/xrouter"),
            bb: path_var("BB", "/data/adb/ksu/bin/busybox"),
            media: PathBuf::from(
                var_nonempty("XR_MEDIA")
                    .or_else(|| var_nonempty("MEDIA"))
                    .unwrap_or_else(|| "/data/media/0".to_owned()),
            ),
            rw_map_opt: env::var("RW_MAP_OPT").unwrap_or_default(),
            sysbin,
            proc_dir: path_var("XR_PROC", "/proc"),
            proc_base: path_var("XR_PROC_BASE", "/proc"),
            proc_uptime: path_var("XR_PROC_UPTIME", "/proc/uptime"),
            proc_mounts: path_var("XR_PROC_MOUNTS", "/proc/mounts"),
            sysnet: path_var("XR_SYSNET", "/sys/class/net"),
            cgroup_root: path_var("XR_CGROUP_ROOT", "/sys/fs/cgroup"),
            crosvm: path_var("CROSVM", "/apex/com.android.virt/bin/crosvm"),
            notifier_jar,
            app_process,
            screen_override: var_nonempty("XR_SCREEN"),
            download_override: var_nonempty("XR_DOWNLOAD").map(PathBuf::from),
            modules_update: path_var("XR_MODULES_UPDATE", "/data/adb/modules_update"),
            tmpdir: var_nonempty("XR_TMPDIR").map(PathBuf::from),
            hs_any: var_nonempty("XR_HS_ANY").is_some(),
            ipv6_wait: var_u64("XR_IPV6_WAIT", 20),
            vpn_wait: var_u64("XR_VPN_WAIT", 20),
            notify_wait: var_u64("XR_NOTIFY_WAIT", 25),
            notify_interval: var_u64("XR_NOTIFY_INTERVAL", 10),
            watchdog_interval: var_u64("XR_WATCHDOG_INTERVAL", 10),
            snap_min_bytes: var_u64("XR_SNAP_MIN_BYTES", SNAP_DEFAULT.min_bytes),
            snap_max_age: var_u64("XR_SNAP_MAX_AGE", SNAP_DEFAULT.max_age),
            role: ProcRole::from_values(
                env::var("XR_IN_DAEMON").ok().as_deref(),
                env::var("XR_IN_WATCHDOG").ok().as_deref(),
            ),
            daemon_mode: DaemonMode::from_value(env::var("XR_DAEMON_FOREGROUND").ok().as_deref()),
            fake_date_fail: env::var("XR_FAKE_DATE_FAIL").as_deref() == Ok("1"),
        }
    }

    /// 配置目录 `<数据目录>/config`。
    #[must_use]
    pub(crate) fn config_dir(&self) -> PathBuf {
        self.xr_data.join("config")
    }

    /// 配置文件 `<数据目录>/config/config.conf`。
    #[must_use]
    pub fn conf_file(&self) -> PathBuf {
        self.xr_data.join("config/config.conf")
    }

    /// VPN 配置目录 `<数据目录>/config/vpn`。
    #[must_use]
    pub(crate) fn vpn_dir(&self) -> PathBuf {
        self.xr_data.join("config/vpn")
    }

    /// 运行时目录 `<数据目录>/run`（pid 文件、锁、开机日志等），开机时清空。
    #[must_use]
    pub fn run_dir(&self) -> PathBuf {
        self.xr_data.join("run")
    }

    /// 实时速率服务的套接字 `<数据目录>/run/stats.sock`。
    #[must_use]
    pub(crate) fn stats_sock(&self) -> PathBuf {
        self.run_dir().join("stats.sock")
    }

    /// 日志目录 `<数据目录>/log`。
    #[must_use]
    pub fn log_dir(&self) -> PathBuf {
        self.xr_data.join("log")
    }

    /// 镜像目录 `<数据目录>/images`。
    #[must_use]
    pub(crate) fn img_dir(&self) -> PathBuf {
        self.xr_data.join("images")
    }

    /// `OpenWrt` 根文件系统镜像 `<数据目录>/images/openwrt-rootfs.img`。
    #[must_use]
    pub(crate) fn rootfs_img(&self) -> PathBuf {
        self.xr_data.join("images/openwrt-rootfs.img")
    }

    /// 模块主日志 `<数据目录>/log/module.log`。
    #[must_use]
    pub fn module_log(&self) -> PathBuf {
        self.xr_data.join("log/module.log")
    }

    /// 调试日志 `<数据目录>/log/debug.log`。
    #[must_use]
    pub(crate) fn debug_log(&self) -> PathBuf {
        self.xr_data.join("log/debug.log")
    }

    /// `crosvm` 输出日志 `<数据目录>/log/vmm.log`。
    #[must_use]
    pub fn vmm_log(&self) -> PathBuf {
        self.xr_data.join("log/vmm.log")
    }

    /// 本次开机进度日志 `<数据目录>/run/boot.log`（存在时才写）。
    #[must_use]
    pub fn boot_log(&self) -> PathBuf {
        self.xr_data.join("run/boot.log")
    }

    /// 虚拟机串口控制台日志 `<数据目录>/run/console.log`。
    #[must_use]
    pub fn console_log(&self) -> PathBuf {
        self.xr_data.join("run/console.log")
    }

    /// 虚拟机控制命令日志 `<数据目录>/run/vm-ctl.log`。
    #[must_use]
    pub(crate) fn ctl_log(&self) -> PathBuf {
        self.xr_data.join("run/vm-ctl.log")
    }

    /// 启动保险丝文件 `<数据目录>/config/vm_booting.fuse`：启动期间存在，连续启动失败时据此熔断。
    #[must_use]
    pub(crate) fn fuse_file(&self) -> PathBuf {
        self.xr_data.join("config/vm_booting.fuse")
    }

    /// 熔断标记文件 `<数据目录>/config/tripped`：存在时拒绝启动虚拟机，需 `xr ack-trip` 确认后清除。
    #[must_use]
    pub(crate) fn tripped_file(&self) -> PathBuf {
        self.xr_data.join("config/tripped")
    }

    /// 上次稳定运行时的配置哈希 `<数据目录>/config/last_good`。
    #[must_use]
    pub(crate) fn last_good_file(&self) -> PathBuf {
        self.xr_data.join("config/last_good")
    }

    /// 调试模式到期时间文件 `<数据目录>/config/debug.until`（内容为 Unix 秒）。
    #[must_use]
    pub(crate) fn debug_until_file(&self) -> PathBuf {
        self.xr_data.join("config/debug.until")
    }

    /// 累计流量基线文件 `<数据目录>/config/traffic.base`。
    #[must_use]
    pub(crate) fn acct_base_file(&self) -> PathBuf {
        self.xr_data.join("config/traffic.base")
    }

    /// 累计流量快照文件 `<数据目录>/config/traffic.snap`。
    #[must_use]
    pub(crate) fn acct_snap_file(&self) -> PathBuf {
        self.xr_data.join("config/traffic.snap")
    }

    /// 累计流量起始时间文件 `<数据目录>/config/traffic.since`。
    #[must_use]
    pub(crate) fn acct_since_file(&self) -> PathBuf {
        self.xr_data.join("config/traffic.since")
    }

    /// 创建 config、run、log、images 四个数据目录（已存在则跳过）。
    ///
    /// # Errors
    ///
    /// 任一目录创建失败（权限、只读文件系统、路径被文件占用等）时返回该 I/O 错误。
    pub fn ensure_dirs(&self) -> std::io::Result<()> {
        for dir in [
            self.config_dir(),
            self.run_dir(),
            self.log_dir(),
            self.img_dir(),
        ] {
            std::fs::create_dir_all(dir)?;
        }
        Ok(())
    }

    /// 模块被禁用或待删除（模块目录下存在 `disable` 或 `remove`）时返回 true。
    #[must_use]
    pub(crate) fn is_module_disabled(&self) -> bool {
        self.moddir.join("disable").is_file() || self.moddir.join("remove").is_file()
    }

    /// 把 `/sdcard`、`/storage/emulated/0` 开头的共享路径换算到内部存储根目录下，其余路径原样返回。
    #[must_use]
    pub(crate) fn canon_share_path(&self, raw: &str) -> String {
        let media_str = self.media.to_string_lossy();
        let media = media_str.trim_end_matches('/');
        if raw == "/sdcard" || raw == "/storage/emulated/0" {
            return media.to_owned();
        }
        if let Some(rest) = raw.strip_prefix("/sdcard/") {
            return format!("{media}/{rest}");
        }
        if let Some(rest) = raw.strip_prefix("/storage/emulated/0/") {
            return format!("{media}/{rest}");
        }
        raw.to_owned()
    }

    /// 系统命令 `name` 的完整路径（`<系统命令目录>/<name>`）。
    #[must_use]
    pub(crate) fn sysbin_cmd(&self, name: &str) -> PathBuf {
        self.sysbin.join(name)
    }

    /// 本 `xr` 的路径：优先模块内的 `bin/xr`（可执行时），否则取当前进程可执行文件。
    #[must_use]
    pub(crate) fn self_exe(&self) -> PathBuf {
        let bin_xr = self.moddir.join("bin/xr");
        if is_executable(&bin_xr) {
            return bin_xr;
        }
        env::current_exe().unwrap_or(bin_xr)
    }
}

/// `path` 是普通文件且任一执行位被设置时返回 true。
#[must_use]
pub(crate) fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(path)
        .is_ok_and(|meta| meta.is_file() && (meta.permissions().mode() & 0o111) != 0)
}
