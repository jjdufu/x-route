# 首页芯片电源：过渡流程

首页可点芯片与舞台走线随 `vmView()` 的 `stopped | starting | stopping | running | failed` 变化。动画只用 `transform` / `opacity` / `stroke-dashoffset`，遵循 Material 3 运动令牌，且须通过 `tests/test-webui.sh`（禁止呼吸光、大面积模糊、无限 scale/glow；电流关键帧只能改 `stroke-dashoffset`）。

舞台走线为 `gen-pcb.py` 生成的内联 SVG（`PCB_TRACES`），路径从芯片引脚向外。全屏底纹仍是静态红/绿平铺图，只做透明度交叉淡入淡出。启动不再使用瀑布日志。

令牌：

- 减速（emphasized-decelerate）：`cubic-bezier(0.05, 0.7, 0.1, 1)`
- 加速（emphasized-accelerate）：`cubic-bezier(0.3, 0, 0.8, 0.15)`
- 标准（standard）：`cubic-bezier(0.2, 0, 0, 1)`
- `prefers-reduced-motion: reduce`：任何切换跳到目标静止态（交叉淡入淡出底纹），不做分段追逐。

中途状态变化必须可打断：取消进行中的修复 rAF 与芯片 WAAPI，立刻跳到或改向目标进度。

## 启动（stopped / failed → starting → running）

| 阶段 | 触发 | 时长 | 缓动 | 动画 |
| --- | --- | --- | --- | --- |
| A 按下 | 指针按下芯片（无确认框） | 120ms | standard | 按钮 `scale(0.96)`，松手回 1 |
| B 修复 | `phase=starting`（已发出 `start-bg`） | 随真实启动（可数十秒） | 线性进度 | 芯片保持可见；走线按离芯片的距离由内向外把绿色 `stroke-dashoffset` 从全长收到 0，红色断开走线渐隐；电荷沿线往复（`pcbPulse`）。进度上限：step 0 约 58%，网络就绪约 84%，到达 `running` 才走完 |
| C 愈合 | 修复进度越过约 0.58，并在进入 `running` 时收尾 | 500ms | emphasized-decelerate | 碎块滑回，裂纹收合 |
| D 上电 / 运行 | `phase=running` 且走线接通 | 循环 1.6s / 2.2s | linear | 内部 `chipFlow` 与外侧 `pcbPulse` 只改 `stroke-dashoffset` |

## 停止（running → stopping → stopped）

先弹出「停止服务」确认，再 `stop-bg`。约 2s 内走线由外向内收回（进度 1→0），绿流停止，裂纹与碎块回到静止断开态。

## 失败

走线迅速回到红色断开，芯片保持裂纹，红走线短暂闪一下（只改 opacity，无发光、模糊、阴影）。错误文案以舞台内绝对定位的 `#power-err` 显示。顶栏徽章为「异常」。整机复位走原有 `#trip` 卡片。

## 静止态

- `stopped` / `failed`：深灰裂纹芯片，表面红走线在裂缝处断开；舞台红线带缺口，全静态。
- `running`：完整芯片，内部电路可见，绿光沿线流动。
- `starting`：芯片可见，走线由内向外逐步接通，电荷往复，直至进入 running。
- `stopping`：反向收线，结束即为 `stopped`。
