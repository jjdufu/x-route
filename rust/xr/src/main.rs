use std::process::ExitCode;

use xr::config::{config_hash, set_kv, valid_kv, Config, KEYS};
use xr::env::Env;
use xr::sys::spawn::exec_crosvm_trampoline;

fn exit_from_i32(code: i32) -> ExitCode {
    let byte = u8::try_from(code.clamp(0, 255)).unwrap_or(1);
    ExitCode::from(byte)
}

fn run_cli(args: &[String]) -> i32 {
    let env = Env::from_process();
    let Some(cmd) = args.first().map(String::as_str) else {
        println!("usage: xr <subcommand>");
        return 2;
    };
    let rest = args.get(1..).unwrap_or(&[]);
    match cmd {
        "get" => {
            let _ = env.ensure_dirs();
            let cfg = Config::load(&env);
            for k in KEYS {
                println!("{k}={}", cfg.get_raw(k));
            }
            0
        }
        "internal" => match rest.first().map(String::as_str) {
            Some("dump-config-keys") => {
                for k in KEYS {
                    println!("{k}");
                }
                0
            }
            Some("config-hash") => {
                println!("{}", config_hash(&env));
                0
            }
            Some("valid-kv") => {
                let (Some(k), Some(v)) = (rest.get(1), rest.get(2)) else {
                    return 2;
                };
                let cfg = Config::load(&env);
                i32::from(!valid_kv(&env, &cfg, k, v))
            }
            Some("set-kv") => {
                let (Some(k), Some(v)) = (rest.get(1), rest.get(2)) else {
                    return 2;
                };
                i32::from(!set_kv(&env, k, v))
            }
            Some("exec-crosvm") => exec_crosvm_trampoline(rest.get(1..).unwrap_or(&[])),
            _ => 2,
        },
        _ => 2,
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    exit_from_i32(run_cli(&args))
}
