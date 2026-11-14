# Rust 最严 Lint 门禁（强制执行，不可协商）

你在本项目中编写或修改的每一行 Rust 代码都必须通过以下门禁。门禁失败即任务失败，没有"先合进去以后再修"。

## 一、绝对禁止（任何情况都不允许）
1. 禁止用任何方式压制、跳过或降级警告和错误，包括但不限于：
   - `#[allow(...)]`、`#[expect(...)]`、`#[warn(...)]` 及其 `#![...]` 形式，以及 `#[cfg_attr(..., allow/expect/warn(...))]`
   - `#[rustfmt::skip]`、`#[clippy::...]` 工具属性、`#[doc(hidden)]`（躲 `missing_docs`）、`#[automatically_derived]`
   - 环境变量 `RUSTFLAGS`、`RUSTDOCFLAGS`、`CARGO_BUILD_RUSTFLAGS`、`CARGO_TARGET_<triple>_RUSTFLAGS`、`CARGO_ENCODED_*`、`CLIPPY_ARGS`、`CLIPPY_CONF_DIR` 里的 `-A`、`-W`、`--cap-lints` 等
   - 任何层级的 `.cargo/config(.toml)` 或 `$CARGO_HOME/config.toml` 里的 `rustflags`、`rustdocflags`、`rustc`、`rustc-wrapper`、`rustc-workspace-wrapper`、`rustdoc`、`target.*.runner`，`[alias]` 覆盖 `build`/`check`/`clippy`/`doc`/`fmt`/`test`，以及 `[env]` 注入上述变量
   - `clippy.toml` 放宽检查：白名单外的键（如 `allow-unwrap-in-tests`、`allow-expect-in-tests`、`allow-panic-in-tests`、`allow-indexing-slicing-in-tests`、`allowed-duplicate-crates`、`doc-valid-idents`），以及调大任何阈值。子目录里另放 `clippy.toml` 也算放宽
   - `rustfmt.toml` 里的 `ignore`、`disable_all_formatting`
   - 在 Cargo.toml 的 `[workspace.lints]` 里写 `allow` 或 `warn`，没有任何例外；成员 crate 的 `[lints]` 里写 `workspace = true` 以外的任何内容，或者干脆不写 `[lints]`
   - 用 `cfg(clippy)`、`cfg(not(clippy))`、`cfg(doc)`、`cfg(miri)`、`cfg(any())`、`cfg(false)`、`include!`、`build.rs` 生成代码、宏包装等手段让检查看不到代码
   - 用 `rustup override`、`RUSTUP_TOOLCHAIN`、`cargo +<toolchain>` 换掉锁定的工具链
2. 禁止 `unsafe`，测试代码和 doctest 也一样。
3. 禁止 `unwrap`、`expect`、`panic!`、`todo!`、`unimplemented!`、`unreachable!`、`dbg!`，以及直接下标或切片访问（`v[i]`、`&s[a..b]`）。测试代码也一样，测试规范见第八节。
4. 禁止 `as` 类型转换，一律用 `From`/`TryFrom`/`try_into()` 并处理失败分支。
5. 禁止未检查的算术，包括 `/` 和 `%`，用 `checked_*`、`saturating_*` 或 `wrapping_*` 明确表达意图。
6. 禁止静默丢弃结果，具体是：
   - 禁止 `let _ = expr;`，无论 `expr` 是什么类型
   - 禁止用 `drop(result)`、`.ok();`、`map_err(|_| ...)`、`let _x: Result<..> = ...` 丢掉错误
   - `Result` 和 `#[must_use]` 值必须用 `?`、`match`/`if let` 处理，或在处理分支里记录日志后继续
   - 非 `must_use` 的返回值（如 `map.insert()` 返回的旧值）要么使用，要么写带类型的具名绑定，比如 `let _previous: Option<V> = map.insert(k, v);`，表明是有意忽略

## 二、遇到报错怎么办
- 修根因，不绕路。函数太长就拆，名字太像就改名，`bool` 太多就改用枚举或状态结构，不该公开的就收成 `pub(crate)`。
- 第三方宏或依赖产生的告警，换写法或换依赖解决，不准加豁免。
- 修 lint 不得改变现有功能的外部行为和操作习惯。动逻辑之前先补行为测试，证明前后一致。
- 不确定一个改法是否改变行为时，停下来说明，不要猜。

## 三、工具链
- 仓库根目录必须有 `rust-toolchain.toml`，锁定精确版本：
  ```toml
  [toolchain]
  channel = "1.100.0"
  profile = "minimal"
  components = ["clippy", "rustfmt"]
  # targets = ["aarch64-unknown-linux-musl"]   # 按项目实际填写
  ```
- 门禁脚本里写死同一个版本号，并核对 `rustc` 和 `clippy` 的实际版本。CI 只用锁定版本，禁止跟随 `stable`/`nightly` 漂移。
- 升级工具链是单独一次提交（同时改 `rust-toolchain.toml` 和 `gate.sh`），升级后必须重新清零全部告警。
- 升级时用 `clippy-driver -W help | grep "clippy::nursery "` 对比新旧 nursery 列表：新增项逐项补成 deny，已移除或改名的项从清单里删掉（否则会报 unknown lint）。restriction 组里的新增项要逐个评估是否加入。`lint-guard.py` 发现缺少 nursery 项时会直接失败。
- edition 使用 2024，`resolver = "3"`。
- `Cargo.lock` 必须提交（库 crate 也一样），否则 `--locked` 无从谈起。

## 四、Cargo.toml

### workspace 根
```toml
[workspace]
resolver = "3"
members = ["crates/*"]

[workspace.package]
version = "0.1.0"
edition = "2024"
rust-version = "1.100"
license = "<SPDX，如 MIT OR Apache-2.0>"
description = "<一句话描述>"
repository = "<仓库 URL>"
readme = "README.md"
keywords = ["<关键词>"]
categories = ["<crates.io 分类>"]
publish = false

[workspace.lints.rust]
warnings = "deny"
future_incompatible = { level = "deny", priority = -1 }
let_underscore = { level = "deny", priority = -1 }
nonstandard_style = { level = "deny", priority = -1 }
rust_2018_idioms = { level = "deny", priority = -1 }
unused = { level = "deny", priority = -1 }
keyword_idents = { level = "deny", priority = -1 }
unsafe_code = "forbid"
missing_docs = "deny"
missing_debug_implementations = "deny"
missing_copy_implementations = "deny"
unreachable_pub = "deny"
unnameable_types = "deny"
unused_qualifications = "deny"
unused_lifetimes = "deny"
unused_results = "deny"
unused_import_braces = "deny"
unused_macro_rules = "deny"
unused_extern_crates = "deny"
redundant_imports = "deny"
redundant_lifetimes = "deny"
single_use_lifetimes = "deny"
elided_lifetimes_in_paths = "deny"
explicit_outlives_requirements = "deny"
trivial_casts = "deny"
trivial_numeric_casts = "deny"
meta_variable_misuse = "deny"
macro_use_extern_crate = "deny"
unit_bindings = "deny"
variant_size_differences = "deny"
non_ascii_idents = "deny"
ambiguous_negative_literals = "deny"
deprecated_in_future = "deny"
closure_returning_async_block = "deny"
impl_trait_redundant_captures = "deny"
deref_into_dyn_supertrait = "deny"
# 以下与 unsafe_code = "forbid" 重叠，作冗余防线保留
unsafe_op_in_unsafe_fn = "deny"
missing_unsafe_on_extern = "deny"
unsafe_attr_outside_unsafe = "deny"

[workspace.lints.rustdoc]
# all 已包含下面三项，显式列出便于审阅
all = { level = "deny", priority = -1 }
missing_crate_level_docs = "deny"
unescaped_backticks = "deny"
private_doc_tests = "deny"

[workspace.lints.clippy]
all = { level = "deny", priority = -1 }
pedantic = { level = "deny", priority = -1 }
cargo = { level = "deny", priority = -1 }
# nursery 组不整组开启：组内 redundant_pub_crate 与 rustc unreachable_pub 冲突。
# 下面逐项列出 clippy 1.100.0 nursery 组除 redundant_pub_crate 外的全部 51 项
as_ptr_cast_mut = "deny"
branches_sharing_code = "deny"
clear_with_drain = "deny"
coerce_container_to_any = "deny"
collection_is_never_read = "deny"
debug_assert_with_mut_call = "deny"
derive_partial_eq_without_eq = "deny"
doc_link_code = "deny"
empty_enums = "deny"
equatable_if_let = "deny"
fallible_impl_from = "deny"
future_not_send = "deny"
imprecise_flops = "deny"
iter_on_empty_collections = "deny"
iter_on_single_items = "deny"
iter_with_drain = "deny"
large_stack_frames = "deny"
literal_string_with_formatting_args = "deny"
missing_const_for_fn = "deny"
needless_collect = "deny"
needless_pass_by_ref_mut = "deny"
needless_type_cast = "deny"
non_send_fields_in_send_ty = "deny"
option_if_let_else = "deny"
or_fun_call = "deny"
path_buf_push_overwrite = "deny"
read_zero_byte_vec = "deny"
redundant_clone = "deny"
search_is_some = "deny"
set_contains_or_insert = "deny"
significant_drop_in_scrutinee = "deny"
significant_drop_tightening = "deny"
single_option_map = "deny"
string_lit_as_bytes = "deny"
suboptimal_flops = "deny"
suspicious_operation_groupings = "deny"
too_long_first_doc_paragraph = "deny"
trailing_empty_array = "deny"
trait_duplication_in_bounds = "deny"
transmute_undefined_repr = "deny"
trivial_regex = "deny"
tuple_array_conversions = "deny"
type_repetition_in_bounds = "deny"
uninhabited_references = "deny"
unnecessary_struct_initialization = "deny"
unused_peekable = "deny"
unused_rounding = "deny"
use_self = "deny"
useless_let_if_seq = "deny"
volatile_composites = "deny"
while_float = "deny"
# restriction 组只能逐项开启（组内有互相矛盾的项），下面是全部可兼容项
allow_attributes = "deny"
allow_attributes_without_reason = "deny"
unwrap_used = "deny"
expect_used = "deny"
unwrap_in_result = "deny"
panic = "deny"
panic_in_result_fn = "deny"
todo = "deny"
unimplemented = "deny"
unreachable = "deny"
dbg_macro = "deny"
print_stdout = "deny"
print_stderr = "deny"
exit = "deny"
as_conversions = "deny"
as_underscore = "deny"
as_pointer_underscore = "deny"
fn_to_numeric_cast_any = "deny"
indexing_slicing = "deny"
string_slice = "deny"
get_unwrap = "deny"
arithmetic_side_effects = "deny"
integer_division = "deny"
modulo_arithmetic = "deny"
default_numeric_fallback = "deny"
lossy_float_literal = "deny"
float_cmp_const = "deny"
clone_on_ref_ptr = "deny"
rc_buffer = "deny"
rc_mutex = "deny"
mutex_atomic = "deny"
mutex_integer = "deny"
mem_forget = "deny"
let_underscore_must_use = "deny"
let_underscore_untyped = "deny"
map_err_ignore = "deny"
unused_result_ok = "deny"
assertions_on_result_states = "deny"
try_err = "deny"
wildcard_enum_match_arm = "deny"
rest_pat_in_fully_bound_structs = "deny"
unneeded_field_pattern = "deny"
ref_patterns = "deny"
shadow_unrelated = "deny"
shadow_reuse = "deny"
shadow_same = "deny"
missing_assert_message = "deny"
error_impl_error = "deny"
missing_errors_doc = "deny"
missing_panics_doc = "deny"
missing_docs_in_private_items = "deny"
doc_include_without_cfg = "deny"
tests_outside_test_module = "deny"
redundant_test_prefix = "deny"
cfg_not_test = "deny"
infinite_loop = "deny"
if_then_some_else_none = "deny"
else_if_without_else = "deny"
impl_trait_in_params = "deny"
mixed_read_write_in_expression = "deny"
multiple_inherent_impl = "deny"
same_name_method = "deny"
renamed_function_params = "deny"
partial_pub_fields = "deny"
empty_drop = "deny"
empty_structs_with_brackets = "deny"
empty_enum_variants_with_brackets = "deny"
str_to_string = "deny"
string_add = "deny"
string_lit_chars_any = "deny"
format_push_string = "deny"
needless_raw_strings = "deny"
deref_by_slicing = "deny"
iter_over_hash_type = "deny"
suspicious_xor_used_as_pow = "deny"
precedence_bits = "deny"
verbose_file_reads = "deny"
filetype_is_file = "deny"
create_dir = "deny"
large_include_file = "deny"
unnecessary_self_imports = "deny"
unused_trait_names = "deny"
redundant_type_annotations = "deny"
non_zero_suggestions = "deny"
pathbuf_init_then_push = "deny"
return_and_then = "deny"
map_with_unused_argument_over_ranges = "deny"
min_ident_chars = "deny"
module_name_repetitions = "deny"
mod_module_files = "deny"
pub_without_shorthand = "deny"
semicolon_inside_block = "deny"
unseparated_literal_suffix = "deny"
# 以下与 unsafe_code 重叠，作冗余防线保留
undocumented_unsafe_blocks = "deny"
multiple_unsafe_ops_per_block = "deny"
unnecessary_safety_comment = "deny"
unnecessary_safety_doc = "deny"
```

### 成员 crate
元数据全部继承 workspace，`[lints]` 只能写 `workspace = true`：
```toml
[package]
name = "pxe-core"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
description.workspace = true
repository.workspace = true
readme.workspace = true
keywords.workspace = true
categories.workspace = true
publish.workspace = true

[lints]
workspace = true
```

说明：
- 这份清单已在 rustc/clippy 1.100.0 上逐项验证：名字全部存在，没有已改名或已废弃的项，也没有互相矛盾的项，整份配置没有一个 allow。清单只增不减，删除任何一项都需要项目负责人批准。
- **全配置零 allow。** `nursery` 组不整组开启，而是逐项列出 1.100.0 里除 `redundant_pub_crate` 外的全部 51 项，并且全部设为 deny。原因是 `redundant_pub_crate` 和 rustc 的 `unreachable_pub` 互相冲突：私有模块里的项写 `pub(crate)` 会被前者报，写 `pub` 会被后者报。这里采用 tokio 等项目的做法，保留 `unreachable_pub`，不收录 `redundant_pub_crate`。门禁会拒绝出现 `nursery` 组和 `redundant_pub_crate`，并从当前工具链读取 nursery 列表，逐项核对是否都已设为 deny，所以清单不会被悄悄删短。
- `cargo` 组里的 `cargo_common_metadata` 要求每个 crate 都有 description、license、repository、readme、keywords、categories，配合 `clippy.toml` 里的 `cargo-ignore-publish = true` 后，`publish = false` 的 crate 也要满足。`multiple_crate_versions` 若因依赖树无法消除，必须通过调整依赖解决，或在门禁文档里写明理由并由项目负责人决定是否从清单删除该项，不准在代码里豁免，也不准用 `allowed-duplicate-crates`。
- 项目确实需要打印输出的二进制入口（CLI），用日志库或显式的 `std::io::Write` 写 stdout/stderr 并处理错误，不准为 `print_stdout` 开豁免。`main` 返回 `ExitCode` 或 `Result`，不准调用 `process::exit`。
- 每个 lib crate 根（`lib.rs`）必须有 `#![doc(test(attr(deny(warnings), forbid(unsafe_code))))]`，因为 workspace lints 不作用于 doctest。

### clippy.toml（必须存在，只放在仓库根目录，只能收紧）
```toml
avoid-breaking-exported-api = false
check-private-items = true
too-many-lines-threshold = 60
cognitive-complexity-threshold = 15
too-many-arguments-threshold = 5
type-complexity-threshold = 150
excessive-nesting-threshold = 4
max-fn-params-bools = 1
max-struct-bools = 2
max-trait-bounds = 3
large-error-threshold = 64
enum-variant-size-threshold = 128
single-char-binding-names-threshold = 2
max-include-file-size = 65536
cargo-ignore-publish = true
```
门禁只接受上面这些键，外加用于收紧的 `disallowed-methods`、`disallowed-types`、`disallowed-macros`、`disallowed-names`。数值只能在 1 到上表值之间，布尔值必须和上表一致。`check-private-items = true` 让 `missing_errors_doc`、`missing_panics_doc` 也覆盖私有函数和测试函数。`cargo-ignore-publish` 在 clippy 文档里标注为内部测试用途，但在 1.100.0 上实测有效。

## 五、门禁脚本（本地和 CI 跑同一份，任一步失败即失败）

`scripts/gate.sh`：
```bash
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
```

`scripts/lint-guard.py`（Python ≥ 3.11，只用标准库；扫描前先去掉注释和字符串字面量，所以注释里写 "unsafe" 不会误报）：
```python
#!/usr/bin/env python3
"""门禁静态守卫：扫描豁免、绕路配置和测试布局。任何发现即失败。"""
import os, re, subprocess, sys, tomllib
from pathlib import Path

ROOT = Path.cwd().resolve()
SKIP = {"target", ".git"}
errs: list[str] = []
def err(msg: str) -> None: errs.append(msg)

def walk(pred):
    for d, dirs, files in os.walk(ROOT):
        dirs[:] = [x for x in dirs if x not in SKIP]
        for f in files:
            if pred(f): yield Path(d) / f

def load(p: Path) -> dict:
    try: return tomllib.loads(p.read_text())
    except tomllib.TOMLDecodeError as e: err(f"{p}: TOML 解析失败 {e}"); return {}

def rel(p: Path) -> str: return str(p.relative_to(ROOT)) if p.is_relative_to(ROOT) else str(p)

# ---------- 1. Cargo.toml ----------
LEVELS = {"deny", "forbid"}
REQUIRED = {("rust", "warnings"), ("rust", "unsafe_code"), ("clippy", "all"), ("clippy", "pedantic"),
            ("clippy", "cargo"), ("rustdoc", "all")}
# nursery 组不得整组开启；组内除 redundant_pub_crate 外每一项都必须逐项 deny（列表取自当前工具链）
BANNED = {("clippy", "nursery"), ("clippy", "redundant_pub_crate")}
def nursery_lints() -> set[str]:
    out = subprocess.run(["clippy-driver", "-W", "help"], capture_output=True, text=True).stdout
    line = next((l for l in out.splitlines() if re.match(r"\s*clippy::nursery\s", l)), "")
    names = {n.strip().removeprefix("clippy::").replace("-", "_") for n in line.split(None, 1)[-1].split(",")}
    names.discard("")
    if not names: err("无法从 clippy-driver -W help 读取 nursery 列表")
    return names - {"redundant_pub_crate"}
root = load(ROOT / "Cargo.toml")
wl = root.get("workspace", {}).get("lints")
if not isinstance(wl, dict): err("根 Cargo.toml 缺少 [workspace.lints]")
else:
    for tool, table in wl.items():
        for name, v in table.items():
            if isinstance(v, dict):
                if set(v) - {"level", "priority"}: err(f"[workspace.lints.{tool}] {name}: 只允许 level/priority")
                level = v.get("level")
            else: level = v
            if level not in LEVELS: err(f"[workspace.lints.{tool}] {name} = {level!r}：只允许 deny/forbid")
    for tool, name in REQUIRED:
        if name not in wl.get(tool, {}): err(f"[workspace.lints.{tool}] 缺少 {name}")
    if wl.get("rust", {}).get("unsafe_code") != "forbid": err("unsafe_code 必须是 forbid")
    for tool, name in BANNED:
        if name in wl.get(tool, {}): err(f"[workspace.lints.{tool}] 不得出现 {name}（与 unreachable_pub 冲突）")
    for name in sorted(nursery_lints() - set(wl.get("clippy", {}))):
        err(f"[workspace.lints.clippy] 缺少 nursery 项 {name}（工具链升级后须补齐）")
if (ROOT / "tests").is_dir() and "package" not in root:
    err("虚拟 workspace 根目录的 tests/ 不会被编译执行，改用独立 e2e crate")
for p in walk(lambda f: f == "Cargo.toml"):
    m = load(p); t = p.read_text()
    if "cargo-features" in m: err(f"{rel(p)}: 禁止 cargo-features")
    if re.search(r"(?m)^\s*rustflags\s*=", t): err(f"{rel(p)}: 禁止 rustflags")
    if "package" in m and m.get("lints") != {"workspace": True}:
        err(f"{rel(p)}: [lints] 必须且只能是 workspace = true")
    lib = p.parent / "src" / "lib.rs"
    if "package" in m and lib.exists() and "#![doc(test(attr(deny(warnings), forbid(unsafe_code))))]" not in lib.read_text():
        err(f"{rel(lib)}: 缺少 #![doc(test(attr(deny(warnings), forbid(unsafe_code))))]")

# ---------- 2. Cargo 配置与 clippy/rustfmt 配置 ----------
BAD_BUILD = {"rustflags", "rustdocflags", "rustc", "rustc-wrapper", "rustc-workspace-wrapper", "rustdoc"}
BAD_ALIAS = {"build", "check", "clippy", "doc", "fmt", "test", "b", "c", "d", "t"}
BAD_ENV = re.compile(r"RUSTFLAGS|RUSTDOCFLAGS|RUSTC|RUSTDOC|CLIPPY|RUSTUP_TOOLCHAIN")
homes = [Path(os.environ.get("CARGO_HOME", Path.home() / ".cargo"))]
cfgs = [d / ".cargo" / n for d in [ROOT, *ROOT.parents] for n in ("config", "config.toml")]
cfgs += [h / n for h in homes for n in ("config", "config.toml")]
for p in cfgs:
    if not p.is_file(): continue
    c = load(p)
    for k in BAD_BUILD & set(c.get("build", {})): err(f"{p}: 禁止 build.{k}")
    for tk, tv in c.get("target", {}).items():
        for k in {"rustflags", "rustdocflags", "runner"} & set(tv): err(f"{p}: 禁止 target.{tk}.{k}")
    for pk, pv in c.get("profile", {}).items():
        if "rustflags" in pv: err(f"{p}: 禁止 profile.{pk}.rustflags")
    for k in BAD_ALIAS & set(c.get("alias", {})): err(f"{p}: 禁止覆盖 alias.{k}")
    for k in c.get("env", {}):
        if BAD_ENV.search(k): err(f"{p}: 禁止 env.{k}")
    if "unstable" in c: err(f"{p}: 禁止 [unstable]")

# clippy.toml：必须存在于根目录且只此一份；只能收紧
STRICT = {"too-many-lines-threshold": 60, "cognitive-complexity-threshold": 15,
          "too-many-arguments-threshold": 5, "type-complexity-threshold": 150,
          "excessive-nesting-threshold": 4, "max-fn-params-bools": 1, "max-struct-bools": 2,
          "max-trait-bounds": 3, "large-error-threshold": 64, "enum-variant-size-threshold": 128,
          "single-char-binding-names-threshold": 2, "max-include-file-size": 65536}
FIXED = {"avoid-breaking-exported-api": False, "check-private-items": True, "cargo-ignore-publish": True}
EXTRA = {"disallowed-methods", "disallowed-types", "disallowed-macros", "disallowed-names"}
confs = list(walk(lambda f: f in ("clippy.toml", ".clippy.toml")))
if confs != [ROOT / "clippy.toml"]: err(f"clippy 配置必须只有根目录 clippy.toml，实际 {[rel(c) for c in confs]}")
if (ROOT / "clippy.toml").is_file():
    cc = load(ROOT / "clippy.toml")
    for k, v in cc.items():
        if k in STRICT:
            if not isinstance(v, int) or isinstance(v, bool) or not 1 <= v <= STRICT[k]:
                err(f"clippy.toml {k} = {v!r}：只能在 1..={STRICT[k]} 内收紧")
        elif k in FIXED:
            if v is not FIXED[k]: err(f"clippy.toml {k} 必须为 {FIXED[k]}")
        elif k not in EXTRA: err(f"clippy.toml 不允许的键 {k}（白名单外一律视为放宽）")
    for k in (set(STRICT) | set(FIXED)) - set(cc): err(f"clippy.toml 缺少 {k}")
for p in walk(lambda f: f in ("rustfmt.toml", ".rustfmt.toml")):
    for k in {"ignore", "disable_all_formatting", "unstable_features"} & set(load(p)):
        err(f"{rel(p)}: 禁止 {k}")

# ---------- 3. Rust 源码（去掉注释和字符串后扫描） ----------
TOKEN = re.compile(r'//[^\n]*|/\*(?:[^*]|\*(?!/))*\*/|b?r(#*)"(?:.|\n)*?"\1|b?"(?:\\.|[^"\\])*"|'
                   r"b?'(?:\\.|[^'\\\n])'", re.S)
def strip(src: str) -> str:
    return TOKEN.sub(lambda m: "\n" * m.group(0).count("\n") if m.group(0)[0] in "/" else '""', src)

RULES = [
    (r"#!?\[\s*(allow|expect|warn)\s*\(", "禁止 allow/expect/warn 属性"),
    (r"\bcfg_attr\s*\((?:[^()]|\([^()]*\))*\b(allow|expect|warn)\s*\(", "禁止 cfg_attr 包装豁免"),
    (r"\bunsafe\b", "禁止 unsafe"),
    (r"\bcfg(?:_attr)?\s*\([^\]]*?\b(clippy|rustfmt|miri|doc)\b(?!\s*[=(])", "禁止按 clippy/rustfmt/miri/doc 条件编译"),
    (r"\bcfg\s*\(\s*(any\s*\(\s*\)|false)\s*\)", "禁止用 cfg(any())/cfg(false) 隐藏代码"),
    (r"#!?\[\s*(rustfmt|clippy)\s*::", "禁止 rustfmt::/clippy:: 工具属性"),
    (r"#!?\[\s*doc\s*\(\s*hidden", "禁止 doc(hidden) 躲避 missing_docs"),
    (r"#!?\[\s*automatically_derived", "禁止 automatically_derived"),
    (r"\binclude\s*!", "禁止 include!（生成代码绕过检查）"),
    (r"#\[\s*cfg\s*\((?:[^()]|\([^()]*\))*\btest\b[^\]]*\]\s*(?:#\[[^\]]*\]\s*)*(?:pub\s*(?:\([^)]*\))?\s*)?mod\s+\w+\s*\{",
     "禁止内联测试模块，改用 #[cfg(test)] mod xxx; 指向独立文件"),
]
for p in walk(lambda f: f.endswith(".rs")):
    code = strip(p.read_text())
    for pat, msg in RULES:
        for m in re.finditer(pat, code):
            err(f"{rel(p)}:{code.count(chr(10), 0, m.start()) + 1}: {msg}")

for e in errs: print(f"lint-guard: {e}", file=sys.stderr)
sys.exit(1 if errs else 0)
```

补充：
- `lint-guard.py` 只是第一道筛子，最终以第 4 到 6 步的真实结果为准。
- 如果某些 feature 组合互斥，在 CI 里用 `cargo hack clippy --workspace --all-targets --feature-powerset --locked -- -D warnings` 逐个组合跑，不准删 `--all-features` 了事。
- 交叉编译目标在 CI 里追加 `cargo clippy ... --target <triple> -- -D warnings`。
- 建议加上供应链检查：`cargo deny check`（许可证、漏洞、重复依赖、来源），以及 `cargo machete` 查未使用依赖。rustc 的 `unused_crate_dependencies` 在 cargo 下会被静默处理，起不到作用，所以不放进清单。

## 六、已有项目接入流程
1. 锁定工具链，建 workspace 结构，写入上面的完整 lint 清单、`clippy.toml` 和门禁脚本，提交 `Cargo.lock`。
2. 跑一次门禁记录基线报错数，按类别统计。
3. 先删掉代码和配置里现存的所有豁免。
4. 先做机械修复（`#[must_use]`、`write!`、`as` 转换、改名、补 `# Errors`/`# Panics`、带后缀字面量 `1_u32`）。
5. 再收窄可见性（能 `pub(crate)` 的不 `pub`），然后为剩余公开项和私有项写文档。
6. 迁移测试布局（第八节），把 `assert!` 改成返回 `Err`。
7. 最后拆分超长函数和复杂结构：先补行为测试，一次只拆一个函数，一次提交。
8. 每一步结束时报错数只能下降，最终必须为 0，门禁脚本全绿才算完成。

## 七、交付时必须报告
- 门禁脚本完整输出（全绿）。
- 本次新增或修改的测试，以及它们证明了什么行为不变。
- 任何你认为该 lint 本身不合理的地方：只报告，不豁免，由项目负责人决定。

## 八、测试规范
1. **单元测试**放在独立文件里，源码只写声明：
   ```rust
   // src/node.rs
   #[cfg(test)]
   mod tests;   // 对应 src/node/tests.rs；lib.rs/main.rs 里对应 src/tests.rs
   ```
   作为子模块，它可以直接测私有函数。禁止内联 `mod tests { ... }`。需要换目录时可以用 `#[cfg(test)] #[path = "..."] mod tests;`。模块布局统一用 `foo.rs` + `foo/`，禁止 `mod.rs`（`mod_module_files`）。
2. **集成测试**用目录形式 `tests/<name>/main.rs` + `tests/<name>/cases.rs`。`main.rs` 只放 `//!` crate 文档和 `#[cfg(test)] mod cases;`，因为 `tests_outside_test_module` 要求所有 `#[test]` 都在 `cfg(test)` 模块里，而 `tests/<name>.rs` 的子模块会被解析到 `tests/` 根目录，变成另一个测试目标。
3. **端到端测试**放在独立的 e2e crate 里。虚拟 workspace 根目录的 `tests/` 不会被编译执行，门禁会直接拒绝。
4. **测试函数**一律返回 `Result`，用 `?` 传播。`assert!`/`assert_eq!` 在返回 `Result` 的函数里会触发 `panic_in_result_fn`，所以断言改成返回 `Err`。推荐建一个 `publish = false` 的 `test-support` crate 作为 dev-dependency 统一提供，也可以在测试模块里就地定义：
   ```rust
   /// 条件不成立时返回错误，替代 `assert!`。
   ///
   /// # Errors
   ///
   /// `cond` 为假时返回 `msg`。
   fn ensure(cond: bool, msg: &'static str) -> Result<(), &'static str> {
       if cond { Ok(()) } else { Err(msg) }
   }
   ```
5. 测试文件同样受全部 lint 约束：每个文件要有 `//!` 文档，每个测试函数要有 `///` 文档和 `# Errors` 小节（`missing_docs_in_private_items` 加 `check-private-items`）；函数名不加 `test_` 前缀（`redundant_test_prefix`）；集成测试 crate 里不要写 `pub`（`unreachable_pub`）。
6. **doctest** 不经过 clippy，只受 `#![doc(test(attr(...)))]` 约束，由评审把关：禁止 `unwrap`/`expect`/`unsafe`，用 `?` 加隐藏行 `# Ok::<(), E>(())`；断言必须带消息。
