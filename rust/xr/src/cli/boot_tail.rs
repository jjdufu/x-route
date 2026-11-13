use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use crate::env::Env;

fn tail_from(path: &Path, offset_str: &str) -> (u64, String) {
    let sz = fs::metadata(path).map_or(0, |m| m.len());
    if offset_str == "-1" {
        return (sz, String::new());
    }
    let mut off = offset_str.trim().parse::<u64>().unwrap_or(0);
    if off > sz {
        off = 0;
    }
    let max_chunk = 6144_u64;
    if sz.saturating_sub(off) > max_chunk {
        off = sz.saturating_sub(max_chunk);
    }
    if off >= sz {
        return (sz, String::new());
    }
    let to_read = usize::try_from(sz.saturating_sub(off)).unwrap_or(6144);
    let Ok(mut f) = File::open(path) else {
        return (sz, String::new());
    };
    if f.seek(SeekFrom::Start(off)).is_err() {
        return (sz, String::new());
    }
    let mut buf = vec![0_u8; to_read];
    let n = f.read(&mut buf).unwrap_or(0);
    let slice = buf.get(..n).unwrap_or(&[]);
    let cleaned: Vec<u8> = slice
        .iter()
        .copied()
        .filter(|&b| b == b'\t' || b == b'\n' || (b >= 0x20 && b != 0x7f))
        .collect();
    (sz, String::from_utf8_lossy(&cleaned).into_owned())
}

pub fn run_boot_tail(env: &Env, moff: &str, coff: &str) -> i32 {
    let run = env.run_dir();
    let st_raw = fs::read_to_string(run.join("state")).unwrap_or_default();
    let st_trim = st_raw.trim();
    let state = if !st_trim.is_empty() {
        st_trim
    } else if run.join("crosvm.pid").is_file() {
        "booting"
    } else {
        "stopped"
    };
    let bg_rc_raw = fs::read_to_string(run.join("bg.rc")).unwrap_or_default();
    let bg_rc = bg_rc_raw.trim();
    let rc_s = if bg_rc.is_empty() { "-" } else { bg_rc };

    let (msz, mchunk) = tail_from(&run.join("boot.log"), moff);
    let (csz, cchunk) = tail_from(&run.join("console.log"), coff);
    print!("R{rc_s}\x1fS{state}\x1f{msz}\x1f{mchunk}\x1f{csz}\x1f{cchunk}");
    0
}
