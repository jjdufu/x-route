# 首页芯片电源：过渡流程

首页大圆圈已改为可点芯片。状态机仍由 `vmView()` 给出：`stopped | starting | stopping | running | failed`。动画只用 `transform` / `opacity` / `stroke-dashoffset`，遵循 Material 3 运动令牌，且须通过 `tests/test-webui.sh`（禁止呼吸光、大面积模糊、无限 scale/glow、`pcbFlow` 只能改 transform）。

令牌：

- 减速（emphasized-decelerate）：`cubic-bezier(0.05, 0.7, 0.1, 1)`
- 加速（emphasized-accelerate）：`cubic-bezier(0.3, 0, 0.8, 0.15)`
- 标准（standard）：`cubic-bezier(0.2, 0, 0, 1)`
- `prefers-reduced-motion: reduce`：任何切换只做 180–200ms 交叉淡入淡出，不做拆装、描线和流动。

中途状态变化必须可打断：取消进行中的 WAAPI，立刻跳到目标状态的静止外观。

## 启动（stopped / failed → starting → running）

| 阶段 | 触发 | 时长 | 缓动 | 动画 |
| --- | --- | --- | --- | --- |
| A 按下 | 指针按下芯片（无确认框） | 120ms | standard | 按钮 `scale(0.96)`，松手回 1 |
| B 让位 | `phase=starting`（已发出 `start-bg`） | 240ms | emphasized-decelerate | 裂纹芯片略缩小并变暗淡出（`scale(0.94)` + opacity 0，随后 `visibility: hidden`）；舞台电路板变暗 |
| C 日志入 | 与 B 尾部重叠约 80ms | 200ms | emphasized-decelerate | 瀑布日志在芯片原位淡入并略上滑到位；步骤条「正在启动 → 网络就绪 → 运行中」按 `vmView.step` 前进 |
| D 启动中 | `phase=starting` 持续 | 不定 | — | 日志按现有 boot-tail 瀑布滚动；芯片保持隐藏；无转圈、无电流 |
| E 日志收 | 进入 `phase=running` | 300ms | emphasized-accelerate | 日志向芯片中心收拢：上移并略缩小、淡出 |
| F 愈合 | E 结束后 | 500ms | emphasized-decelerate | 芯片淡入出现；碎块滑回本体；裂纹沿路径用 `stroke-dashoffset` 收合愈合 |
| G 上电 | F 结束后 | 400ms | standard | 内部走线由中心向外从红接到绿（`stroke-dashoffset` 错开）；引脚随之点亮 |
| H 运行 | G 结束后，静止态 | 循环 2.2s | linear | 内部路径与引脚外侧的稳定绿流（内部用 `stroke-dashoffset`，外侧沿用 `pcbFlow` 的 transform 脉冲） |

## 停止（running → stopping → stopped）

先弹出「停止服务」确认，再 `stop-bg`。

| 阶段 | 触发 | 时长 | 缓动 | 动画 |
| --- | --- | --- | --- | --- |
| S1 停流 | `phase=stopping` | 0–80ms | — | 内部与外侧绿流立即停住（`animation-play-state: paused` 或撤类） |
| S2 断电 | 紧接 S1 | 400ms | standard | 走线由外向内从绿收到红（`stroke-dashoffset` 反向） |
| S3 开裂 | 紧接 S2 | 500ms | emphasized-decelerate | 裂纹沿路径画出；右下碎块平移旋出；停在裂纹芯片静止态（无循环） |

若在 S2/S3 中途已变为 `stopped`，不重开动画，让当前这段走完或直接跳到裂纹静止态（以是否仍匹配这次停止为准）。无转圈环。

## 失败（starting → failed，或运行中崩溃到 failed）

| 阶段 | 触发 | 时长 | 缓动 | 动画 |
| --- | --- | --- | --- | --- |
| F1 日志出 | `phase=failed` | 180–200ms | emphasized-accelerate | 若仍在显示瀑布，日志淡出（不做收拢愈合） |
| F2 裂纹回 | 紧接 F1 | 200ms | emphasized-decelerate | 裂纹芯片淡入，碎块已在裂开位置 |
| F3 闪红 | 紧接 F2 | 180ms × 2 | standard | 表面红走线短暂提高透明度再恢复（只改 opacity，无发光、模糊、阴影） |

之后保持裂纹静止态。`stopped` 与 `failed` 的芯片造型相同，失败多一次闪红，文案走 `#power-text` 的错误色。

## 静止态

- `stopped` / `failed`：深灰裂纹芯片，表面红走线在裂缝处断开，右下有脱落碎块；底图为红色断开走线，全静态。
- `running`：完整芯片，内部电路可见，绿光沿内部路径流向引脚并接到外侧电流。
- `starting`：芯片不可见，只显示瀑布与步骤条。
- `stopping`：过渡态，见上表；结束即为 `stopped` 静止态。
