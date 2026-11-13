use std::env;
use std::path::{Path, PathBuf};

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
    pub in_daemon: bool,
    pub daemon_foreground: bool,
    pub in_watchdog: bool,
    pub in_watch: bool,
    pub fake_date_fail: bool,
}

fn var_nonempty(key: &str) -> Option<String> {
    env::var(key).ok().filter(|v| !v.is_empty())
}

fn var_u64(key: &str, default: u64) -> u64 {
    var_nonempty(key)
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(default)
}

impl Env {
    pub fn from_process() -> Self {
        let moddir = PathBuf::from(
            var_nonempty("MODDIR").unwrap_or_else(|| "/data/adb/modules/xrouter".to_string()),
        );
        let xr_data = PathBuf::from(
            var_nonempty("XR_DATA").unwrap_or_else(|| "/data/adb/xrouter".to_string()),
        );
        let bb = PathBuf::from(
            var_nonempty("BB").unwrap_or_else(|| "/data/adb/ksu/bin/busybox".to_string()),
        );
        let media = PathBuf::from(
            var_nonempty("XR_MEDIA")
                .or_else(|| var_nonempty("MEDIA"))
                .unwrap_or_else(|| "/data/media/0".to_string()),
        );
        let rw_map_opt = env::var("RW_MAP_OPT").unwrap_or_default();
        let sysbin =
            PathBuf::from(var_nonempty("XR_SYSBIN").unwrap_or_else(|| "/system/bin".to_string()));
        let proc_dir =
            PathBuf::from(var_nonempty("XR_PROC").unwrap_or_else(|| "/proc".to_string()));
        let proc_base =
            PathBuf::from(var_nonempty("XR_PROC_BASE").unwrap_or_else(|| "/proc".to_string()));
        let proc_uptime = PathBuf::from(
            var_nonempty("XR_PROC_UPTIME").unwrap_or_else(|| "/proc/uptime".to_string()),
        );
        let proc_mounts = PathBuf::from(
            var_nonempty("XR_PROC_MOUNTS").unwrap_or_else(|| "/proc/mounts".to_string()),
        );
        let sysnet = PathBuf::from(
            var_nonempty("XR_SYSNET").unwrap_or_else(|| "/sys/class/net".to_string()),
        );
        let cgroup_root = PathBuf::from(
            var_nonempty("XR_CGROUP_ROOT").unwrap_or_else(|| "/sys/fs/cgroup".to_string()),
        );
        let crosvm = PathBuf::from(
            var_nonempty("CROSVM")
                .unwrap_or_else(|| "/apex/com.android.virt/bin/crosvm".to_string()),
        );
        let notifier_jar = var_nonempty("XR_NOTIFIER_JAR")
            .map_or_else(|| moddir.join("bin/xrnotify.jar"), PathBuf::from);
        let app_process = PathBuf::from(
            var_nonempty("XR_APP_PROCESS").unwrap_or_else(|| "/system/bin/app_process".to_string()),
        );

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
                    .unwrap_or_else(|| "/data/adb/modules_update".to_string()),
            ),
            tmpdir: var_nonempty("XR_TMPDIR").map(PathBuf::from),
            hs_any: var_nonempty("XR_HS_ANY").is_some(),
            ipv6_wait: var_u64("XR_IPV6_WAIT", 20),
            vpn_wait: var_u64("XR_VPN_WAIT", 20),
            notify_wait: var_u64("XR_NOTIFY_WAIT", 25),
            notify_interval: var_u64("XR_NOTIFY_INTERVAL", 10),
            watchdog_interval: var_u64("XR_WATCHDOG_INTERVAL", 10),
            in_daemon: env::var("XR_IN_DAEMON").as_deref() == Ok("1"),
            daemon_foreground: env::var("XR_DAEMON_FOREGROUND").as_deref() == Ok("1"),
            in_watchdog: env::var("XR_IN_WATCHDOG").as_deref() == Ok("1"),
            in_watch: env::var("XR_IN_WATCH").as_deref() == Ok("1"),
            fake_date_fail: env::var("XR_FAKE_DATE_FAIL").as_deref() == Ok("1"),
        }
    }

    pub fn config_dir(&self) -> PathBuf {
        self.xr_data.join("config")
    }

    pub fn conf_file(&self) -> PathBuf {
        self.xr_data.join("config/config.conf")
    }

    pub fn vpn_dir(&self) -> PathBuf {
        self.xr_data.join("config/vpn")
    }

    pub fn run_dir(&self) -> PathBuf {
        self.xr_data.join("run")
    }

    pub fn log_dir(&self) -> PathBuf {
        self.xr_data.join("log")
    }

    pub fn img_dir(&self) -> PathBuf {
        self.xr_data.join("images")
    }

    pub fn rootfs_img(&self) -> PathBuf {
        self.xr_data.join("images/openwrt-rootfs.img")
    }

    pub fn kernel_img(&self) -> PathBuf {
        self.xr_data.join("images/Image")
    }

    pub fn module_log(&self) -> PathBuf {
        self.xr_data.join("log/module.log")
    }

    pub fn debug_log(&self) -> PathBuf {
        self.xr_data.join("log/debug.log")
    }

    pub fn vmm_log(&self) -> PathBuf {
        self.xr_data.join("log/vmm.log")
    }

    pub fn boot_log(&self) -> PathBuf {
        self.xr_data.join("run/boot.log")
    }

    pub fn console_log(&self) -> PathBuf {
        self.xr_data.join("run/console.log")
    }

    pub fn ctl_log(&self) -> PathBuf {
        self.xr_data.join("run/vm-ctl.log")
    }

    pub fn fuse_file(&self) -> PathBuf {
        self.xr_data.join("config/vm_booting.fuse")
    }

    pub fn tripped_file(&self) -> PathBuf {
        self.xr_data.join("config/tripped")
    }

    pub fn last_good_file(&self) -> PathBuf {
        self.xr_data.join("config/last_good")
    }

    pub fn debug_until_file(&self) -> PathBuf {
        self.xr_data.join("config/debug.until")
    }

    pub fn acct_base_file(&self) -> PathBuf {
        self.xr_data.join("config/traffic.base")
    }

    pub fn acct_snap_file(&self) -> PathBuf {
        self.xr_data.join("config/traffic.snap")
    }

    pub fn acct_since_file(&self) -> PathBuf {
        self.xr_data.join("config/traffic.since")
    }

    pub fn ensure_dirs(&self) -> std::io::Result<()> {
        for d in [
            self.config_dir(),
            self.run_dir(),
            self.log_dir(),
            self.img_dir(),
        ] {
            std::fs::create_dir_all(d)?;
        }
        Ok(())
    }

    pub fn is_module_disabled(&self) -> bool {
        self.moddir.join("disable").is_file() || self.moddir.join("remove").is_file()
    }

    pub fn canon_share_path(&self, raw: &str) -> String {
        let media_str = self.media.to_string_lossy();
        let media = media_str.trim_end_matches('/');
        if raw == "/sdcard" || raw == "/storage/emulated/0" {
            return media.to_string();
        }
        if let Some(rest) = raw.strip_prefix("/sdcard/") {
            return format!("{media}/{rest}");
        }
        if let Some(rest) = raw.strip_prefix("/storage/emulated/0/") {
            return format!("{media}/{rest}");
        }
        raw.to_string()
    }

    pub fn sysbin_cmd(&self, name: &str) -> PathBuf {
        self.sysbin.join(name)
    }

    pub fn self_exe(&self) -> PathBuf {
        let bin_xr = self.moddir.join("bin/xr");
        if is_executable(&bin_xr) {
            return bin_xr;
        }
        env::current_exe().unwrap_or(bin_xr)
    }
}

pub fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && (m.permissions().mode() & 0o111) != 0)
}
