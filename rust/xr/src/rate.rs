//! 实时速率：两次累计计数之差除以时间差，只在内存中按需计算。

/// 一组上下行速率（字节/秒）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize)]
pub(crate) struct Pair {
    /// 上行。
    pub(crate) up: u64,
    /// 下行。
    pub(crate) down: u64,
}

/// 公网、内网、合计三组速率。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize)]
pub(crate) struct Rates {
    /// 公网。
    pub(crate) wan: Pair,
    /// 内网。
    pub(crate) lan: Pair,
    /// 合计。
    pub(crate) total: Pair,
    /// 本次速率对应的采样间隔（毫秒）。
    pub(crate) interval_ms: u64,
}

/// 上一次采样的基线。
#[derive(Debug, Clone, Copy)]
struct Baseline {
    /// 采样时刻（毫秒，单调时钟上的任意起点）。
    at_ms: u64,
    /// 采样时的累计值。
    quad: [u64; 4],
}

/// 上一次采样与最近一次结果。
#[derive(Debug, Default)]
pub(crate) struct RateState {
    /// 基线；`None` 表示尚未采样或刚被重置。
    last: Option<Baseline>,
    /// 最近一次算出的速率。
    rate: Option<Rates>,
}

/// 最小采样间隔：小于此间隔的重复查询直接返回上次结果，避免除以极小时间放大抖动。
pub(crate) const MIN_INTERVAL_MS: u64 = 1_000;
/// 超过此间隔视为冷启动：只记基线、不出速率，避免把长时间平均当成实时。
pub(crate) const MAX_INTERVAL_MS: u64 = 30_000;

/// `delta` 字节在 `dt_ms` 毫秒内的速率（字节/秒），乘法饱和、除数为 0 时为 0。
fn per_sec(delta: u64, dt_ms: u64) -> u64 {
    delta.saturating_mul(1_000).checked_div(dt_ms).unwrap_or(0)
}

/// 两次累计值之差；任一分量回退（清零或链重建）时返回 `None`。
fn quad_delta(now: [u64; 4], before: [u64; 4]) -> Option<[u64; 4]> {
    let mut delta = [0_u64; 4];
    for (slot, (cur, old)) in delta.iter_mut().zip(now.into_iter().zip(before)) {
        *slot = cur.checked_sub(old)?;
    }
    Some(delta)
}

/// 由增量与间隔算出三组速率。
fn rates_of(delta: [u64; 4], dt_ms: u64) -> Rates {
    let [wan_up, wan_down, lan_up, lan_down] = delta.map(|bytes| per_sec(bytes, dt_ms));
    Rates {
        wan: Pair {
            up: wan_up,
            down: wan_down,
        },
        lan: Pair {
            up: lan_up,
            down: lan_down,
        },
        total: Pair {
            up: wan_up.saturating_add(lan_up),
            down: wan_down.saturating_add(lan_down),
        },
        interval_ms: dt_ms,
    }
}

impl RateState {
    /// 在时刻 `now_ms` 喂入一次累计值 `quad = [wan_up, wan_down, lan_up, lan_down]`（与 `acct_total` 同序），返回当前速率。
    ///
    /// 首次采样、计数回退或间隔超过 [`MAX_INTERVAL_MS`] 时重置基线并返回 `None`；
    /// 间隔不足 [`MIN_INTERVAL_MS`] 时返回上次结果、基线不变。
    pub(crate) fn update_at(&mut self, now_ms: u64, quad: [u64; 4]) -> Option<Rates> {
        let fresh = Baseline {
            at_ms: now_ms,
            quad,
        };
        let Some(base) = self.last else {
            self.last = Some(fresh);
            return None;
        };
        let dt_ms = now_ms.saturating_sub(base.at_ms);
        if dt_ms < MIN_INTERVAL_MS {
            return self.rate;
        }
        let delta = quad_delta(quad, base.quad).filter(|_| dt_ms <= MAX_INTERVAL_MS);
        self.last = Some(fresh);
        self.rate = delta.map(|bytes| rates_of(bytes, dt_ms));
        self.rate
    }
}

#[cfg(test)]
mod tests;
