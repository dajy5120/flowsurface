//! 画线分析 × 回放（docs/52 A2）：分析记下它是在哪段数据、回放到哪、看着哪里画的；打开分析就回到那里。
//!
//! 记下的东西放在分析的 `state` 里：
//!
//! | 键 | 内容 |
//! |---|---|
//! | `data` | 数据段（[`super::feature_source::DataSeg`]：数据商、根目录、市场、标的、日期、起始时刻、时长） |
//! | `replay` | 回放状态（[`super::replay_clock::State`]：播放头、倍速、步长、事件筛选、条件） |
//! | `view` | 视野：`{"center_ms": …}`（拖过 / 跳过的中心时刻）或 `"live"`（跟随最新） |
//!
//! - **自动记**：当前分析的品种正在回放、且分析还没记过数据段或记的就是这一段时，每 3 秒看一次，变了就记；
//!   切到别的分析之前也记一次。记的是别的数据段时不自动覆盖（卡片上写明，点「记下现在的状态」才改）。
//! - **打开**（设为当前）：记过数据段就开那段回放（已经在看就不重开），播放头等算到了再跳，筛选、条件照旧，
//!   灌完图再把视野移回去。
//! - 回放书签也跟着分析走（`replay_clock::set_scope`）：默认画板用 `数据商|标的`，分析用 `数据商|标的#分析 id`。

use std::sync::Mutex;
use std::time::Instant;

use super::drawing::{self, DrawMsg};
use super::feature_source::{self as fs, DataSeg};
use super::replay_clock::{self as rc, ViewSync};

/// 回放图表外壳的品种键（与画线同一个键：外壳是 `BinanceLinear:<标的>`，见 `feature_source` 的外壳说明）。
#[must_use]
pub fn replay_key(symbol: &str) -> Option<String> {
    let s = fs::shell_symbol(symbol)?;
    Some(drawing::symbol_key(&exchange::Ticker::new(&s, exchange::adapter::Exchange::BinanceLinear)))
}

/// 现在看的回放：(品种键, 记下来的状态)。
fn capture() -> Option<(String, serde_json::Value)> {
    let seg = fs::current_seg()?;
    let key = replay_key(&seg.symbol)?;
    let replay = rc::snapshot()?;
    let view = match rc::last_view() {
        Some(ViewSync::Center(t)) => serde_json::json!({ "center_ms": t }),
        _ => serde_json::json!("live"),
    };
    Some((key, serde_json::json!({ "data": seg, "replay": replay, "view": view })))
}

fn seg_of(v: &serde_json::Value) -> Option<DataSeg> {
    serde_json::from_value(v.get("data")?.clone()).ok()
}

/// 记进当前分析。`force`：记的是别的数据段也覆盖（用户点了「记下现在的状态」）。
fn record(force: bool) {
    let Some((key, st)) = capture() else { return };
    let Some(id) = drawing::active_analysis(&key) else { return };
    let old = drawing::analysis_state(&key, &id);
    let same = match (old.as_ref().and_then(seg_of), seg_of(&st)) {
        (Some(a), Some(b)) => a.same_as(&b),
        (None, _) => true,
        _ => false,
    };
    if same || force {
        drawing::set_analysis_state(&key, &id, st);
    }
}

/// 灌完图以后要移回去的视野。
static PENDING_VIEW: Mutex<Option<ViewSync>> = Mutex::new(None);
static LAST: Mutex<Option<Instant>> = Mutex::new(None);

/// 已经「接上」的 (回放目录, 当前分析)：一次回放刚开起来、正好是当前分析记的那一段，先回到它记下的地方再开始自动记。
static JOINED: Mutex<Option<(std::path::PathBuf, Option<String>)>> = Mutex::new(None);

/// 主循环每帧调：书签跟着当前分析；新开的回放正好是当前分析记的那一段就回到它记下的地方；每 3 秒自动记一次；
/// 打开分析后图灌好了把视野移回去。
pub fn tick() {
    let Some(seg) = fs::current_seg() else { return };
    let Some(key) = replay_key(&seg.symbol) else { return };
    let active = drawing::active_analysis(&key);
    rc::set_scope(active.clone());
    // 回放刚开起来（或换了当前分析）：记的就是这一段的话先回到原处——不然自动记会拿刚开始的播放头把它冲掉
    if let Some((dir, _, _)) = rc::target() {
        let me = (dir, active.clone());
        let fresh = JOINED.lock().is_ok_and(|mut g| {
            let changed = g.as_ref() != Some(&me);
            if changed {
                *g = Some(me);
            }
            changed
        });
        if fresh
            && let Some(id) = &active
            && drawing::analysis_state(&key, id).as_ref().and_then(seg_of).is_some_and(|s| s.same_as(&seg))
        {
            open_state(&key);
            return;
        }
    }
    if rc::settled()
        && let Some(v) = PENDING_VIEW.lock().ok().and_then(|mut g| g.take())
    {
        rc::publish_view(0, v);
    }
    let due = LAST.lock().is_ok_and(|mut g| {
        let go = g.is_none_or(|t| t.elapsed().as_secs() >= 3);
        if go {
            *g = Some(Instant::now());
        }
        go
    });
    if due {
        record(false);
    }
}

/// 绘图消息的外层（`main.rs` 交过来）：切分析之前先记下离开的那份，切过去之后回到新的那份记下的地方。
pub fn on_draw(m: DrawMsg) {
    let switching = match &m {
        DrawMsg::AnaOpen(sym, _) | DrawMsg::AnaNew(sym) | DrawMsg::AnaSaveAs(sym) => Some(sym.clone()),
        DrawMsg::AnaRecord(_) => {
            record(true);
            None
        }
        _ => None,
    };
    if switching.is_some() {
        record(false);
    }
    drawing::handle(m);
    if let Some(sym) = switching {
        open_state(&sym);
    }
}

/// 从「全部分析」列表打开一份（可能是别的品种 / 别的数据段上的）：设为当前，记过数据段就开那段回放回到原处；
/// 没记过数据段又不是这张图的品种，就只能到那个品种的图上再看（写进说明）。
pub fn open_from_list(sym: &str, id: &str) {
    let has_seg = drawing::analysis_state(sym, id).as_ref().and_then(seg_of).is_some();
    on_draw(DrawMsg::AnaOpen(sym.to_string(), Some(id.to_string())));
    if !has_seg && drawing::ana_menu().as_deref() != Some(sym) {
        drawing::set_note(format!("这份分析没有记下数据段：到 {} 的图上看它的画线", sym.rsplit(':').next().unwrap_or(sym)));
    }
}

/// 回到当前分析记下的地方（没记过就不动）。
fn open_state(sym: &str) {
    let Some(id) = drawing::active_analysis(sym) else { return };
    let Some(st) = drawing::analysis_state(sym, &id) else { return };
    if let Some(seg) = seg_of(&st) {
        fs::open_seg(&seg);
    }
    if let Some(r) = st.get("replay").and_then(|r| serde_json::from_value::<rc::State>(r.clone()).ok()) {
        rc::restore(&r);
    }
    let view = match st.get("view") {
        Some(v) if v.get("center_ms").is_some() => v["center_ms"].as_u64().map(ViewSync::Center),
        _ => Some(ViewSync::Live),
    };
    if let Ok(mut g) = PENDING_VIEW.lock() {
        *g = view;
    }
    // 打开之后先别马上自动记（播放头还没回到记下的地方，记了会把它冲掉）
    if let Ok(mut g) = LAST.lock() {
        *g = Some(Instant::now() + std::time::Duration::from_secs(5));
    }
}

// ── 缩略图（docs/52 A3）────────────────────────────────────────────────────────
//
// 截一张主窗口，缩到 [`THUMB_W`] 像素宽、编成 PNG、base64 放进分析 `extra` 的 `thumb`（随导出一起走）。
// 自动：当前分析在回放时每分钟一张；手动：卡片上「更新缩略图」。

/// 缩略图宽（像素）。
pub const THUMB_W: u32 = 320;

static THUMB_LAST: Mutex<Option<Instant>> = Mutex::new(None);

/// 该截一张了吗：当前分析的品种在回放、看的是「订单流特征」、上一张过了一分钟。返回 (品种键, 分析 id)。
#[must_use]
pub fn thumb_due() -> Option<(String, String)> {
    if !rc::charts_on() {
        return None;
    }
    let seg = fs::current_seg()?;
    let key = replay_key(&seg.symbol)?;
    let id = drawing::active_analysis(&key)?;
    let mut g = THUMB_LAST.lock().ok()?;
    // 第一张也等一分钟（刚开回放时图还是空的）
    if g.is_none_or(|t| t.elapsed().as_secs() < 60) {
        if g.is_none() {
            *g = Some(Instant::now());
        }
        return None;
    }
    *g = Some(Instant::now());
    Some((key, id))
}

/// 手动截一张：这个品种当前的分析。
#[must_use]
pub fn thumb_target(symbol: &str) -> Option<(String, String)> {
    let id = drawing::active_analysis(symbol)?;
    if let Ok(mut g) = THUMB_LAST.lock() {
        *g = Some(Instant::now());
    }
    Some((symbol.to_string(), id))
}

/// 截好的窗口 → 缩略图 → 存进分析（缩放、编码在后台线程）。
pub fn store_thumb(key: String, id: String, shot: &iced::window::Screenshot) {
    let (w, h) = (shot.size.width, shot.size.height);
    let rgba = shot.rgba.clone();
    super::spawn_named("ws-ana-thumb", move || {
        if let Some(v) = encode_thumb(&rgba, w, h) {
            drawing::set_analysis_extra(&key, &id, "thumb", v);
        }
    });
}

/// RGBA（`w × h`）→ 宽 [`THUMB_W`] 的缩略图（逐块取平均），PNG + base64：`{"w", "h", "png"}`。
#[must_use]
pub fn encode_thumb(rgba: &[u8], w: u32, h: u32) -> Option<serde_json::Value> {
    use base64::Engine as _;
    if w == 0 || h == 0 || rgba.len() < (w * h * 4) as usize {
        return None;
    }
    let tw = THUMB_W.min(w);
    let th = ((u64::from(h) * u64::from(tw)) / u64::from(w)).max(1) as u32;
    let mut out = vec![0u8; (tw * th * 4) as usize];
    for ty in 0..th {
        let (y0, y1) = (ty * h / th, ((ty + 1) * h / th).max(ty * h / th + 1));
        for tx in 0..tw {
            let (x0, x1) = (tx * w / tw, ((tx + 1) * w / tw).max(tx * w / tw + 1));
            let mut acc = [0u32; 4];
            for y in y0..y1 {
                for x in x0..x1 {
                    let i = ((y * w + x) * 4) as usize;
                    for c in 0..4 {
                        acc[c] += u32::from(rgba[i + c]);
                    }
                }
            }
            let n = (y1 - y0) * (x1 - x0);
            let o = ((ty * tw + tx) * 4) as usize;
            for c in 0..4 {
                out[o + c] = (acc[c] / n) as u8;
            }
        }
    }
    let mut png_bytes = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut png_bytes, tw, th);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut wr = enc.write_header().ok()?;
        wr.write_image_data(&out).ok()?;
    }
    Some(serde_json::json!({ "w": tw, "h": th, "png": base64::engine::general_purpose::STANDARD.encode(png_bytes) }))
}

/// 缩略图 → 能显示的 RGBA（解 PNG）。
#[must_use]
pub fn decode_thumb(v: &serde_json::Value) -> Option<(u32, u32, Vec<u8>)> {
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD.decode(v.get("png")?.as_str()?).ok()?;
    let dec = png::Decoder::new(std::io::Cursor::new(bytes));
    let mut r = dec.read_info().ok()?;
    let mut buf = vec![0u8; r.output_buffer_size()?];
    let info = r.next_frame(&mut buf).ok()?;
    buf.truncate(info.buffer_size());
    (info.color_type == png::ColorType::Rgba).then_some((info.width, info.height, buf))
}

/// 卡片上一份分析记下的状态一行（数据段 · 播放头；不是现在看的那段时写明）。
#[must_use]
pub fn describe(state: Option<&serde_json::Value>) -> Option<(String, bool)> {
    let st = state?;
    let seg = seg_of(st)?;
    let head = st.get("replay").and_then(|r| r.get("head")).and_then(serde_json::Value::as_i64);
    let mut s = seg.describe();
    if let Some(h) = head {
        s.push_str(&format!(" · 播放头 {}", rc::fmt_t(h)));
    }
    let here = fs::current_seg().is_some_and(|c| c.same_as(&seg));
    Some((s, here))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 缩略图缩放与编解码往返() {
        // 640×360 的左半红、右半蓝 → 320×180
        let (w, h) = (640u32, 360u32);
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                rgba[i..i + 4].copy_from_slice(if x < w / 2 { &[255, 0, 0, 255] } else { &[0, 0, 255, 255] });
            }
        }
        let v = encode_thumb(&rgba, w, h).unwrap();
        assert_eq!((v["w"].as_u64(), v["h"].as_u64()), (Some(320), Some(180)));
        let (tw, th, px) = decode_thumb(&v).unwrap();
        assert_eq!((tw, th, px.len()), (320, 180, 320 * 180 * 4));
        assert_eq!(&px[0..4], &[255, 0, 0, 255]);
        let last = px.len() - 4;
        assert_eq!(&px[last..], &[0, 0, 255, 255]);
        assert!(encode_thumb(&rgba[..10], w, h).is_none(), "字节不够不编");
    }

    #[test]
    fn 回放外壳的品种键与画线一致() {
        let k = replay_key("BTCUSDT").unwrap();
        assert_eq!(k, drawing::symbol_key(&exchange::Ticker::new("BTCUSDT", exchange::adapter::Exchange::BinanceLinear)));
        assert!(replay_key("").is_none());
    }

    #[test]
    fn 状态里的数据段() {
        let seg = DataSeg {
            source: "tardis".into(),
            root: "/r".into(),
            market: "linear".into(),
            symbol: "BTCUSDT".into(),
            date: "2026-06-01".into(),
            hhmm: "00:00".into(),
            minutes: 30,
            types: vec![],
        };
        let st = serde_json::json!({"data": seg, "replay": {"head": 1_780_272_123_400_000_000i64, "pace": "10", "step": "1s", "cond": ""}, "view": "live"});
        assert_eq!(seg_of(&st).as_ref(), Some(&seg));
        let (s, here) = describe(Some(&st)).unwrap();
        assert!(s.contains("BTCUSDT 2026-06-01 00:00 起 30 分钟") && s.contains("播放头 00:02:03.4"), "{s}");
        assert!(!here, "没在回放");
        let r: rc::State = serde_json::from_value(st["replay"].clone()).unwrap();
        assert_eq!((r.pace.as_str(), r.kinds), ("10", None));
        let mut other = seg.clone();
        other.root = "/other".into();
        assert!(seg.same_as(&other), "换个根目录还是同一段");
        other.minutes = 60;
        assert!(!seg.same_as(&other));
    }
}
