use crate::config::{Config, valid_kv};
use crate::env::Env;
use crate::log;
use crate::sys::{ce_ready, fsx};

#[derive(Debug, Clone)]
pub struct Spec {
    pub mode: String,
    pub spec: String,
    pub wrap: Vec<String>,
}

#[must_use]
pub fn prepare(env: &Env, cfg: &Config) -> Spec {
    let mut res = Spec {
        mode: "off".to_owned(),
        spec: String::new(),
        wrap: Vec::new(),
    };
    if !cfg.vm_share() {
        return res;
    }
    if !ce_ready() {
        log::err(env, "share", "手机未解锁（CE 不可用），本次不共享");
        return res;
    }
    let path = env.canon_share_path(cfg.vm_share_path());
    if !valid_kv(env, cfg, "VM_SHARE_PATH", &path) {
        log::err(
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
            log::err(
                env,
                "share",
                "当前 crosvm 不支持属主映射，rw 不可用，本次不共享",
            );
            return res;
        }
        res.spec = format!("{base}:{}", env.rw_map_opt);
        "rw".clone_into(&mut res.mode);
    } else {
        let bb = env.bb.to_string_lossy();
        let script = format!(
            "#!/system/bin/sh\n{bb} mount --make-rslave / || exit 1\n{bb} mount --bind \"{path}\" \"{path}\" || exit 1\n{bb} mount -o remount,bind,ro,nosuid,nodev,noexec \"{path}\" || exit 1\nexec \"$@\"\n"
        );
        let ro_path = env.run_dir().join("share-ro.sh");
        fsx::write_logged(env, &ro_path, script.as_bytes());
        fsx::chmod_logged(env, &ro_path, 0o700);
        res.spec = base;
        "ro".clone_into(&mut res.mode);
        res.wrap = vec![
            bb.into_owned(),
            "unshare".to_owned(),
            "-m".to_owned(),
            "sh".to_owned(),
            ro_path.to_string_lossy().into_owned(),
        ];
    }
    log::info(env, "share", &format!("共享 {path} 模式={}", res.mode));
    res
}
