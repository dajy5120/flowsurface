//! 回放时间线的读取（docs/51 P2）：主仓 `replay_source --timeline` 写出的 `tl_*`（布局见主仓
//! `wealthspring_features::timeline_out`，Python 读取器 `factory/replay/timeline.py` 是同一份布局）。
//!
//! 全部按接收时刻 `ti`（纳秒）排好序，**边算边可读**：定长记录按文件长度取整条（末尾半条不读），
//! jsonl 只读以换行结尾的行；**frontier = 栅格文件最后一个完整行的 `ti`**，它之前的成交、深度、事件都已经在盘上。
//! 这里按位置读（`read_exact_at`）+ 二分，不把整份读进内存——30 分钟全开的栅格有 200 MB。
//!
//! 不依赖主仓 crate（子模块循环，见 `super`）：布局在这里照写一份，开文件时拿 `tl_meta.json` 里的记录长度核对。

use std::fs::File;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// 成交记录长度（`ti i64, te i64, px f64, qty f64, side i8` + 填充）。
const TRADE_REC: u64 = 40;

#[derive(Debug, Clone, Default)]
pub struct Meta {
    pub symbol: String,
    pub vendor: String,
    pub t0: i64,
    pub t1: i64,
    pub tick: f64,
    pub lot: f64,
    pub grid_ms: u64,
    pub columns: Vec<String>,
    pub quality_codes: Vec<String>,
    pub reason_codes: Vec<String>,
    pub depth_levels: usize,
    pub done: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TradeRec {
    pub ti: i64,
    pub te: i64,
    pub px: f64,
    pub qty: f64,
    /// 1 主动买 / −1 主动卖 / 0 不知道
    pub side: i8,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DepthRec {
    pub ti: i64,
    pub te: i64,
    /// 最优在前
    pub bids: Vec<(f64, f64)>,
    pub asks: Vec<(f64, f64)>,
}

/// 栅格的一行（与 `Meta::columns` 同序）。
#[derive(Debug, Clone, PartialEq)]
pub struct GridRow {
    pub ti: i64,
    pub te: i64,
    pub seq: u64,
    pub val: Vec<f64>,
    pub z: Vec<f32>,
    pub pct: Vec<f32>,
    pub q: Vec<u8>,
    /// 0 = 没有原因，否则 `Meta::reason_codes[r - 1]`
    pub reason: Vec<u8>,
}

/// 事件（OFMS 与引擎检出）。
#[derive(Debug, Clone, PartialEq)]
pub struct EventRec {
    /// 检出的接收时刻（纳秒）
    pub ti: i64,
    /// 事件自己的时刻（纳秒；检测器有确认窗口，可能比 `ti` 早）
    pub ts: i64,
    pub kind: String,
    pub dir: i8,
    pub px: Option<f64>,
    pub size: Option<f64>,
}

/// jsonl 的增量索引：已经读过的完整行（`ti`、偏移、长度），下次从读到的地方接着读。
#[derive(Default)]
struct LineIndex {
    pos: u64,
    lines: Vec<(i64, u64, u32)>,
}

impl LineIndex {
    fn refresh(&mut self, path: &Path) {
        let Ok(f) = File::open(path) else { return };
        let len = f.metadata().map_or(0, |m| m.len());
        if len < self.pos {
            // 文件被重写（换了一次回放）：从头来
            *self = Self::default();
        }
        if len == self.pos {
            return;
        }
        let mut buf = vec![0u8; (len - self.pos) as usize];
        if f.read_exact_at(&mut buf, self.pos).is_err() {
            return;
        }
        let mut start = 0usize;
        while let Some(nl) = buf[start..].iter().position(|b| *b == b'\n') {
            let line = &buf[start..start + nl];
            if let Some(ti) = ti_of(line) {
                self.lines.push((ti, self.pos + start as u64, nl as u32));
            }
            start += nl + 1;
        }
        self.pos += start as u64;
    }

    /// `ti ≤ t` 的最后一行。
    fn last_at(&self, t: i64) -> Option<(u64, u32)> {
        let i = self.lines.partition_point(|l| l.0 <= t);
        (i > 0).then(|| (self.lines[i - 1].1, self.lines[i - 1].2))
    }
}

/// 一行 jsonl 的 `"ti":` 字段（不整行解析：状态行有 15 KB）。
fn ti_of(line: &[u8]) -> Option<i64> {
    let s = std::str::from_utf8(line).ok()?;
    let i = s.find("\"ti\":")? + 5;
    let rest = s[i..].trim_start();
    let end = rest.find(|c: char| !(c.is_ascii_digit() || c == '-')).unwrap_or(rest.len());
    rest[..end].parse().ok()
}

pub struct Timeline {
    pub dir: PathBuf,
    pub meta: Meta,
    grid_rec: u64,
    depth_rec: u64,
    k: usize,
    grid: File,
    trades: File,
    depth: File,
    status: Mutex<LineIndex>,
    events: Mutex<(LineIndex, Vec<EventRec>)>,
}

fn read_meta(dir: &Path) -> Result<(Meta, serde_json::Value), String> {
    let p = dir.join("tl_meta.json");
    let t = std::fs::read_to_string(&p).map_err(|e| format!("读 {} 失败：{e}", p.display()))?;
    let v: serde_json::Value = serde_json::from_str(&t).map_err(|e| format!("{} 不是 JSON：{e}", p.display()))?;
    let strs = |k: &str| -> Vec<String> {
        v[k].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default()
    };
    let m = Meta {
        symbol: v["symbol"].as_str().unwrap_or_default().to_string(),
        vendor: v["vendor"].as_str().unwrap_or_default().to_string(),
        t0: v["t0"].as_i64().unwrap_or(0),
        t1: v["t1"].as_i64().unwrap_or(0),
        tick: v["tick"].as_f64().unwrap_or(0.0),
        lot: v["lot"].as_f64().unwrap_or(0.0),
        grid_ms: v["grid_ms"].as_u64().unwrap_or(0),
        columns: strs("columns"),
        quality_codes: strs("quality_codes"),
        reason_codes: strs("reason_codes"),
        depth_levels: v["depth_levels"].as_u64().unwrap_or(0) as usize,
        done: v["done"].as_bool().unwrap_or(false),
    };
    Ok((m, v))
}

/// 栅格记录长度（与主仓 `timeline_out::grid_rec` 同一条）。
#[must_use]
pub const fn grid_rec(k: usize) -> u64 {
    ((24 + 18 * k).div_ceil(8) * 8) as u64
}

fn i64_at(b: &[u8], at: usize) -> i64 {
    i64::from_le_bytes(b[at..at + 8].try_into().unwrap_or_default())
}

fn f64_at(b: &[u8], at: usize) -> f64 {
    f64::from_le_bytes(b[at..at + 8].try_into().unwrap_or_default())
}

impl Timeline {
    /// 打开一份时间线（`tl_meta.json` 要已经写出来）。记录长度与这里的布局对不上就拒绝。
    ///
    /// # Errors
    /// 文件缺失 / 布局对不上。
    pub fn open(dir: &Path) -> Result<Self, String> {
        let (meta, raw) = read_meta(dir)?;
        let k = meta.columns.len();
        let levels = meta.depth_levels;
        let (grid_rec, depth_rec) = (grid_rec(k), (24 + 32 * levels) as u64);
        for (name, mine) in [("trade", TRADE_REC), ("depth", depth_rec), ("grid", grid_rec)] {
            let theirs = raw["layout"][name]["rec"].as_u64().unwrap_or(0);
            if theirs != mine {
                return Err(format!("时间线的 {name} 记录长度对不上：文件说 {theirs}，这里算出 {mine}（两边版本不一致）"));
            }
        }
        let open = |n: &str| File::open(dir.join(n)).map_err(|e| format!("打开 {n} 失败：{e}"));
        Ok(Self {
            dir: dir.to_path_buf(),
            meta,
            grid_rec,
            depth_rec,
            k,
            grid: open("tl_grid.bin")?,
            trades: open("tl_trades.bin")?,
            depth: open("tl_depth.bin")?,
            status: Mutex::new(LineIndex::default()),
            events: Mutex::new((LineIndex::default(), Vec::new())),
        })
    }

    /// 重读 `tl_meta.json`（只关心算完没有）。
    pub fn refresh_done(&mut self) {
        if !self.meta.done
            && let Ok((m, _)) = read_meta(&self.dir)
        {
            self.meta.done = m.done;
        }
    }

    fn count(f: &File, rec: u64) -> u64 {
        f.metadata().map_or(0, |m| m.len()) / rec
    }

    fn ti_at(f: &File, rec: u64, i: u64) -> i64 {
        let mut b = [0u8; 8];
        if f.read_exact_at(&mut b, i * rec).is_err() {
            return i64::MAX;
        }
        i64::from_le_bytes(b)
    }

    /// 前 `n` 条里 `ti < t`（`incl` 时 `≤ t`）的条数。
    fn bound(f: &File, rec: u64, n: u64, t: i64, incl: bool) -> u64 {
        let (mut lo, mut hi) = (0u64, n);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let x = Self::ti_at(f, rec, mid);
            if x < t || (incl && x == t) {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        lo
    }

    /// 已经可以读到哪：算完 = 时段终点；一行栅格都还没有 = `None`。
    #[must_use]
    pub fn frontier(&self) -> Option<i64> {
        if self.meta.done {
            return Some(self.meta.t1);
        }
        let n = Self::count(&self.grid, self.grid_rec);
        (n > 0).then(|| Self::ti_at(&self.grid, self.grid_rec, n - 1))
    }

    /// `ti ∈ [lo, hi)` 的成交，至多 `cap` 条（多了只给前 `cap` 条；调用方按最后一条的 `ti` 接着取）。
    #[must_use]
    pub fn trades(&self, lo: i64, hi: i64, cap: usize) -> Vec<TradeRec> {
        let n = Self::count(&self.trades, TRADE_REC);
        let a = Self::bound(&self.trades, TRADE_REC, n, lo, false);
        let b = Self::bound(&self.trades, TRADE_REC, n, hi, false).min(a + cap as u64);
        if b <= a {
            return Vec::new();
        }
        let mut buf = vec![0u8; ((b - a) * TRADE_REC) as usize];
        if self.trades.read_exact_at(&mut buf, a * TRADE_REC).is_err() {
            return Vec::new();
        }
        buf.chunks_exact(TRADE_REC as usize)
            .map(|r| TradeRec { ti: i64_at(r, 0), te: i64_at(r, 8), px: f64_at(r, 16), qty: f64_at(r, 24), side: r[32] as i8 })
            .collect()
    }

    fn depth_rec(&self, r: &[u8]) -> DepthRec {
        let levels = self.meta.depth_levels;
        let nb = u32::from_le_bytes(r[16..20].try_into().unwrap_or_default()) as usize;
        let na = u32::from_le_bytes(r[20..24].try_into().unwrap_or_default()) as usize;
        let side = |base: usize, n: usize| (0..n.min(levels)).map(|i| (f64_at(r, base + 16 * i), f64_at(r, base + 16 * i + 8))).collect();
        DepthRec { ti: i64_at(r, 0), te: i64_at(r, 8), bids: side(24, nb), asks: side(24 + 16 * levels, na) }
    }

    /// `ti ∈ [lo, hi)` 的深度快照。
    #[must_use]
    pub fn depths(&self, lo: i64, hi: i64) -> Vec<DepthRec> {
        let n = Self::count(&self.depth, self.depth_rec);
        let a = Self::bound(&self.depth, self.depth_rec, n, lo, false);
        let b = Self::bound(&self.depth, self.depth_rec, n, hi, false);
        if b <= a {
            return Vec::new();
        }
        let mut buf = vec![0u8; ((b - a) * self.depth_rec) as usize];
        if self.depth.read_exact_at(&mut buf, a * self.depth_rec).is_err() {
            return Vec::new();
        }
        buf.chunks_exact(self.depth_rec as usize).map(|r| self.depth_rec(r)).collect()
    }

    /// `ti ≤ t` 的最后一行栅格（`t` 先截到 frontier）。
    #[must_use]
    pub fn row_at(&self, t: i64) -> Option<GridRow> {
        let t = t.min(self.frontier()?);
        let n = Self::count(&self.grid, self.grid_rec);
        let i = Self::bound(&self.grid, self.grid_rec, n, t, true).checked_sub(1)?;
        let mut r = vec![0u8; self.grid_rec as usize];
        self.grid.read_exact_at(&mut r, i * self.grid_rec).ok()?;
        let k = self.k;
        let (zv, pv, qv) = (24 + 8 * k, 24 + 12 * k, 24 + 16 * k);
        let f32_at = |at: usize| f32::from_le_bytes(r[at..at + 4].try_into().unwrap_or_default());
        Some(GridRow {
            ti: i64_at(&r, 0),
            te: i64_at(&r, 8),
            seq: u64::from_le_bytes(r[16..24].try_into().unwrap_or_default()),
            val: (0..k).map(|j| f64_at(&r, 24 + 8 * j)).collect(),
            z: (0..k).map(|j| f32_at(zv + 4 * j)).collect(),
            pct: (0..k).map(|j| f32_at(pv + 4 * j)).collect(),
            q: r[qv..qv + k].to_vec(),
            reason: r[qv + k..qv + 2 * k].to_vec(),
        })
    }

    /// 条件跳转（docs/51 P5）：从 `t` 往后（`forward`）或往前找条件**由不成立变成立**的那一格，返回它的 `ti`。
    /// 只看 frontier 之前的行；质量编码 ≥ 2（STALE / INVALID / UNAVAILABLE …）的格当 NaN。
    /// 往后找的是 `ti > t` 的第一处；往前找的是 `ti < t` 的最后一处。成块读（一次 256 行），只取用到的列。
    #[must_use]
    pub fn find_edge(&self, cond: &super::replay_cond::Cond, t: i64, forward: bool) -> Option<i64> {
        let f = self.frontier()?;
        let n = Self::count(&self.grid, self.grid_rec);
        let n = Self::bound(&self.grid, self.grid_rec, n, f, true);
        if n == 0 {
            return None;
        }
        let k = self.k;
        let (qv, rec) = (24 + 16 * k, self.grid_rec as usize);
        // 一格成立吗（第 i 行）；块缓存
        const CH: u64 = 256;
        let mut cache: Option<(u64, Vec<u8>)> = None;
        let mut row_ok = |i: u64| -> Option<(i64, bool)> {
            let base = (i / CH) * CH;
            if cache.as_ref().is_none_or(|c| c.0 != base) {
                let m = CH.min(n - base);
                let mut buf = vec![0u8; (m * self.grid_rec) as usize];
                self.grid.read_exact_at(&mut buf, base * self.grid_rec).ok()?;
                cache = Some((base, buf));
            }
            let (_, buf) = cache.as_ref()?;
            let r = &buf[((i - base) as usize) * rec..((i - base) as usize + 1) * rec];
            let val = |c: usize| if r[qv + c] >= 2 { f64::NAN } else { f64_at(r, 24 + 8 * c) };
            Some((i64_at(r, 0), cond.eval(&val)))
        };
        let first_after = Self::bound(&self.grid, self.grid_rec, n, t, true); // ti > t 的第一行
        if forward {
            let mut prev = if first_after == 0 { false } else { row_ok(first_after - 1)?.1 };
            for i in first_after..n {
                let (ti, ok) = row_ok(i)?;
                if ok && !prev {
                    return Some(ti);
                }
                prev = ok;
            }
            None
        } else {
            let last_before = Self::bound(&self.grid, self.grid_rec, n, t, false); // ti < t 的行数
            let mut i = last_before;
            while i > 0 {
                i -= 1;
                let (ti, ok) = row_ok(i)?;
                let prev = if i == 0 { false } else { row_ok(i - 1)?.1 };
                if ok && !prev {
                    return Some(ti);
                }
            }
            None
        }
    }

    /// `ti ≤ t` 的最后一行引擎状态（每秒一行），`(ti, 原文)`。
    #[must_use]
    pub fn status_at(&self, t: i64) -> Option<(i64, serde_json::Value)> {
        let t = t.min(self.frontier()?);
        let mut g = self.status.lock().ok()?;
        g.refresh(&self.dir.join("tl_status.jsonl"));
        let (off, len) = g.last_at(t)?;
        drop(g);
        let f = File::open(self.dir.join("tl_status.jsonl")).ok()?;
        let mut buf = vec![0u8; len as usize];
        f.read_exact_at(&mut buf, off).ok()?;
        let v: serde_json::Value = serde_json::from_slice(&buf).ok()?;
        Some((v["ti"].as_i64().unwrap_or(0), v))
    }

    /// 各类事件的条数（frontier 之前），多的在前。
    #[must_use]
    pub fn event_counts(&self) -> Vec<(String, usize)> {
        let f = self.frontier().unwrap_or(i64::MIN);
        let mut m: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
        for e in self.events(i64::MIN, f, &[]) {
            *m.entry(e.kind).or_default() += 1;
        }
        let mut v: Vec<_> = m.into_iter().collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        v
    }

    /// `ti ∈ [lo, hi)` 的事件（`kinds` 为空 = 全部；否则只要这些 kind）。
    #[must_use]
    pub fn events(&self, lo: i64, hi: i64, kinds: &[&str]) -> Vec<EventRec> {
        let Ok(mut g) = self.events.lock() else { return Vec::new() };
        let (idx, parsed) = &mut *g;
        idx.refresh(&self.dir.join("tl_events.jsonl"));
        if parsed.len() < idx.lines.len()
            && let Ok(f) = File::open(self.dir.join("tl_events.jsonl"))
        {
            for &(_, off, len) in &idx.lines[parsed.len()..] {
                let mut buf = vec![0u8; len as usize];
                let e = f.read_exact_at(&mut buf, off).ok().and_then(|()| serde_json::from_slice::<serde_json::Value>(&buf).ok());
                let Some(e) = e else { break };
                parsed.push(EventRec {
                    ti: e["ti"].as_i64().unwrap_or(0),
                    ts: e["ts"].as_i64().unwrap_or_else(|| e["ti"].as_i64().unwrap_or(0)),
                    kind: e["kind"].as_str().unwrap_or_default().to_string(),
                    dir: e["dir"].as_i64().unwrap_or(0) as i8,
                    px: e["px"].as_f64(),
                    size: e["size"].as_f64(),
                });
            }
        }
        let a = parsed.partition_point(|e| e.ti < lo);
        let b = parsed.partition_point(|e| e.ti < hi);
        parsed[a..b.max(a)].iter().filter(|e| kinds.is_empty() || kinds.contains(&e.kind.as_str())).cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ws_tl_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn meta(d: &Path, k: usize, done: bool) {
        let cols: Vec<String> = (0..k).map(|i| format!("c{i}@0")).collect();
        let v = serde_json::json!({"symbol": "X", "t0": 0, "t1": 10_000_000_000i64, "columns": cols, "depth_levels": 2, "done": done,
            "reason_codes": ["not_implemented"], "quality_codes": ["GOOD"],
            "layout": {"trade": {"rec": 40}, "depth": {"rec": 24 + 32 * 2}, "grid": {"rec": grid_rec(k)}}});
        std::fs::write(d.join("tl_meta.json"), v.to_string()).unwrap();
    }

    fn grid_row(k: usize, ti: i64, v: f64) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&ti.to_le_bytes());
        b.extend_from_slice(&(ti - 5).to_le_bytes());
        b.extend_from_slice(&7u64.to_le_bytes());
        for _ in 0..k {
            b.extend_from_slice(&v.to_le_bytes());
        }
        for _ in 0..k {
            b.extend_from_slice(&1.5f32.to_le_bytes());
        }
        for _ in 0..k {
            b.extend_from_slice(&0.25f32.to_le_bytes());
        }
        b.extend(std::iter::repeat_n(0u8, k));
        b.extend(std::iter::repeat_n(1u8, k));
        b.resize(grid_rec(k) as usize, 0);
        b
    }

    fn trade(ti: i64) -> Vec<u8> {
        let mut b = vec![0u8; 40];
        b[0..8].copy_from_slice(&ti.to_le_bytes());
        b[16..24].copy_from_slice(&100.5f64.to_le_bytes());
        b[24..32].copy_from_slice(&2.0f64.to_le_bytes());
        b[32] = (-1i8) as u8;
        b
    }

    #[test]
    fn 布局对不上拒绝打开() {
        let d = tmp("layout");
        meta(&d, 2, false);
        let mut v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(d.join("tl_meta.json")).unwrap()).unwrap();
        v["layout"]["grid"]["rec"] = 999.into();
        std::fs::write(d.join("tl_meta.json"), v.to_string()).unwrap();
        for n in ["tl_grid.bin", "tl_trades.bin", "tl_depth.bin"] {
            std::fs::write(d.join(n), b"").unwrap();
        }
        assert!(Timeline::open(&d).err().unwrap().contains("记录长度"));
    }

    #[test]
    fn 按时刻取行_截到frontier_半条不读() {
        let d = tmp("read");
        meta(&d, 3, false);
        let mut g = grid_row(3, 100, 1.0);
        g.extend(grid_row(3, 200, 2.0));
        g.extend_from_slice(&[1, 2, 3]); // 半条
        std::fs::write(d.join("tl_grid.bin"), g).unwrap();
        let mut t = trade(50);
        t.extend(trade(150));
        t.extend(trade(250));
        std::fs::write(d.join("tl_trades.bin"), t).unwrap();
        std::fs::write(d.join("tl_depth.bin"), b"").unwrap();
        std::fs::write(d.join("tl_status.jsonl"), "{\"ti\":0,\"regime\":\"a\"}\n{\"ti\":150,\"regime\":\"b\"}\n{\"ti\":3").unwrap();
        std::fs::write(d.join("tl_events.jsonl"), "{\"ti\":120,\"kind\":\"SWEEP\",\"dir\":1,\"px\":1.0}\n{\"ti\":180,\"kind\":\"det:sweep\",\"dir\":-1}\n").unwrap();
        let mut tl = Timeline::open(&d).unwrap();
        assert_eq!(tl.frontier(), Some(200));
        assert!(tl.row_at(99).is_none());
        let r = tl.row_at(10_000).unwrap(); // 截到 frontier
        assert_eq!((r.ti, r.te, r.seq, r.val[2], r.z[0], r.pct[1], r.q[0], r.reason[2]), (200, 195, 7, 2.0, 1.5, 0.25, 0, 1));
        assert_eq!(tl.row_at(199).unwrap().ti, 100);
        let tr = tl.trades(0, 200, 100);
        assert_eq!(tr.iter().map(|x| x.ti).collect::<Vec<_>>(), vec![50, 150]);
        assert_eq!((tr[0].px, tr[0].qty, tr[0].side), (100.5, 2.0, -1));
        assert_eq!(tl.trades(0, 1000, 1).len(), 1); // cap
        assert_eq!(tl.status_at(160).unwrap().1["regime"], "b");
        assert_eq!(tl.status_at(149).unwrap().0, 0);
        assert_eq!(tl.events(0, 200, &[]).len(), 2);
        assert_eq!(tl.events(0, 200, &["det:sweep"])[0].dir, -1);
        meta(&d, 3, true);
        tl.refresh_done();
        assert_eq!(tl.frontier(), Some(10_000_000_000));
    }

    #[test]
    fn 条件由不成立变成立的那一格() {
        let d = tmp("edge");
        meta(&d, 2, true);
        // 第 0 列：0 1 1 0 1；第 1 列质量在第 4 行不够（编码 3）
        let mut g = Vec::new();
        for (i, v) in [0.0, 1.0, 1.0, 0.0, 1.0].iter().enumerate() {
            let mut r = grid_row(2, (i as i64 + 1) * 100, *v);
            if i == 4 {
                let qv = 24 + 16 * 2;
                r[qv] = 3;
            }
            g.extend(r);
        }
        std::fs::write(d.join("tl_grid.bin"), g).unwrap();
        for n in ["tl_trades.bin", "tl_depth.bin"] {
            std::fs::write(d.join(n), b"").unwrap();
        }
        let tl = Timeline::open(&d).unwrap();
        let c = super::super::replay_cond::Cond::parse("c0@0 > 0.5", &tl.meta.columns).unwrap();
        assert_eq!(tl.find_edge(&c, 0, true), Some(200));
        assert_eq!(tl.find_edge(&c, 200, true), None, "第 5 行质量不够：不算又成立");
        assert_eq!(tl.find_edge(&c, 450, false), Some(200));
        assert_eq!(tl.find_edge(&c, 200, false), None, "往前找要 ti < t");
        assert_eq!(tl.find_edge(&c, 150, true), Some(200), "150 之后第一行就是边");
        assert_eq!(tl.find_edge(&c, 250, true), None, "250 之后：300 前一行已成立、400 不成立、500 质量不够");
    }

    #[test]
    fn 行首的ti字段() {
        assert_eq!(ti_of(br#"{"chart_values":[],"ti":1780272001000000000,"x":1}"#), Some(1_780_272_001_000_000_000));
        assert_eq!(ti_of(b"{\"x\":1}"), None);
    }
}
