//! 内部存储共享：按 `VM_SHARE*` 配置生成 `crosvm --shared-dir` 参数，只读模式下用挂载命名空间包装 `crosvm`。

use crate::config::{Config, valid_kv};
use crate::env::Env;
use crate::log;
use crate::sys::{ce_ready, fsx};

/// 一次启动的共享目录设置。
#[derive(Debug, Clone)]
pub(crate) struct Spec {
    /// 实际生效的模式：`off`、`ro` 或 `rw`。
    pub(crate) mode: String,
    /// 传给 `--shared-dir` 的参数，不共享时为空串。
    pub(crate) shared_dir: String,
    /// 放在 `crosvm` 命令前面的包装命令（只读模式下为 `busybox unshare -m sh share-ro.sh`），不需要时为空。
    pub(crate) wrap: Vec<String>,
}

/// 按配置准备共享目录：未开启、手机未解锁、路径不合法或读写模式不受支持时不共享（后几种记 ERROR）。
///
/// 只读模式会生成 `run/share-ro.sh`，在私有挂载命名空间里把目录重新挂为只读后再执行 `crosvm`。
#[must_use]
pub(crate) fn prepare(env: &Env, cfg: &Config) -> Spec {
    let mut res = Spec {
        mode: "off".to_owned(),
        shared_dir: String::new(),
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
        res.shared_dir = format!("{base}:{}", env.rw_map_opt);
        "rw".clone_into(&mut res.mode);
    } else {
        let bb = env.bb.to_string_lossy();
        let script = format!(
            "#!/system/bin/sh\n{bb} mount --make-rslave / || exit 1\n{bb} mount --bind \"{path}\" \"{path}\" || exit 1\n{bb} mount -o remount,bind,ro,nosuid,nodev,noexec \"{path}\" || exit 1\nexec \"$@\"\n"
        );
        let ro_path = env.run_dir().join("share-ro.sh");
        fsx::write_logged(env, &ro_path, script.as_bytes());
        fsx::chmod_logged(env, &ro_path, 0o700);
        res.shared_dir = base;
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
