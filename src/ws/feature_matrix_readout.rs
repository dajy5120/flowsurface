//! 特征矩阵面板 — 只读数据（docs/31 §8.1）。
//!
//! 读 `~/ws-data/cockpit/feature_matrix.json`，由主仓 `wealthspring-features`
//! 的 `sidecar::SidecarWriter` 周期性写出。**零交易所连接、零计算**——
//! 面板不重算任何特征值。
//!
//! ## 为什么面板一个数都不算
//!
//! 这是 W7 验收标准（「面板与引擎对同一时刻的值一致」）唯一能立住的做法。
//! 只要面板里有第二条计算路径，两边就会漂——docs/20 的教训是这类
//! 不报错的口径漂移最贵。所以这里只做一件事：把 JSON 的字段搬成结构体。
//!
//! 代价是面板的**质量列、z、分位全由引擎决定**，面板不能「顺手补一个 z」。
//! 那正是要的：引擎说 `UNAVAILABLE` 的地方面板就得显示不可用，不能填 0。

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

/// 矩阵里的一行 = 一个 (特征, 窗口) slot。字段与主仓 `engine::SnapshotSlot` 一一对应。
#[derive(Clone, Default, Debug, PartialEq)]
pub struct Slot {
    pub key: String,
    pub name_cn: String,
    pub stage: String,
    pub family: String,
    pub unit: String,
    /// 需要的数据层徽标（`BBO` / `trades` / `L2` / …）。
    pub inputs: Vec<String>,
    pub markets: Vec<String>,
    pub window_ms: u32,
    /// 值。**`None` 与 `0.0` 是两件不同的事**，所以这里是 `Option` 而不是 `f64`——
    /// `unwrap_or(0.0)` 会把「这个市场没有成交流」显示成「净量恰好为零」。
    pub value: Option<f64>,
    pub z: Option<f64>,
    pub percentile: Option<f64>,
    /// `GOOD` / `DEGRADED` / `STALE` / `INVALID` / `UNAVAILABLE`。
    pub quality: String,
    /// 降级/不可用的原因（`insufficient_samples` / `capability_missing` / …）。
    pub reason: String,
    /// `spec` = 已登记·待实现，`impl` / `verified` = 已实现。
    pub status: String,
    pub wave: u8,
}

impl Slot {
    /// 窗口的可读写法。`0` 是**瞬时量**而不是「零秒窗口」。
    #[must_use]
    pub fn window_label(&self) -> String {
        match self.window_ms {
            0 => "瞬时".into(),
            w if w % 60_000 == 0 => format!("{}m", w / 60_000),
            w if w % 1_000 == 0 => format!("{}s", w / 1_000),
            w => format!("{w}ms"),
        }
    }

    /// 已登记但尚未实现。面板要**照样显示**它（docs/31 §8.1）——
    /// 藏起来会让人以为矩阵里的就是全部，于是「以为实现了其实没有」。
    #[must_use]
    pub fn not_implemented(&self) -> bool {
        self.status == "spec"
    }

    /// 质量是否异常（非 `GOOD`）。实时向量视图按它高亮。
    #[must_use]
    pub fn abnormal(&self) -> bool {
        !self.quality.is_empty() && self.quality != "GOOD"
    }
}

/// 一条共享窗口缓冲的容量健康。
#[derive(Clone, Default, Debug)]
pub struct PoolWindow {
    pub observable: String,
    pub window_ms: u32,
    pub len: usize,
    pub capacity: usize,
    pub drops: u64,
    pub saturated: bool,
}

#[derive(Clone, Default, Debug)]
pub struct Matrix {
    /// 快照文件存在且可解析。
    pub present: bool,
    pub symbol_id: u32,
    /// 快照时刻（事件时钟，ns）。
    pub as_of: u64,
    pub event_count: u64,
    pub session_kind: String,
    pub session_bucket: Option<u8>,
    pub placeholder_count: u64,
    pub book_sync: String,
    pub book_depth: (usize, usize),
    pub book_desync_count: u64,
    pub book_uncrossed_levels: u64,
    pub book_truncated_levels: u64,
    pub sweep_groups: u64,
    pub iceberg_refills: u32,
    pub regime: String,
    pub vpin_buckets: u64,
    pub markout_pending: usize,
    pub refreshes_per_event: f64,
    pub usable_slots: usize,
    pub total_slots: usize,
    pub pool_windows: usize,
    pub window_drops: u64,
    pub saturated_windows: usize,
    pub pool_detail: Vec<PoolWindow>,
    pub slots: Vec<Slot>,
    /// 面板最后一次重新解析的墙钟时刻（只是「这一帧读到了」的凭据，不是数据时间）。
    pub refreshed: String,
}

impl Matrix {
    /// 七阶段的固定顺序。**按字典序排会把 S10 排在 S2 前面**，而阶段是有依赖次序的
    /// （S1 价格 → … → S7 执行），乱序的矩阵读起来就不是一条管线了。
    pub const STAGES: [(&'static str, &'static str); 7] = [
        ("S1_price", "S1 价格"),
        ("S2_trade", "S2 成交"),
        ("S3_book", "S3 买卖盘"),
        ("S4_liquidity", "S4 流动性"),
        ("S5_order_behavior", "S5 订单行为"),
        ("S6_regime", "S6 市场状态"),
        ("S7_execution", "S7 交易执行"),
    ];

    /// 某阶段的 slot。
    #[must_use]
    pub fn by_stage(&self, stage: &str) -> Vec<&Slot> {
        self.slots.iter().filter(|s| s.stage == stage).collect()
    }

    /// 按质量计数。
    #[must_use]
    pub fn quality_counts(&self) -> Vec<(String, usize)> {
        let mut out: Vec<(String, usize)> = Vec::new();
        for s in &self.slots {
            match out.iter_mut().find(|(q, _)| *q == s.quality) {
                Some((_, n)) => *n += 1,
                None => out.push((s.quality.clone(), 1)),
            }
        }
        out.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
        out
    }

    /// 已登记待实现的条数。
    #[must_use]
    pub fn not_implemented(&self) -> usize {
        self.slots.iter().filter(|s| s.not_implemented()).count()
    }

    /// 全部出现过的族，去重升序（「类别」筛选的取值来源）。
    #[must_use]
    pub fn families(&self) -> Vec<String> {
        let mut v: Vec<String> = self.slots.iter().map(|s| s.family.clone()).collect();
        v.sort();
        v.dedup();
        v
    }

    /// 全部出现过的市场，去重（「市场」筛选的取值来源）。
    #[must_use]
    pub fn markets(&self) -> Vec<String> {
        let mut v: Vec<String> = self.slots.iter().flat_map(|s| s.markets.clone()).collect();
        v.sort();
        v.dedup();
        v
    }
}

static CACHE: OnceLock<Mutex<(Option<SystemTime>, Matrix)>> = OnceLock::new();

/// 面板读的旁路文件。**必须与主仓 `sidecar::board_path()` 算出同一个路径**——
/// 两边各写一份默认值，表现是面板说「暂无快照」而引擎日志显示一直在写。
pub fn board_path() -> PathBuf {
    std::env::var("WS_FEATURE_MATRIX")
        .map(PathBuf::from)
        .unwrap_or_else(|_| super::paths::data_dir().join("cockpit").join("feature_matrix.json"))
}

pub fn snapshot() -> Matrix {
    let lock = CACHE.get_or_init(|| Mutex::new((None, Matrix::default())));
    let Ok(mut g) = lock.lock() else { return Matrix::default() };
    let p = board_path();
    let mt = std::fs::metadata(&p).ok().and_then(|m| m.modified().ok());
    if g.0.is_some() && g.0 == mt {
        return g.1.clone();
    }
    let mut v = match std::fs::read_to_string(&p) {
        Ok(t) => parse(&t),
        Err(_) => Matrix::default(),
    };
    v.refreshed = chrono::Local::now().format("%H:%M:%S").to_string();
    *g = (mt, v.clone());
    v
}

fn strings(v: &serde_json::Value) -> Vec<String> {
    v.as_array()
        .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

pub fn parse(text: &str) -> Matrix {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else {
        return Matrix::default();
    };
    // 没有 `slots` 的 JSON 不是这个面板的快照（很可能指错了文件）。
    // 当成「暂无」而不是「一个空矩阵」——后者会显示成「0 个特征全部健康」。
    if !v["slots"].is_array() {
        return Matrix::default();
    }
    let mut out = Matrix {
        present: true,
        symbol_id: v["symbol_id"].as_u64().unwrap_or(0) as u32,
        as_of: v["as_of"].as_u64().unwrap_or(0),
        event_count: v["event_count"].as_u64().unwrap_or(0),
        session_kind: v["session_kind"].as_str().unwrap_or_default().to_string(),
        session_bucket: v["session_bucket"].as_u64().map(|x| x as u8),
        placeholder_count: v["placeholder_count"].as_u64().unwrap_or(0),
        book_sync: v["book_sync"].as_str().unwrap_or_default().to_string(),
        book_depth: {
            let d = &v["book_depth"];
            (
                d[0].as_u64().unwrap_or(0) as usize,
                d[1].as_u64().unwrap_or(0) as usize,
            )
        },
        book_desync_count: v["book_desync_count"].as_u64().unwrap_or(0),
        book_uncrossed_levels: v["book_uncrossed_levels"].as_u64().unwrap_or(0),
        book_truncated_levels: v["book_truncated_levels"].as_u64().unwrap_or(0),
        sweep_groups: v["sweep_groups"].as_u64().unwrap_or(0),
        iceberg_refills: v["iceberg_refills"].as_u64().unwrap_or(0) as u32,
        regime: v["regime"].as_str().unwrap_or_default().to_string(),
        vpin_buckets: v["vpin_buckets"].as_u64().unwrap_or(0),
        markout_pending: v["markout_pending"].as_u64().unwrap_or(0) as usize,
        refreshes_per_event: v["refreshes_per_event"].as_f64().unwrap_or(0.0),
        usable_slots: v["usable_slots"].as_u64().unwrap_or(0) as usize,
        total_slots: v["total_slots"].as_u64().unwrap_or(0) as usize,
        pool_windows: v["pool_windows"].as_u64().unwrap_or(0) as usize,
        window_drops: v["window_drops"].as_u64().unwrap_or(0),
        saturated_windows: v["saturated_windows"].as_u64().unwrap_or(0) as usize,
        ..Default::default()
    };
    for s in v["slots"].as_array().into_iter().flatten() {
        out.slots.push(Slot {
            key: s["key"].as_str().unwrap_or_default().to_string(),
            name_cn: s["name_cn"].as_str().unwrap_or_default().to_string(),
            stage: s["stage"].as_str().unwrap_or_default().to_string(),
            family: s["family"].as_str().unwrap_or_default().to_string(),
            unit: s["unit"].as_str().unwrap_or_default().to_string(),
            inputs: strings(&s["inputs"]),
            markets: strings(&s["markets"]),
            window_ms: s["window_ms"].as_u64().unwrap_or(0) as u32,
            // `as_f64()` 对 JSON `null` 返回 `None`——这正是要的语义。
            value: s["value"].as_f64(),
            z: s["z"].as_f64(),
            percentile: s["percentile"].as_f64(),
            quality: s["quality"].as_str().unwrap_or_default().to_string(),
            reason: s["reason"].as_str().unwrap_or_default().to_string(),
            status: s["status"].as_str().unwrap_or_default().to_string(),
            wave: s["wave"].as_u64().unwrap_or(0) as u8,
        });
    }
    for w in v["pool_detail"].as_array().into_iter().flatten() {
        out.pool_detail.push(PoolWindow {
            observable: w["observable"].as_str().unwrap_or_default().to_string(),
            window_ms: w["window_ms"].as_u64().unwrap_or(0) as u32,
            len: w["len"].as_u64().unwrap_or(0) as usize,
            capacity: w["capacity"].as_u64().unwrap_or(0) as usize,
            drops: w["drops"].as_u64().unwrap_or(0),
            saturated: w["saturated"].as_bool().unwrap_or(false),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
      "symbol_id":1,"as_of":1789780799422000000,"event_count":1827684,
      "session_kind":"continuous","session_bucket":1,"placeholder_count":381,
      "book_sync":"in_sync","book_depth":[200,200],"book_desync_count":0,
      "book_uncrossed_levels":15037,"book_truncated_levels":960327,
      "sweep_groups":2546,"iceberg_refills":1,"regime":"calm_trend",
      "vpin_buckets":4671,"markout_pending":8,"refreshes_per_event":102.4,
      "usable_slots":93,"total_slots":109,"pool_windows":68,
      "window_drops":4097952,"saturated_windows":2,
      "pool_detail":[{"observable":"Ofi","window_ms":30000,"len":26854,
                      "capacity":32768,"drops":431906,"saturated":true}],
      "slots":[
        {"key":"mid_price","name_cn":"中间价","stage":"S1_price","family":"price",
         "unit":"price","inputs":["BBO"],"markets":["crypto_perp","equity"],
         "window_ms":0,"value":81138.35,"z":null,"percentile":null,
         "quality":"GOOD","reason":null,"status":"impl","wave":1},
        {"key":"trade_delta","name_cn":"成交净量","stage":"S2_trade","family":"delta",
         "unit":"qty","inputs":["trades"],"markets":["crypto_perp"],
         "window_ms":1000,"value":null,"z":null,"percentile":null,
         "quality":"UNAVAILABLE","reason":"capability_missing","status":"impl","wave":1},
        {"key":"l3_queue_pos","name_cn":"队列位置","stage":"S5_order_behavior",
         "family":"queue","unit":"count","inputs":["L3"],"markets":["crypto_perp"],
         "window_ms":0,"value":null,"z":null,"percentile":null,
         "quality":"UNAVAILABLE","reason":"offline_only","status":"spec","wave":9}
      ]}"#;

    #[test]
    fn 解析完整矩阵() {
        let m = parse(SAMPLE);
        assert!(m.present);
        assert_eq!(m.slots.len(), 3);
        assert_eq!(m.usable_slots, 93);
        assert_eq!(m.total_slots, 109);
        assert_eq!(m.book_depth, (200, 200));
        assert_eq!(m.pool_detail.len(), 1);
        assert!(m.pool_detail[0].saturated);
    }

    #[test]
    fn 缺值必须是none而不是零() {
        // 这条是整个面板最要紧的一条断言：`trade_delta` 在没有成交流的市场上
        // 是**不可用**，不是「净量为 0」。读成 0 的话面板会显示一个完全正常的数字。
        let m = parse(SAMPLE);
        let d = m.slots.iter().find(|s| s.key == "trade_delta").unwrap();
        assert_eq!(d.value, None, "null 被读成了 0——缺数据与零被混成一件事");
        assert_eq!(d.quality, "UNAVAILABLE");
        assert_eq!(d.reason, "capability_missing");
    }

    #[test]
    fn 已登记待实现要能判得出来并且照样显示() {
        let m = parse(SAMPLE);
        assert_eq!(m.not_implemented(), 1);
        let s = m.slots.iter().find(|s| s.key == "l3_queue_pos").unwrap();
        assert!(s.not_implemented());
        // 待实现的不能从 slots 里被过滤掉——藏起来会让人以为矩阵就是全部。
        assert_eq!(m.slots.len(), 3);
    }

    #[test]
    fn 阶段顺序是管线顺序不是字典序() {
        let names: Vec<&str> = Matrix::STAGES.iter().map(|(k, _)| *k).collect();
        assert_eq!(names[0], "S1_price");
        assert_eq!(names[6], "S7_execution");
        // 每个阶段键都得能在真实快照里匹配上，否则那一节永远空着。
        let m = parse(SAMPLE);
        assert_eq!(m.by_stage("S1_price").len(), 1);
        assert_eq!(m.by_stage("S2_trade").len(), 1);
    }

    #[test]
    fn 窗口标签把瞬时和零秒分开() {
        let m = parse(SAMPLE);
        let inst = m.slots.iter().find(|s| s.key == "mid_price").unwrap();
        assert_eq!(inst.window_label(), "瞬时", "0 不是「0 秒窗口」而是瞬时量");
        let w = m.slots.iter().find(|s| s.key == "trade_delta").unwrap();
        assert_eq!(w.window_label(), "1s");
    }

    #[test]
    fn 异常质量要能判得出来() {
        let m = parse(SAMPLE);
        assert!(!m.slots[0].abnormal());
        assert!(m.slots[1].abnormal());
        let qc = m.quality_counts();
        assert_eq!(qc.iter().map(|(_, n)| n).sum::<usize>(), 3);
    }

    #[test]
    fn 坏json不当成空矩阵() {
        assert!(!parse("{不是 json").present);
        // 指到了另一个面板的快照文件：没有 slots，必须判为「暂无」而不是
        // 「0 个特征、全部健康」——后者是一个看起来很正常的假象。
        assert!(!parse(r#"{"ready":true,"rows":[]}"#).present);
    }

    /// W7 验收用的**真实**快照：30 分钟录制的 BTCUSDT 永续重放到引擎之后，
    /// 由 `sidecar::SidecarWriter` 写出的那一份原文（109 个 slot / 68 条缓冲）。
    ///
    /// 手写的 `SAMPLE` 守语义，这一份守**格式**：字段名、null 的位置、
    /// 数量级、枚举取值全是引擎真的会写出来的样子。
    const REAL: &str = include_str!("testdata/feature_matrix.json");

    #[test]
    fn 面板与引擎对同一时刻的值一致() {
        // ── W7 验收（docs/31 §9）────────────────────────────────────────
        // 面板不算任何值，所以「一致」= 解析没有丢、没有默认、没有把 null 变成 0。
        // 这里拿**另一条独立路径**（直接遍历 serde_json::Value）与 `parse` 的结果
        // 逐字段对拍。只断言「解析没 panic」是抓不到东西的：读错一个键名的后果
        // 正是安静地得到 `0` 或空串。
        let m = parse(REAL);
        assert!(m.present);
        let v: serde_json::Value = serde_json::from_str(REAL).unwrap();
        let raw = v["slots"].as_array().unwrap();

        assert_eq!(m.slots.len(), raw.len(), "解析丢了 slot");
        // 引擎自报的总数也要对上——它是独立算出来的（`rows.len()`）。
        assert_eq!(m.total_slots, m.slots.len(), "引擎自报 slot 数与实际行数不符");

        let mut checked_null = 0;
        let mut checked_val = 0;
        for (i, r) in raw.iter().enumerate() {
            let s = &m.slots[i];
            assert_eq!(s.key, r["key"].as_str().unwrap(), "第 {i} 行 key");
            assert_eq!(s.name_cn, r["name_cn"].as_str().unwrap());
            assert_eq!(s.stage, r["stage"].as_str().unwrap());
            assert_eq!(s.family, r["family"].as_str().unwrap());
            assert_eq!(s.unit, r["unit"].as_str().unwrap());
            assert_eq!(s.quality, r["quality"].as_str().unwrap());
            assert_eq!(s.status, r["status"].as_str().unwrap());
            assert_eq!(s.window_ms as u64, r["window_ms"].as_u64().unwrap());
            assert_eq!(s.wave as u64, r["wave"].as_u64().unwrap());
            assert_eq!(s.inputs.len(), r["inputs"].as_array().unwrap().len());
            assert_eq!(s.markets.len(), r["markets"].as_array().unwrap().len());
            // 三个可空数值列：null ↔ None 必须一一对应，**不能有一边变成 0**。
            for (got, key) in [(s.value, "value"), (s.z, "z"), (s.percentile, "percentile")] {
                if r[key].is_null() {
                    assert_eq!(got, None, "第 {i} 行 {key}：null 被读成了 {got:?}");
                    checked_null += 1;
                } else {
                    let want = r[key].as_f64().unwrap();
                    assert_eq!(got, Some(want), "第 {i} 行 {key} 值不一致");
                    // 逐位一致：`f64` 直接比而不是比到某个 epsilon——
                    // JSON 往返之后应当是同一个数，「差一点」说明格式化丢了精度。
                    assert_eq!(got.unwrap().to_bits(), want.to_bits(), "第 {i} 行 {key} 差了低位");
                    checked_val += 1;
                }
            }
            // reason 的 null 读成空串是刻意的（面板要拼串），但**不能反过来**：
            // 有 reason 的时候必须原样带过来。
            match r["reason"].as_str() {
                Some(x) => assert_eq!(s.reason, x),
                None => assert!(s.reason.is_empty()),
            }
        }
        // 两条路径都得真的走到过，否则这个测试是空的。
        assert!(checked_null > 0, "这份快照里没有 null，测不到缺值语义");
        assert!(checked_val > 50, "只对拍到 {checked_val} 个实数，样本太少");

        // 顶栏那几个标量同样要对上——它们决定面板第一句话说什么。
        assert_eq!(m.usable_slots as u64, v["usable_slots"].as_u64().unwrap());
        assert_eq!(m.event_count, v["event_count"].as_u64().unwrap());
        assert_eq!(m.as_of, v["as_of"].as_u64().unwrap());
        assert_eq!(m.window_drops, v["window_drops"].as_u64().unwrap());
        assert_eq!(m.pool_detail.len(), v["pool_detail"].as_array().unwrap().len());
    }

    #[test]
    fn 真实快照里的可用数与逐行质量对得上() {
        // 引擎的 `usable_slots` 是从 `FeatureVector` 独立数出来的，
        // 而面板会按逐行的 quality 自己数一遍（顶栏的质量分布）。两者必须相等——
        // 不等说明某一边把 DEGRADED 算进了「可用」。
        let m = parse(REAL);
        let good = m.slots.iter().filter(|s| s.quality == "GOOD").count();
        assert_eq!(
            good, m.usable_slots,
            "逐行数出 {good} 个 GOOD，引擎自报可用 {}",
            m.usable_slots
        );
        // 这份快照是 30 分钟真实数据，绝大多数 slot 应当已经热起来了。
        assert!(good > 80, "真实快照里只有 {good} 个 GOOD，引擎侧多半有问题");
    }

    #[test]
    fn 真实快照的七个阶段都非空() {
        // 少一个阶段说明 `STAGES` 里的键与引擎写出来的串对不上——
        // 后果是那一节在面板上永远空白，而引擎一直在算。
        let m = parse(REAL);
        for (k, label) in Matrix::STAGES {
            assert!(!m.by_stage(k).is_empty(), "{label}（{k}）在真实快照里没有任何 slot");
        }
        // 反过来：快照里出现的阶段必须都被 STAGES 覆盖，否则那些 slot 哪一节都进不去。
        let known: Vec<&str> = Matrix::STAGES.iter().map(|(k, _)| *k).collect();
        for s in &m.slots {
            assert!(known.contains(&s.stage.as_str()), "阶段 {} 没有对应分节", s.stage);
        }
    }

    #[test]
    fn 筛选取值来自数据而不是写死() {
        let m = parse(SAMPLE);
        assert_eq!(m.families(), vec!["delta", "price", "queue"]);
        assert_eq!(m.markets(), vec!["crypto_perp", "equity"]);
    }
}
