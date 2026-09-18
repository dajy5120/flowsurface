//! WealthSpring 集成层（docs/08，策略 B）。
//!
//! 自包含读 Redis：① 控制面 `ws:active_run`（P0 广播的活动 run → P2 三态切换）；
//! ② 回测逐笔成交流 `ws:bt:{run}:trades`（→ 喂 FS 图表，实现回测行情入图）。
//! 不走 git 依赖主仓（私有 + 含本 fork 子模块 → 循环）；wire 契约简单稳定（docs/03/07/08）。

#![allow(dead_code)] // F1 先落读取层；replay 订阅 / 三态接线见后续阶段

pub mod active_run;
pub mod backtest_readout;
pub mod backtest_view;
pub mod c4;
pub mod c4_readout;
pub mod deps;
pub mod egress;
pub mod paths;
pub mod lifecycle;
pub mod procs;
pub mod provenance;
pub mod procs_view;
pub mod egress_view;
pub mod c4_view;
pub mod observatory;
pub mod observatory_lib;
pub mod observatory_hist;
pub mod observatory_readout;
pub mod observatory_table;
pub mod observatory_view;
pub mod news;
pub mod news_readout;
pub mod news_view;
pub mod orders_view;
pub mod options_readout;
pub mod options_view;
pub mod prediction;
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
pub mod staleness;
pub mod svcctl;
pub mod radar;
pub mod radar_filter;
pub mod radar_readout;
pub mod radar_view;
pub mod treemap;
pub mod tardis_board;
pub mod tardis_board_readout;
pub mod tardis_board_view;
pub mod tardis_replay;
pub mod tardis_replay_readout;
pub mod tardis_replay_view;
pub mod view;
pub mod workspace;

#[cfg(test)]
mod subscription_liveness_tests {
    //! 订阅线程的对端存活探测（RELEASE_REMEDIATION_PLAN C-01）。
    //!
    //! 六个 `pub fn subscription` 各自 spawn 一条 OS 线程轮询数据源，通过
    //! `tokio::mpsc` 把结果交回 iced 的 subscription stream。订阅被销毁时
    //! 接收端 drop，线程必须自己发现并退出——否则每次销毁泄漏一条线程 + 一条
    //! Redis 连接。
    //!
    //! **原先六个全都发现不了**，三种形态：
    //! ① 唯一的检测点 `tx.blocking_send(..).is_err()` 嵌在「值变了」分支里
    //!    （signals / factory / active_run / selfdata）——值冻住就永不检查，
    //!    而**值冻住恰恰是上游守护挂掉的表现**；
    //! ② `let Some(c) = .. else { sleep; continue; }` 这条路径完全不碰 `tx`
    //!    （replay / orders）——没有活动 run 时是常态；
    //! ③ `let _ = tx.blocking_send(..)` 直接丢弃错误（orders）。
    //!
    //! 统一改法：每轮循环**开头**无条件 `if tx.is_closed() { break; }`，
    //! 一行覆盖全部路径（含 `continue`），不重写成 async。

    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    /// 机制自证：值**从不变化**、接收端 drop → 线程必须退出。
    /// 这一条钉住的是 `tokio::mpsc::Sender::is_closed()` 的语义本身——
    /// 整个修法都建立在它之上。
    #[test]
    fn a_polling_thread_exits_when_the_receiver_is_dropped() {
        let (tx, rx) = tokio::sync::mpsc::channel::<u8>(1);
        let done = Arc::new(AtomicBool::new(false));
        let flag = done.clone();

        let h = std::thread::spawn(move || {
            loop {
                if tx.is_closed() {
                    break;
                }
                // 注意：**一次都不发送**——模拟「值冻住」。
                std::thread::sleep(Duration::from_millis(2));
            }
            flag.store(true, Ordering::SeqCst);
        });

        drop(rx); // 订阅被销毁
        let t0 = Instant::now();
        h.join().expect("线程应当退出，而不是永久空转");
        assert!(done.load(Ordering::SeqCst));
        assert!(t0.elapsed() < Duration::from_secs(5), "退出太慢");
    }

    /// 会红的守卫：**每个 `pub fn subscription` 所在的文件都必须有这道探测**。
    /// 将来新增第七个订阅时漏掉，这条测试会红——而不是安静地多一个泄漏点。
    #[test]
    fn every_subscription_module_probes_the_receiver() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/ws");
        let Ok(rd) = std::fs::read_dir(&dir) else {
            return; // 非源码树布局（如从 crates.io 构建）就跳过
        };
        let mut checked = 0;
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|s| s.to_str()) != Some("rs") {
                continue;
            }
            let Ok(src) = std::fs::read_to_string(&p) else { continue };
            if !src.contains("pub fn subscription") || !src.contains("thread::spawn") {
                continue;
            }
            checked += 1;
            assert!(
                src.contains("tx.is_closed()"),
                "{} 有 subscription + spawn 但没有对端存活探测——\
                 每轮循环开头加 `if tx.is_closed() {{ break; }}`（C-01）",
                p.file_name().unwrap().to_string_lossy()
            );
        }
        assert!(checked >= 6, "只扫到 {checked} 个订阅模块，预期至少 6 个——扫描逻辑可能失效了");
    }
}

#[cfg(test)]
mod data_lake_date_tests {
    //! 数据湖索引日期必须与写入端同口径（RELEASE_REMEDIATION_PLAN F-01）。
    //!
    //! 录制器把 parquet 落在 `~/ws-data/raw/{stream}/{SYM}/{YYYY-MM-DD}/` 下，
    //! 目录名由**数据自己的 UTC 时间戳**决定
    //! （`crates/wealthspring-recorder/src/writer.rs` 的 `partition_date`，
    //!  那边有一条对应的行为测试 `partition_date_is_utc_not_local`）。
    //!
    //! 面板这一侧要算「今天」去索引那些目录。**只要有一边用本地时间，
    //! 在本地日界前后就会各错位几小时——而且不报错**，只是「今日行数」
    //! 显示成 0 或翻倍。这正是本项目反复栽的静默错误。
    //!
    //! 这条守卫是源码级的：面板侧的「今天」是内联算的，没有可测函数，
    //! 而为了测它去重构一个跨 workspace 的公共函数，代价远大于收益
    //! （fork 刻意不依赖任何 wealthspring crate，见 Do Not Change）。

    /// 扫数据湖的模块里，**算日期不许用 `Local`**。
    #[test]
    fn lake_readers_compute_dates_in_utc() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/ws");
        let Ok(rd) = std::fs::read_dir(&dir) else {
            return; // 非源码树布局就跳过
        };
        let mut checked = 0;
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|s| s.to_str()) != Some("rs") {
                continue;
            }
            // 跳过本文件：这条测试自己的源码里就有那个字面量锚点，
            // 不跳过就会扫到自己（源码级扫描的经典自指问题，实测踩到）。
            if p.file_name().and_then(|n| n.to_str()) == Some("mod.rs") {
                continue;
            }
            let Ok(src) = std::fs::read_to_string(&p) else { continue };
            // 匹配锚点必须**精确**：`.join("raw")` 是数据湖根目录的实际拼接。
            // 初版写的是 `contains("raw") && contains("%Y-%m-%d")`，当场误报——
            // `customchart.rs` 因为一个叫 `xs_raw` 的变量名和一个日期**解析**格式
            // 被判成数据湖读者。**源码级守卫的锚点不精确，就会变成噪音源，
            // 而一条会乱叫的守卫比没有守卫更糟：人会开始无视它。**
            if !src.contains(".join(\"raw\")") {
                continue;
            }
            checked += 1;
            let name = p.file_name().unwrap().to_string_lossy().to_string();
            assert!(
                !src.contains("Local::now().format(\"%Y-%m-%d\")"),
                "{name} 用本地时间算数据湖日期——录制器按 **UTC** 分目录，\
                 两边不同口径会在本地日界前后错位几小时且不报错（F-01）"
            );
            assert!(
                src.contains("Utc::now().format(\"%Y-%m-%d\")"),
                "{name} 扫数据湖却没有显式的 UTC 日期计算——\
                 若确实不需要算「今天」，把这个文件从本测试的匹配条件里排除并说明理由"
            );
        }
        assert!(
            checked >= 1,
            "一个扫数据湖的模块都没扫到——匹配条件可能失效了"
        );
    }
}
