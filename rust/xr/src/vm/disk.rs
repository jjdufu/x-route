//! 读写系统盘的扩容与重置。

use std::fs::{self, File, OpenOptions};
use std::io;

use flate2::read::GzDecoder;

use crate::config::{Config, set_kv};
use crate::env::Env;
use crate::log;
use crate::sys::fsx;
use crate::vm;

/// `xr expand <MB>`：虚拟机停止时把系统盘镜像扩大到新大小并记入 `VM_DISK_SIZE_MB`；文件系统在下次启动时由客机扩展。
///
/// 运行中、参数非法或不大于当前大小、写入失败时记 ERROR 并返回 1，成功返回 0。
#[must_use]
pub(crate) fn expand(env: &Env, new_mb_str: &str) -> i32 {
    if vm::running(env) {
        log::err(env, "vm", "请先停止 VM");
        return 1;
    }
    let Ok(new_mb) = new_mb_str.trim().parse::<u64>() else {
        return 1;
    };
    let rootfs = env.rootfs_img();
    let cur_bytes = fs::metadata(&rootfs).map_or(0, |meta| meta.len());
    let cur_mb = cur_bytes.checked_div(1_048_576_u64).unwrap_or(0_u64);
    if new_mb <= cur_mb {
        log::err(env, "vm", &format!("新大小必须大于当前 {cur_mb}MB"));
        return 1;
    }
    if !set_kv(env, "VM_DISK_SIZE_MB", new_mb_str.trim()) {
        return 1;
    }
    let Some(new_bytes) = new_mb.checked_mul(1_048_576) else {
        return 1;
    };
    let Ok(file) = OpenOptions::new().write(true).open(&rootfs) else {
        log::err(env, "vm", "无法打开系统盘");
        return 1;
    };
    if file.set_len(new_bytes).is_err() {
        log::err(env, "vm", "扩容失败");
        return 1;
    }
    if let Err(err) = file.sync_all() {
        fsx::record_failure(env, "sync", &rootfs, &err);
    }
    log::info(
        env,
        "vm",
        &format!("系统盘 {cur_mb}MB -> {new_mb}MB，下次启动时 VM 内执行 resize2fs"),
    );
    0
}

/// `xr reset-disk`：虚拟机停止时从 `openwrt-rootfs.img.gz` 重新解压系统盘并按 `VM_DISK_SIZE_MB` 扩到目标大小，删除 `last_good`。
///
/// 失败时删除临时文件、记 ERROR 并返回 1，成功返回 0。
#[must_use]
pub(crate) fn reset(env: &Env) -> i32 {
    if vm::running(env) {
        log::err(env, "vm", "请先停止 VM");
        return 1;
    }
    let gz_path = env.img_dir().join("openwrt-rootfs.img.gz");
    if !gz_path.is_file() {
        log::err(env, "vm", "缺少 openwrt-rootfs.img.gz");
        return 1;
    }
    let cfg = Config::load(env);
    let rootfs = env.rootfs_img();
    let tmp_path = env.xr_data.join("rootfs.img.new");
    let ok = (|| -> io::Result<()> {
        let gz_file = File::open(&gz_path)?;
        let mut decoder = GzDecoder::new(gz_file);
        let mut out = File::create(&tmp_path)?;
        let _decompressed: u64 = io::copy(&mut decoder, &mut out)?;
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
        fsx::remove_logged(env, &tmp_path);
        log::err(env, "vm", "重置失败");
        return 1;
    }
    fsx::remove_logged(env, env.last_good_file());
    log::info(env, "vm", "系统盘已重置");
    0
}
