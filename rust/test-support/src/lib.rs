//! `XRouter` 测试辅助：用返回 `Err` 的检查函数替代 `assert!` 系列宏。
//!
//! 测试函数返回 `Result<(), Failure>`，每个检查用 `?` 传播失败，测试代码里不出现 `panic`、`unwrap` 与 `expect`。
#![doc(test(attr(deny(warnings), forbid(unsafe_code))))]

use std::error::Error;
use std::fmt::{self, Debug, Display, Formatter};

/// 一次检查失败：带人能读懂的说明。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure(String);

impl Failure {
    /// 用一段说明构造失败。
    #[must_use]
    pub fn new(msg: &str) -> Self {
        Self(msg.to_owned())
    }
}

impl Display for Failure {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Error for Failure {}

/// 条件成立时返回 `Ok(())`。
///
/// # Errors
///
/// `cond` 为假时返回以 `msg` 为说明的 [`Failure`]。
pub fn ensure(cond: bool, msg: &str) -> Result<(), Failure> {
    if cond { Ok(()) } else { Err(Failure::new(msg)) }
}

/// 两值相等时返回 `Ok(())`。
///
/// # Errors
///
/// 不相等时返回 [`Failure`]，说明里带 `what` 和两边的 `Debug` 输出。
pub fn ensure_eq<T: PartialEq + Debug + ?Sized>(
    left: &T,
    right: &T,
    what: &str,
) -> Result<(), Failure> {
    if left == right {
        Ok(())
    } else {
        Err(Failure(format!("{what}：左 {left:?}，右 {right:?}")))
    }
}

/// 两值不相等时返回 `Ok(())`。
///
/// # Errors
///
/// 相等时返回 [`Failure`]，说明里带 `what` 和该值的 `Debug` 输出。
pub fn ensure_ne<T: PartialEq + Debug + ?Sized>(
    left: &T,
    right: &T,
    what: &str,
) -> Result<(), Failure> {
    if left == right {
        Err(Failure(format!("{what}：两边都是 {left:?}")))
    } else {
        Ok(())
    }
}

/// 结果为 `Ok` 时取出其中的值。
///
/// # Errors
///
/// 结果为 `Err` 时返回 [`Failure`]，说明里带 `what` 和原错误。
pub fn ensure_ok<T, E: Display>(res: Result<T, E>, what: &str) -> Result<T, Failure> {
    res.map_err(|err| Failure(format!("{what}：{err}")))
}

/// 值为 `Some` 时取出其中的值。
///
/// # Errors
///
/// 值为 `None` 时返回以 `what` 为说明的 [`Failure`]。
pub fn ensure_some<T>(opt: Option<T>, what: &str) -> Result<T, Failure> {
    opt.ok_or_else(|| Failure(format!("{what}：没有值")))
}
