//! `XRouter` 控制面库：手机侧守护进程、网络规则、虚拟机管理与 CLI 共用的实现。
#![doc(test(attr(deny(warnings), forbid(unsafe_code))))]
pub mod cidr;
pub mod clash;
pub mod cli;
pub mod config;
pub mod daemon;
pub mod diag;
pub mod env;
pub mod log;
pub mod net;
pub(crate) mod out;
pub mod sys;
pub mod traffic;
pub mod uninstall;
pub mod vm;
