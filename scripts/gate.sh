#!/usr/bin/env bash
set -euo pipefail
cd "$(git rev-parse --show-toplevel 2>/dev/null || pwd)"

# 0. 清掉一切能改 lint 级别、换编译器或换 runner 的环境变量
unset RUSTFLAGS RUSTDOCFLAGS CARGO_ENCODED_RUSTFLAGS CARGO_ENCODED_RUSTDOCFLAGS \
      CARGO_BUILD_RUSTFLAGS CARGO_BUILD_RUSTDOCFLAGS RUSTC RUSTDOC RUSTC_WRAPPER \
      RUSTC_WORKSPACE_WRAPPER CARGO_BUILD_RUSTC CARGO_BUILD_RUSTDOC CARGO_BUILD_RUSTC_WRAPPER \
      CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER CLIPPY_ARGS CLIPPY_CONF_DIR RUSTUP_TOOLCHAIN
while read -r v; do unset "$v"; done < <(compgen -e | grep -E '^CARGO_(TARGET_[A-Z0-9_]+_(RUSTFLAGS|RUSTDOCFLAGS|RUNNER)|PROFILE_[A-Z0-9_]+_RUSTFLAGS)$' || true)

# 1. 工具链必须是锁定版本
want="1.100.0"   # 升级工具链时与 rust-toolchain.toml 在同一次提交里修改
python3 -c 'import sys,tomllib; sys.exit(tomllib.load(open("rust-toolchain.toml","rb"))["toolchain"]["channel"] != sys.argv[1])' "${want}" \
  || { echo "rust-toolchain.toml 未锁定 ${want}"; exit 1; }
rustc --version | grep -qF "rustc ${want} " || { echo "rustc 不是锁定的 ${want}"; exit 1; }
minor="$(cut -d. -f2 <<<"${want}")"
cargo clippy --version | grep -qF "clippy 0.1.${minor} " || { echo "clippy 不是 ${want} 自带版本"; exit 1; }

# 2. 静态守卫：豁免写法、绕路配置、测试布局
python3 scripts/lint-guard.py

# 3. 格式
cargo fmt --all -- --check

# 4. Clippy：所有 target、所有 feature、锁定依赖
#    先清掉本 workspace 的缓存产物：只改 Cargo.toml 元数据不会让 clippy 重跑，cargo 类 lint 会被缓存漏掉
cargo clean --workspace
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings

# 5. 文档（含私有项）零告警
RUSTDOCFLAGS='-D warnings' cargo doc --workspace --all-features --no-deps --document-private-items --locked

# 6. 测试：--all-targets 不含 doctest，单独跑
cargo test --workspace --all-targets --all-features --locked
cargo test --workspace --doc --all-features --locked

echo "gate: 全部通过"
