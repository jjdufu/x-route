use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use crate::config::{valid_kv, Config};
use crate::env::Env;
use crate::log::{log_err, log_info};
use crate::sys::ce_ready;

#[derive(Debug, Clone)]
pub struct ShareSpec {
    pub mode: String,
    pub spec: String,
    pub wrap: Vec<String>,
}

pub fn share_prepare(env: &Env, cfg: &Config) -> ShareSpec {
    let mut res = ShareSpec {
        mode: "off".to_string(),
        spec: String::new(),
        wrap: Vec::new(),
    };
    if !cfg.vm_share() {
        return res;
    }
    if !ce_ready() {
        log_err(env, "share", "手机未解锁（CE 不可用），本次不共享");
        return res;
    }
    let path = env.canon_share_path(cfg.vm_share_path());
    if !valid_kv(env, cfg, "VM_SHARE_PATH", &path) {
        log_err(
            env,
            "share",
            &format!(
                "路径不允许或不存在：{path}（只能是 {} 或其子目录）",
                env.media.display()
            ),
        );
        return res;
    }
    let base = format!("{path}:xrshare:type=fs:cache=auto:timeout=1");
    if cfg.vm_share_mode() == "rw" {
        if env.rw_map_opt.is_empty() {
            log_err(
                env,
                "share",
                "当前 crosvm 不支持属主映射，rw 不可用，本次不共享",
            );
            return res;
        }
        res.spec = format!("{base}:{}", env.rw_map_opt);
        res.mode = "rw".to_string();
    } else {
        let bb = env.bb.to_string_lossy();
        let script = format!(
            "#!/system/bin/sh\n{bb} mount --make-rslave / || exit 1\n{bb} mount --bind \"{path}\" \"{path}\" || exit 1\n{bb} mount -o remount,bind,ro,nosuid,nodev,noexec \"{path}\" || exit 1\nexec \"$@\"\n"
        );
        let ro_path = env.run_dir().join("share-ro.sh");
        let _ = fs::write(&ro_path, script);
        #[cfg(unix)]
        {
            let _ = fs::set_permissions(&ro_path, fs::Permissions::from_mode(0o700));
        }
        res.spec = base;
        res.mode = "ro".to_string();
        res.wrap = vec![
            bb.into_owned(),
            "unshare".to_string(),
            "-m".to_string(),
            "sh".to_string(),
            ro_path.to_string_lossy().into_owned(),
        ];
    }
    log_info(env, "share", &format!("共享 {path} 模式={}", res.mode));
    res
}
