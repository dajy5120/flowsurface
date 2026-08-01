//! WealthSpring 集成层（docs/08，策略 B）。
//!
//! 自包含读 Redis：① 控制面 `ws:active_run`（P0 广播的活动 run → P2 三态切换）；
//! ② 回测逐笔成交流 `ws:bt:{run}:trades`（→ 喂 FS 图表，实现回测行情入图）。
//! 不走 git 依赖主仓（私有 + 含本 fork 子模块 → 循环）；wire 契约简单稳定（docs/03/07/08）。

#![allow(dead_code)] // F1 先落读取层；replay 订阅 / 三态接线见后续阶段

pub mod active_run;
pub mod backtest_readout;
pub mod backtest_view;
pub mod c4_readout;
pub mod c4_view;
pub mod options_readout;
pub mod options_view;
pub mod prediction_readout;
pub mod prediction_view;
pub mod customchart;
pub mod bt_trades;
pub mod factory;
pub mod factory_readout;
pub mod factory_view;
pub mod flow;
pub mod orders;
pub mod readout;
pub mod recorder;
pub mod recorder_readout;
pub mod recorder_view;
pub mod replay;
pub mod selfdata;
pub mod signals;
pub mod tardis_replay;
pub mod tardis_replay_readout;
pub mod tardis_replay_view;
pub mod view;
pub mod workspace;
