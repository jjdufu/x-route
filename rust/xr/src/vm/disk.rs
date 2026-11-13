use std::fs::{self, File, OpenOptions};
use std::io;

use flate2::read::GzDecoder;

use crate::config::{set_kv, Config};
use crate::env::Env;
use crate::log::{log_err, log_info};
use crate::vm::vm_running;

pub fn disk_expand(env: &Env, new_mb_str: &str) -> i32 {
    if vm_running(env) {
        log_err(env, "vm", "请先停止 VM");
        return 1;
    }
    let Ok(new_mb) = new_mb_str.trim().parse::<u64>() else {
        return 1;
    };
    let rootfs = env.rootfs_img();
    let cur_bytes = fs::metadata(&rootfs).map_or(0, |m| m.len());
    let cur_mb = cur_bytes / 1_048_576;
    if new_mb <= cur_mb {
        log_err(env, "vm", &format!("新大小必须大于当前 {cur_mb}MB"));
        return 1;
    }
    if !set_kv(env, "VM_DISK_SIZE_MB", new_mb_str.trim()) {
        return 1;
    }
    let Some(new_bytes) = new_mb.checked_mul(1_048_576) else {
        return 1;
    };
    let Ok(file) = OpenOptions::new().write(true).open(&rootfs) else {
        log_err(env, "vm", "无法打开系统盘");
        return 1;
    };
    if file.set_len(new_bytes).is_err() {
        log_err(env, "vm", "扩容失败");
        return 1;
    }
    let _ = file.sync_all();
    log_info(
        env,
        "vm",
        &format!("系统盘 {cur_mb}MB -> {new_mb}MB，下次启动时 VM 内执行 resize2fs"),
    );
    0
}

pub fn disk_reset(env: &Env) -> i32 {
    if vm_running(env) {
        log_err(env, "vm", "请先停止 VM");
        return 1;
    }
    let gz_path = env.img_dir().join("openwrt-rootfs.img.gz");
    if !gz_path.is_file() {
        log_err(env, "vm", "缺少 openwrt-rootfs.img.gz");
        return 1;
    }
    let cfg = Config::load(env);
    let rootfs = env.rootfs_img();
    let tmp_path = env.xr_data.join("rootfs.img.new");
    let ok = (|| -> io::Result<()> {
        let gz_file = File::open(&gz_path)?;
        let mut decoder = GzDecoder::new(gz_file);
        let mut out = File::create(&tmp_path)?;
        io::copy(&mut decoder, &mut out)?;
        let target_bytes = cfg
            .vm_disk_size_mb()
            .checked_mul(1_048_576)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "size overflow"))?;
        out.set_len(target_bytes)?;
        out.sync_all()?;
        fs::rename(&tmp_path, &rootfs)?;
        Ok(())
    })();
    if ok.is_err() {
        let _ = fs::remove_file(&tmp_path);
        log_err(env, "vm", "重置失败");
        return 1;
    }
    let _ = fs::remove_file(env.last_good_file());
    log_info(env, "vm", "系统盘已重置");
    0
}
