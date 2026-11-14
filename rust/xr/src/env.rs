use std::env;
use std::path::{Path, PathBuf};

#[cfg(test)]
mod tests;

pub const XR_NET6: &str = "fd7a:7872:e9";
pub const XR_IF: &str = "xr0";
pub const HS_TAP: &str = "xr1";
pub const HS_BR: &str = "xrbr1";

#[derive(Debug, Clone)]
pub struct Env {
    pub moddir: PathBuf,
    pub xr_data: PathBuf,
    pub bb: PathBuf,
    pub media: PathBuf,
    pub rw_map_opt: String,
    pub sysbin: PathBuf,
    pub proc_dir: PathBuf,
    pub proc_base: PathBuf,
    pub proc_uptime: PathBuf,
    pub proc_mounts: PathBuf,
    pub sysnet: PathBuf,
    pub cgroup_root: PathBuf,
    pub crosvm: PathBuf,
    pub notifier_jar: PathBuf,
    pub app_process: PathBuf,
    pub screen_override: Option<String>,
    pub download_override: Option<PathBuf>,
    pub modules_update: PathBuf,
    pub tmpdir: Option<PathBuf>,
    pub hs_any: bool,
    pub ipv6_wait: u64,
    pub vpn_wait: u64,
    pub notify_wait: u64,
    pub notify_interval: u64,
    pub watchdog_interval: u64,
    pub role: ProcRole,
    pub daemon_mode: DaemonMode,
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
    pub fn from_values(in_daemon: Option<&str>, in_watchdog: Option<&str>) -> Self {
        match (in_daemon == Some("1"), in_watchdog == Some("1")) {
            (true, _) => Self::Daemon,
            (false, true) => Self::Watchdog,
            (false, false) => Self::Cli,
        }
    }

    /// 是否为守护进程。
    #[must_use]
    pub const fn is_daemon(self) -> bool {
        matches!(self, Self::Daemon)
    }

    /// 是否为看门狗。
    #[must_use]
    pub const fn is_watchdog(self) -> bool {
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
    pub fn from_value(foreground: Option<&str>) -> Self {
        if foreground == Some("1") {
            Self::Foreground
        } else {
            Self::Detached
        }
    }

    /// 是否为前台模式。
    #[must_use]
    pub const fn is_foreground(self) -> bool {
        matches!(self, Self::Foreground)
    }
}

fn var_nonempty(key: &str) -> Option<String> {
    env::var(key).ok().filter(|value| !value.is_empty())
}

fn var_u64(key: &str, default: u64) -> u64 {
    var_nonempty(key)
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(default)
}

impl Env {
    pub fn from_process() -> Self {
        let moddir = PathBuf::from(
            var_nonempty("MODDIR").unwrap_or_else(|| "/data/adb/modules/xrouter".to_owned()),
        );
        let xr_data = PathBuf::from(
            var_nonempty("XR_DATA").unwrap_or_else(|| "/data/adb/xrouter".to_owned()),
        );
        let bb = PathBuf::from(
            var_nonempty("BB").unwrap_or_else(|| "/data/adb/ksu/bin/busybox".to_owned()),
        );
        let media = PathBuf::from(
            var_nonempty("XR_MEDIA")
                .or_else(|| var_nonempty("MEDIA"))
                .unwrap_or_else(|| "/data/media/0".to_owned()),
        );
        let rw_map_opt = env::var("RW_MAP_OPT").unwrap_or_default();
        let sysbin =
            PathBuf::from(var_nonempty("XR_SYSBIN").unwrap_or_else(|| "/system/bin".to_owned()));
        let proc_dir = PathBuf::from(var_nonempty("XR_PROC").unwrap_or_else(|| "/proc".to_owned()));
        let proc_base =
            PathBuf::from(var_nonempty("XR_PROC_BASE").unwrap_or_else(|| "/proc".to_owned()));
        let proc_uptime = PathBuf::from(
            var_nonempty("XR_PROC_UPTIME").unwrap_or_else(|| "/proc/uptime".to_owned()),
        );
        let proc_mounts = PathBuf::from(
            var_nonempty("XR_PROC_MOUNTS").unwrap_or_else(|| "/proc/mounts".to_owned()),
        );
        let sysnet =
            PathBuf::from(var_nonempty("XR_SYSNET").unwrap_or_else(|| "/sys/class/net".to_owned()));
        let cgroup_root = PathBuf::from(
            var_nonempty("XR_CGROUP_ROOT").unwrap_or_else(|| "/sys/fs/cgroup".to_owned()),
        );
        let crosvm = PathBuf::from(
            var_nonempty("CROSVM")
                .unwrap_or_else(|| "/apex/com.android.virt/bin/crosvm".to_owned()),
        );
        let notifier_jar = var_nonempty("XR_NOTIFIER_JAR")
            .map_or_else(|| moddir.join("bin/xrnotify.jar"), PathBuf::from);
        let app_process = var_nonempty("XR_APP_PROCESS")
            .map_or_else(|| sysbin.join("app_process"), PathBuf::from);

        Self {
            moddir,
            xr_data,
            bb,
            media,
            rw_map_opt,
            sysbin,
            proc_dir,
            proc_base,
            proc_uptime,
            proc_mounts,
            sysnet,
            cgroup_root,
            crosvm,
            notifier_jar,
            app_process,
            screen_override: var_nonempty("XR_SCREEN"),
            download_override: var_nonempty("XR_DOWNLOAD").map(PathBuf::from),
            modules_update: PathBuf::from(
                var_nonempty("XR_MODULES_UPDATE")
                    .unwrap_or_else(|| "/data/adb/modules_update".to_owned()),
            ),
            tmpdir: var_nonempty("XR_TMPDIR").map(PathBuf::from),
            hs_any: var_nonempty("XR_HS_ANY").is_some(),
            ipv6_wait: var_u64("XR_IPV6_WAIT", 20),
            vpn_wait: var_u64("XR_VPN_WAIT", 20),
            notify_wait: var_u64("XR_NOTIFY_WAIT", 25),
            notify_interval: var_u64("XR_NOTIFY_INTERVAL", 10),
            watchdog_interval: var_u64("XR_WATCHDOG_INTERVAL", 10),
            role: ProcRole::from_values(
                env::var("XR_IN_DAEMON").ok().as_deref(),
                env::var("XR_IN_WATCHDOG").ok().as_deref(),
            ),
            daemon_mode: DaemonMode::from_value(env::var("XR_DAEMON_FOREGROUND").ok().as_deref()),
            fake_date_fail: env::var("XR_FAKE_DATE_FAIL").as_deref() == Ok("1"),
        }
    }

    #[must_use]
    pub fn config_dir(&self) -> PathBuf {
        self.xr_data.join("config")
    }

    #[must_use]
    pub fn conf_file(&self) -> PathBuf {
        self.xr_data.join("config/config.conf")
    }

    #[must_use]
    pub fn vpn_dir(&self) -> PathBuf {
        self.xr_data.join("config/vpn")
    }

    #[must_use]
    pub fn run_dir(&self) -> PathBuf {
        self.xr_data.join("run")
    }

    #[must_use]
    pub fn log_dir(&self) -> PathBuf {
        self.xr_data.join("log")
    }

    #[must_use]
    pub fn img_dir(&self) -> PathBuf {
        self.xr_data.join("images")
    }

    #[must_use]
    pub fn rootfs_img(&self) -> PathBuf {
        self.xr_data.join("images/openwrt-rootfs.img")
    }

    #[must_use]
    pub fn kernel_img(&self) -> PathBuf {
        self.xr_data.join("images/Image")
    }

    #[must_use]
    pub fn module_log(&self) -> PathBuf {
        self.xr_data.join("log/module.log")
    }

    #[must_use]
    pub fn debug_log(&self) -> PathBuf {
        self.xr_data.join("log/debug.log")
    }

    #[must_use]
    pub fn vmm_log(&self) -> PathBuf {
        self.xr_data.join("log/vmm.log")
    }

    #[must_use]
    pub fn boot_log(&self) -> PathBuf {
        self.xr_data.join("run/boot.log")
    }

    #[must_use]
    pub fn console_log(&self) -> PathBuf {
        self.xr_data.join("run/console.log")
    }

    #[must_use]
    pub fn ctl_log(&self) -> PathBuf {
        self.xr_data.join("run/vm-ctl.log")
    }

    #[must_use]
    pub fn fuse_file(&self) -> PathBuf {
        self.xr_data.join("config/vm_booting.fuse")
    }

    #[must_use]
    pub fn tripped_file(&self) -> PathBuf {
        self.xr_data.join("config/tripped")
    }

    #[must_use]
    pub fn last_good_file(&self) -> PathBuf {
        self.xr_data.join("config/last_good")
    }

    #[must_use]
    pub fn debug_until_file(&self) -> PathBuf {
        self.xr_data.join("config/debug.until")
    }

    #[must_use]
    pub fn acct_base_file(&self) -> PathBuf {
        self.xr_data.join("config/traffic.base")
    }

    #[must_use]
    pub fn acct_snap_file(&self) -> PathBuf {
        self.xr_data.join("config/traffic.snap")
    }

    #[must_use]
    pub fn acct_since_file(&self) -> PathBuf {
        self.xr_data.join("config/traffic.since")
    }

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

    #[must_use]
    pub fn is_module_disabled(&self) -> bool {
        self.moddir.join("disable").is_file() || self.moddir.join("remove").is_file()
    }

    #[must_use]
    pub fn canon_share_path(&self, raw: &str) -> String {
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

    #[must_use]
    pub fn sysbin_cmd(&self, name: &str) -> PathBuf {
        self.sysbin.join(name)
    }

    #[must_use]
    pub fn self_exe(&self) -> PathBuf {
        let bin_xr = self.moddir.join("bin/xr");
        if is_executable(&bin_xr) {
            return bin_xr;
        }
        env::current_exe().unwrap_or(bin_xr)
    }
}

#[must_use]
pub fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(path)
        .is_ok_and(|meta| meta.is_file() && (meta.permissions().mode() & 0o111) != 0)
}
