//! 消息中心的提醒声音（docs/68）：按级别 P0–P3 各放一个用户选的声音。
//!
//! - 声音文件统一放在程序目录下的 `sounds/`（`paths::repo_root()`，`WS_SOUNDS_DIR` 可改），设置里只存相对这个文件夹的文件名；
//!   第一次用时建文件夹、写三个合成的默认提示音。
//! - 决定响不响是纯逻辑（[`Decider`]）：只对这次运行新进来的消息响、同一秒多条只按最高级响一次、同级至少隔 3 秒、
//!   免打扰（沿用日历的时段）、静音；P0 一直响到这条点了「知道了」。
//! - 放音在独立线程 `ws-sound`（声音设备只在那个线程里开）；设备打不开（没声卡、Xvfb）时记下原因、30 秒后再试，不崩。
//!
//! 调用方：`inbox` 的后台线程每轮把新消息交给 [`on_arrivals`]、再调 [`tick`]；界面走 [`handle`]。

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use wealthspring_inbox as ib;

/// 同一级响完至少隔这么久才再响（P0 的重复按自己的间隔）。
const MIN_GAP_MS: i64 = 3_000;
/// 能选的文件类型（rodio 开了 wav / mp3 / flac / vorbis / mp4 解码）。
const EXTS: [&str; 7] = ["wav", "mp3", "flac", "ogg", "oga", "m4a", "mp4"];
/// 设备打不开时隔多久再试。
const RETRY: Duration = Duration::from_secs(30);

// ── 设置 ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LevelCfg {
    pub on: bool,
    /// 相对声音文件夹的文件名（`/` 分隔）；空 = 没选
    pub file: String,
    /// 0..=100
    pub volume: u8,
}

impl Default for LevelCfg {
    fn default() -> Self {
        Self { on: false, file: String::new(), volume: 70 }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SoundCfg {
    /// P0..P3
    pub levels: [LevelCfg; 4],
    /// P0 没确认时每隔几秒再响
    pub p0_repeat_secs: u32,
    /// 每一声最长放几秒（选的是整首歌时只放开头）
    pub max_secs: u32,
    pub mute_forever: bool,
    /// 静音到这个时刻（毫秒；0 = 没静音）
    pub mute_until_ms: i64,
}

impl Default for SoundCfg {
    fn default() -> Self {
        let l = |file: &str, volume| LevelCfg { on: true, file: file.into(), volume };
        Self {
            levels: [l(DEFAULT_FILES[0].0, 80), l(DEFAULT_FILES[1].0, 70), l(DEFAULT_FILES[2].0, 60), LevelCfg::default()],
            p0_repeat_secs: 15,
            max_secs: 8,
            mute_forever: false,
            mute_until_ms: 0,
        }
    }
}

impl SoundCfg {
    #[must_use]
    pub fn muted(&self, now_ms: i64) -> bool {
        self.mute_forever || now_ms < self.mute_until_ms
    }
    fn level(&self, l: u8) -> &LevelCfg {
        &self.levels[usize::from(l.min(3))]
    }
    /// 这一级设了要响（开着、选了文件）。
    #[must_use]
    pub fn wants(&self, l: u8) -> bool {
        let c = self.level(l);
        c.on && !c.file.is_empty()
    }
}

fn cfg_path() -> PathBuf {
    super::paths::hot_dir().join("alert_sounds.json")
}

fn cfg_cell() -> &'static Mutex<SoundCfg> {
    static C: OnceLock<Mutex<SoundCfg>> = OnceLock::new();
    C.get_or_init(|| {
        let c = if cfg!(test) {
            SoundCfg::default()
        } else {
            std::fs::read_to_string(cfg_path()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
        };
        Mutex::new(c)
    })
}

#[must_use]
pub fn cfg() -> SoundCfg {
    cfg_cell().lock().map(|g| g.clone()).unwrap_or_default()
}

fn edit(f: impl FnOnce(&mut SoundCfg)) {
    let snap = {
        let mut g = cfg_cell().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        f(&mut g);
        g.clone()
    };
    if cfg!(test) {
        return;
    }
    let p = cfg_path();
    let tmp = p.with_extension("json.tmp");
    let r = serde_json::to_string_pretty(&snap)
        .map_err(std::io::Error::other)
        .and_then(|s| std::fs::write(&tmp, s))
        .and_then(|()| std::fs::rename(&tmp, &p));
    if let Err(e) = r {
        set_note(format!("✗ 声音设置没存上（{}）：{e}", p.display()));
    }
}

// ── 声音文件夹 ────────────────────────────────────────────────────────

/// 第一次建文件夹时写进去的默认提示音（文件名, 合成函数）。
type Synth = fn() -> Vec<f32>;
const DEFAULT_FILES: [(&str, Synth); 3] = [("警报.wav", synth_alarm), ("双音.wav", synth_two), ("叮.wav", synth_ding)];

const README: &str = "这里是 WealthSpring 消息中心的提醒声音文件夹（docs/68）。\n\n\
把自己的音乐 / 音效（wav、mp3、flac、ogg、m4a）放进来，可以再建一层子文件夹分类；\n\
然后在 Cockpit 顶部铃铛 → 消息中心 → 「声音」里点「重新扫描」，给 P0–P3 各选一个。\n\n\
设置里只记相对这个文件夹的文件名，整个文件夹搬走也能用。选的是整首歌时，每次只放开头几秒（设置里可调）。\n\
这个文件夹不进 git。\n";

/// 声音文件夹：`WS_SOUNDS_DIR`，否则 `<程序目录>/sounds`。
#[must_use]
pub fn dir() -> PathBuf {
    match std::env::var("WS_SOUNDS_DIR") {
        Ok(d) if !d.trim().is_empty() => PathBuf::from(d.trim()),
        _ => super::paths::repo_root().join("sounds"),
    }
}

/// 没有就建，并写进默认提示音与说明（只在新建时写：用户删掉的默认音不会再冒出来）。
pub fn ensure_dir(d: &Path) -> std::io::Result<bool> {
    if d.is_dir() {
        return Ok(false);
    }
    std::fs::create_dir_all(d)?;
    for (name, f) in DEFAULT_FILES {
        std::fs::write(d.join(name), wav_bytes(&f()))?;
    }
    std::fs::write(d.join("说明.txt"), README)?;
    Ok(true)
}

fn is_audio(p: &Path) -> bool {
    p.extension().and_then(|e| e.to_str()).is_some_and(|e| EXTS.contains(&e.to_ascii_lowercase().as_str()))
}

/// 文件夹里的声音文件（含一层子文件夹），相对路径、`/` 分隔、排好序。
#[must_use]
pub fn scan(d: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(d) else { return out };
    for e in rd.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        if p.is_dir() {
            if let Ok(sub) = std::fs::read_dir(&p) {
                for s in sub.flatten() {
                    let sp = s.path();
                    if sp.is_file() && is_audio(&sp) {
                        out.push(format!("{name}/{}", s.file_name().to_string_lossy()));
                    }
                }
            }
        } else if is_audio(&p) {
            out.push(name);
        }
    }
    out.sort();
    out
}

/// 相对名 → 文件夹里的路径；带 `..` / 绝对路径的不认（只许选文件夹里的）。
#[must_use]
pub fn resolve(d: &Path, rel: &str) -> Option<PathBuf> {
    let r = Path::new(rel);
    if rel.is_empty() || r.is_absolute() || r.components().any(|c| !matches!(c, std::path::Component::Normal(_))) {
        return None;
    }
    Some(d.join(r))
}

struct Files {
    list: Vec<String>,
    scanned: bool,
}

fn files_cell() -> &'static Mutex<Files> {
    static F: OnceLock<Mutex<Files>> = OnceLock::new();
    F.get_or_init(|| Mutex::new(Files { list: Vec::new(), scanned: false }))
}

fn rescan() {
    let d = dir();
    let made = ensure_dir(&d);
    let list = scan(&d);
    if let Ok(mut g) = files_cell().lock() {
        g.list = list;
        g.scanned = true;
    }
    match made {
        Ok(true) => set_note(format!("✔ 建好了声音文件夹 {}，放了三个默认提示音", d.display())),
        Ok(false) => {}
        Err(e) => set_note(format!("✗ 建不了声音文件夹 {}：{e}", d.display())),
    }
}

/// `inbox` 后台线程启动时调一次：建好声音文件夹（默认设置指着里面的默认提示音）。
pub fn init() {
    let d = dir();
    if let Err(e) = ensure_dir(&d) {
        log::warn!("[sound] 建不了声音文件夹 {}：{e}", d.display());
        set_note(format!("✗ 建不了声音文件夹 {}：{e}", d.display()));
    }
}

/// 文件夹里能选的文件（第一次调时扫一遍；之后点「重新扫描」才更新）。
#[must_use]
pub fn files() -> Vec<String> {
    let need = files_cell().lock().map(|g| !g.scanned).unwrap_or(false);
    if need && !cfg!(test) {
        rescan();
    }
    files_cell().lock().map(|g| g.list.clone()).unwrap_or_default()
}

// ── 合成默认提示音（16 位单声道 44.1 kHz）──────────────────────────────

const SR: f32 = 44_100.0;

fn tone(out: &mut Vec<f32>, f: f32, secs: f32, decay: f32, amp: f32) {
    let n = (secs * SR) as usize;
    let fade = (0.004 * SR) as usize;
    for k in 0..n {
        let t = k as f32 / SR;
        let ph = 2.0 * std::f32::consts::PI * f * t;
        // 基音 + 一点二次谐波，不那么「电子」
        let s = ph.sin() + 0.25 * (2.0 * ph).sin();
        let mut e = (-t * decay).exp();
        if k < fade {
            e *= k as f32 / fade as f32;
        }
        if n - k < fade {
            e *= (n - k) as f32 / fade as f32;
        }
        out.push(amp * e * s / 1.25);
    }
}

fn gap(out: &mut Vec<f32>, secs: f32) {
    out.extend(std::iter::repeat_n(0.0, (secs * SR) as usize));
}

fn synth_ding() -> Vec<f32> {
    let mut v = Vec::new();
    tone(&mut v, 1318.5, 0.9, 5.0, 0.55);
    v
}

fn synth_two() -> Vec<f32> {
    let mut v = Vec::new();
    tone(&mut v, 880.0, 0.22, 6.0, 0.55);
    gap(&mut v, 0.04);
    tone(&mut v, 1174.7, 0.45, 5.0, 0.55);
    v
}

fn synth_alarm() -> Vec<f32> {
    let mut v = Vec::new();
    for _ in 0..3 {
        tone(&mut v, 988.0, 0.13, 1.0, 0.6);
        tone(&mut v, 1318.5, 0.13, 1.0, 0.6);
        gap(&mut v, 0.08);
    }
    v
}

fn wav_bytes(samples: &[f32]) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut b = Vec::with_capacity(44 + data_len as usize);
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + data_len).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes()); // PCM
    b.extend_from_slice(&1u16.to_le_bytes()); // 单声道
    b.extend_from_slice(&(SR as u32).to_le_bytes());
    b.extend_from_slice(&((SR as u32) * 2).to_le_bytes());
    b.extend_from_slice(&2u16.to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        b.extend_from_slice(&((s.clamp(-1.0, 1.0) * 32_767.0) as i16).to_le_bytes());
    }
    b
}

// ── 响不响（纯逻辑）──────────────────────────────────────────────────

/// 新进来的一条。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Arrival {
    pub id: i64,
    pub level: u8,
    /// 入库即已读（历史、免打扰挡掉的、导入的）或已归档：不响
    pub silent: bool,
}

/// 此刻的环境。
#[derive(Debug, Clone, Copy, Default)]
pub struct Env {
    pub now_ms: i64,
    /// 正在日历的免打扰时段里
    pub quiet: bool,
    /// 免打扰时 P0 例外
    pub p0_exempt: bool,
}

/// 要放音线程做的。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Act {
    Play(u8),
    /// 停掉正在放的 P0（都确认了）
    StopP0,
}

#[derive(Debug, Default)]
pub struct Decider {
    last: [Option<i64>; 4],
    /// 这次运行里响起来、还没确认的 P0
    ringing: BTreeSet<i64>,
}

impl Decider {
    fn blocked_by_quiet(env: &Env, l: u8) -> bool {
        env.quiet && !(l == 0 && env.p0_exempt)
    }

    /// 现在能不能响这一级（设了、没静音、不在免打扰里）。
    fn allowed(cfg: &SoundCfg, env: &Env, l: u8) -> bool {
        cfg.wants(l) && !cfg.muted(env.now_ms) && !Self::blocked_by_quiet(env, l)
    }

    /// 一批新消息：响哪一级（同一批只响最高的一级）。
    pub fn arrivals(&mut self, v: &[Arrival], cfg: &SoundCfg, env: &Env) -> Option<Act> {
        let mut best: Option<u8> = None;
        for a in v.iter().filter(|a| !a.silent) {
            let l = a.level.min(3);
            // P0 记下来：静音结束后还没确认的接着响；免打扰挡掉的（且 P0 不例外）不记
            if l == 0 && cfg.wants(0) && !Self::blocked_by_quiet(env, 0) {
                self.ringing.insert(a.id);
            }
            if Self::allowed(cfg, env, l) && best.is_none_or(|b| l < b) {
                best = Some(l);
            }
        }
        let l = best?;
        if self.last[usize::from(l)].is_some_and(|t| env.now_ms - t < MIN_GAP_MS) {
            return None;
        }
        self.last[usize::from(l)] = Some(env.now_ms);
        Some(Act::Play(l))
    }

    /// 每秒一次：`unacked` = 响着的 P0 里还没确认 / 归档的。
    pub fn tick(&mut self, unacked: &BTreeSet<i64>, cfg: &SoundCfg, env: &Env) -> Option<Act> {
        if self.ringing.is_empty() {
            return None;
        }
        self.ringing.retain(|id| unacked.contains(id));
        if self.ringing.is_empty() {
            return Some(Act::StopP0);
        }
        if !Self::allowed(cfg, env, 0) {
            return None;
        }
        let every = i64::from(cfg.p0_repeat_secs.max(3)) * 1000;
        if self.last[0].is_some_and(|t| env.now_ms - t < every) {
            return None;
        }
        self.last[0] = Some(env.now_ms);
        Some(Act::Play(0))
    }

    #[must_use]
    pub fn ringing(&self) -> Vec<i64> {
        self.ringing.iter().copied().collect()
    }
}

fn decider() -> std::sync::MutexGuard<'static, Decider> {
    static D: OnceLock<Mutex<Decider>> = OnceLock::new();
    D.get_or_init(Default::default).lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn env_now(c: &ib::rusqlite::Connection) -> Env {
    let q = super::calendar_alerts::quiet(c);
    if let Ok(mut g) = status_cell().lock() {
        g.quiet = if q.on() { format!("{}–{}{}", q.from, q.to, if q.p0_exempt { "（P0 照响）" } else { "（P0 也不响）" }) } else { String::new() };
    }
    Env { now_ms: chrono::Utc::now().timestamp_millis(), quiet: q.covers(chrono::Local::now().time()), p0_exempt: q.p0_exempt }
}

/// `inbox` 后台线程：一批新消息。
pub fn on_arrivals(c: &ib::rusqlite::Connection, v: &[Arrival]) {
    if v.is_empty() {
        return;
    }
    let cfg = cfg();
    let env = env_now(c);
    let act = decider().arrivals(v, &cfg, &env);
    if let Some(act) = act {
        let top = v.iter().filter(|a| !a.silent).map(|a| a.level).min().unwrap_or(3);
        log::info!("[sound] 新消息 {} 条（最高 P{top}）→ {act:?}", v.len());
        run(act, &cfg);
    }
}

/// `inbox` 后台线程：每秒一次（P0 重复、确认后停）。
pub fn tick(c: &ib::rusqlite::Connection) {
    let ids = decider().ringing();
    let unacked: BTreeSet<i64> = if ids.is_empty() { BTreeSet::new() } else { ib::unacked(c, &ids).into_iter().collect() };
    let cfg = cfg();
    let env = env_now(c);
    let act = decider().tick(&unacked, &cfg, &env);
    if let Some(act) = act {
        match act {
            Act::StopP0 => log::info!("[sound] P0 都确认了，停"),
            Act::Play(_) => log::info!("[sound] P0 还没确认（{} 条），再响", unacked.len()),
        }
        run(act, &cfg);
    }
}

fn run(act: Act, cfg: &SoundCfg) {
    match act {
        Act::Play(l) => {
            let lc = cfg.level(l);
            match resolve(&dir(), &lc.file) {
                Some(path) => send(Cmd::Play { tag: l, path, volume: lc.volume, max_secs: cfg.max_secs }),
                None => set_note(format!("✗ P{l} 的声音文件名不对：{}", lc.file)),
            }
        }
        Act::StopP0 => send(Cmd::Stop(Some(0))),
    }
}

/// 桌面通知要不要带「别出声」（Cockpit 自己放这一级的声音时）。
#[must_use]
pub fn plays_level(l: u8) -> bool {
    let c = cfg();
    c.wants(l) && !c.muted(chrono::Utc::now().timestamp_millis())
}

// ── 放音线程 ──────────────────────────────────────────────────────────

enum Cmd {
    /// `tag`：级别（试听 = 9）
    Play { tag: u8, path: PathBuf, volume: u8, max_secs: u32 },
    /// 停（`Some(级别)` = 只在放的是这一级时停）
    Stop(Option<u8>),
}

const TEST_TAG: u8 = 9;

#[derive(Debug, Clone, Default)]
pub struct Status {
    /// 设备打不开的原因（空 = 正常 / 还没用过）
    pub device_err: String,
    /// 正在放的级别
    pub playing: Option<u8>,
    /// 最近一句说明（✔ / ✗ 开头）
    pub note: String,
    /// 日历的免打扰时段（空 = 没设）
    pub quiet: String,
}

fn status_cell() -> &'static Mutex<Status> {
    static S: OnceLock<Mutex<Status>> = OnceLock::new();
    S.get_or_init(Default::default)
}

#[must_use]
pub fn status() -> Status {
    let mut s = status_cell().lock().map(|g| g.clone()).unwrap_or_default();
    if s.playing.is_none() && !decider().ringing.is_empty() {
        // 两声之间也算「P0 在响」
        s.playing = Some(0);
    }
    s
}

fn set_note(n: String) {
    if let Ok(mut g) = status_cell().lock() {
        g.note = n;
    }
}

fn sender() -> Option<Sender<Cmd>> {
    if cfg!(test) {
        return None;
    }
    static TX: OnceLock<Mutex<Sender<Cmd>>> = OnceLock::new();
    let tx = TX.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel();
        super::spawn_named("ws-sound", move || player(rx));
        Mutex::new(tx)
    });
    tx.lock().ok().map(|g| g.clone())
}

fn send(c: Cmd) {
    if let Some(tx) = sender() {
        let _ = tx.send(c);
    }
}

fn open_file(path: &Path) -> Result<rodio::Decoder<std::io::BufReader<std::fs::File>>, String> {
    let f = std::fs::File::open(path).map_err(|e| format!("打不开 {}：{e}", path.display()))?;
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let mut b = rodio::Decoder::builder().with_data(std::io::BufReader::new(f)).with_byte_len(len).with_seekable(true);
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        b = b.with_hint(ext);
    }
    b.build().map_err(|e| format!("解不了 {}：{e}", path.display()))
}

fn player(rx: Receiver<Cmd>) {
    use rodio::Source;
    let mut sink: Option<rodio::MixerDeviceSink> = None;
    let mut last_try: Option<Instant> = None;
    let mut cur: Option<(u8, rodio::Player)> = None;
    loop {
        let cmd = match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(c) => Some(c),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => None,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
        };
        // 放完了就不算「在放」
        if cur.as_ref().is_some_and(|(_, p)| p.empty()) {
            cur = None;
        }
        match cmd {
            None => {}
            Some(Cmd::Stop(only)) => {
                if let Some((tag, p)) = cur.as_ref()
                    && only.is_none_or(|l| l == *tag)
                {
                    p.stop();
                    cur = None;
                }
            }
            Some(Cmd::Play { tag, path, volume, max_secs }) => {
                if sink.is_none() && last_try.is_none_or(|t| t.elapsed() >= RETRY || tag == TEST_TAG) {
                    last_try = Some(Instant::now());
                    match rodio::DeviceSinkBuilder::open_default_sink() {
                        Ok(mut s) => {
                            s.log_on_drop(false);
                            sink = Some(s);
                            if let Ok(mut g) = status_cell().lock() {
                                g.device_err.clear();
                            }
                        }
                        Err(e) => {
                            log::warn!("[sound] 打不开声音设备：{e}");
                            if let Ok(mut g) = status_cell().lock() {
                                g.device_err = format!("打不开声音设备：{e}（30 秒后再试）");
                            }
                        }
                    }
                }
                if let Some(s) = sink.as_ref() {
                    match open_file(&path) {
                        Ok(dec) => {
                            if let Some((_, p)) = cur.take() {
                                p.stop();
                            }
                            let p = rodio::Player::connect_new(s.mixer());
                            p.set_volume(f32::from(volume.min(100)) / 100.0);
                            p.append(dec.take_duration(Duration::from_secs(u64::from(max_secs.clamp(1, 60)))));
                            cur = Some((tag, p));
                        }
                        Err(e) => {
                            log::warn!("[sound] {e}");
                            set_note(format!("✗ {e}"));
                        }
                    }
                }
            }
        }
        if let Ok(mut g) = status_cell().lock() {
            g.playing = cur.as_ref().map(|(t, _)| *t).filter(|t| *t <= 3);
        }
    }
}

// ── 界面动作 ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum SoundMsg {
    On(u8, bool),
    File(u8, String),
    Volume(u8, u8),
    /// 试听这一级
    Test(u8),
    StopTest,
    Repeat(u32),
    MaxSecs(u32),
    /// 静音 1 小时
    MuteHour,
    MuteForever(bool),
    Unmute,
    /// 顶栏喇叭：没静音 → 静音 1 小时；静音中 → 恢复
    ToggleMute,
    Rescan,
    OpenDir,
}

pub fn handle(m: SoundMsg) {
    let now = chrono::Utc::now().timestamp_millis();
    let lv = |l: u8| usize::from(l.min(3));
    match m {
        SoundMsg::On(l, on) => edit(|c| c.levels[lv(l)].on = on),
        SoundMsg::File(l, f) => edit(|c| c.levels[lv(l)].file = f),
        SoundMsg::Volume(l, v) => edit(|c| c.levels[lv(l)].volume = v.min(100)),
        SoundMsg::Test(l) => {
            let c = cfg();
            let lc = c.level(l);
            match resolve(&dir(), &lc.file) {
                Some(path) if path.is_file() => {
                    set_note(String::new());
                    send(Cmd::Play { tag: TEST_TAG, path, volume: lc.volume, max_secs: c.max_secs });
                }
                Some(path) => set_note(format!("✗ 找不到 {}", path.display())),
                None => set_note(format!("✗ P{l} 还没选声音")),
            }
        }
        SoundMsg::StopTest => send(Cmd::Stop(None)),
        SoundMsg::Repeat(s) => edit(|c| c.p0_repeat_secs = s.clamp(3, 600)),
        SoundMsg::MaxSecs(s) => edit(|c| c.max_secs = s.clamp(1, 60)),
        SoundMsg::MuteHour => {
            edit(|c| c.mute_until_ms = now + 3_600_000);
            send(Cmd::Stop(None));
        }
        SoundMsg::MuteForever(on) => {
            edit(|c| c.mute_forever = on);
            if on {
                send(Cmd::Stop(None));
            }
        }
        SoundMsg::Unmute => edit(|c| {
            c.mute_forever = false;
            c.mute_until_ms = 0;
        }),
        SoundMsg::ToggleMute => {
            if cfg().muted(now) {
                handle(SoundMsg::Unmute);
            } else {
                handle(SoundMsg::MuteHour);
            }
        }
        SoundMsg::Rescan => {
            rescan();
            let n = files_cell().lock().map(|g| g.list.len()).unwrap_or(0);
            set_note(format!("✔ 扫到 {n} 个声音文件"));
        }
        SoundMsg::OpenDir => {
            let d = dir();
            if let Err(e) = ensure_dir(&d).and_then(|_| std::process::Command::new("xdg-open").arg(&d).spawn().map(|_| ())) {
                set_note(format!("✗ 打不开 {}：{e}", d.display()));
            }
        }
    }
}

/// 静音状态的一句（顶栏喇叭的悬停 / 设置页）；没静音为空。
#[must_use]
pub fn mute_text(c: &SoundCfg, now_ms: i64) -> String {
    use chrono::TimeZone;
    if c.mute_forever {
        "一直静音".into()
    } else if now_ms < c.mute_until_ms {
        let t = chrono::Local.timestamp_millis_opt(c.mute_until_ms).single().map(|t| t.format("%H:%M").to_string()).unwrap_or_default();
        format!("静音到 {t}")
    } else {
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(id: i64, level: u8) -> Arrival {
        Arrival { id, level, silent: false }
    }

    fn env(now_ms: i64) -> Env {
        Env { now_ms, quiet: false, p0_exempt: true }
    }

    #[test]
    fn 默认_p0到p2响_p3静音() {
        let c = SoundCfg::default();
        assert!(c.wants(0) && c.wants(1) && c.wants(2));
        assert!(!c.wants(3));
        let mut d = Decider::default();
        assert_eq!(d.arrivals(&[a(1, 3)], &c, &env(0)), None);
    }

    #[test]
    fn 同一批只响最高级_同级隔3秒() {
        let c = SoundCfg::default();
        let mut d = Decider::default();
        assert_eq!(d.arrivals(&[a(1, 2), a(2, 1), a(3, 3)], &c, &env(0)), Some(Act::Play(1)));
        assert_eq!(d.arrivals(&[a(4, 1)], &c, &env(1_000)), None, "同级 3 秒内不再响");
        assert_eq!(d.arrivals(&[a(5, 2)], &c, &env(1_000)), Some(Act::Play(2)), "别的级别不受影响");
        assert_eq!(d.arrivals(&[a(6, 1)], &c, &env(3_500)), Some(Act::Play(1)));
    }

    #[test]
    fn 入库即已读的不响() {
        let c = SoundCfg::default();
        let mut d = Decider::default();
        let v = [Arrival { id: 1, level: 0, silent: true }];
        assert_eq!(d.arrivals(&v, &c, &env(0)), None);
        assert!(d.ringing().is_empty(), "历史 / 免打扰挡掉的 P0 也不进「一直响」");
    }

    #[test]
    fn p0一直响到确认() {
        let c = SoundCfg { p0_repeat_secs: 15, ..Default::default() };
        let mut d = Decider::default();
        assert_eq!(d.arrivals(&[a(7, 0)], &c, &env(0)), Some(Act::Play(0)));
        let un: BTreeSet<i64> = [7].into();
        assert_eq!(d.tick(&un, &c, &env(10_000)), None, "没到间隔");
        assert_eq!(d.tick(&un, &c, &env(15_000)), Some(Act::Play(0)));
        assert_eq!(d.tick(&un, &c, &env(20_000)), None);
        assert_eq!(d.tick(&un, &c, &env(30_000)), Some(Act::Play(0)));
        // 点了「知道了」
        assert_eq!(d.tick(&BTreeSet::new(), &c, &env(31_000)), Some(Act::StopP0));
        assert_eq!(d.tick(&BTreeSet::new(), &c, &env(60_000)), None, "停了就不再响");
    }

    #[test]
    fn 静音时不响_结束后没确认的p0接着响() {
        let now = 1_000_000;
        let c = SoundCfg { mute_until_ms: now + 60_000, ..Default::default() };
        let mut d = Decider::default();
        assert_eq!(d.arrivals(&[a(1, 0), a(2, 2)], &c, &env(now)), None);
        let un: BTreeSet<i64> = [1].into();
        assert_eq!(d.tick(&un, &c, &env(now + 30_000)), None);
        assert_eq!(d.tick(&un, &c, &env(now + 61_000)), Some(Act::Play(0)), "静音结束，P0 还没确认");
        let c2 = SoundCfg { mute_forever: true, ..Default::default() };
        assert_eq!(Decider::default().arrivals(&[a(3, 0)], &c2, &env(now)), None);
    }

    #[test]
    fn 免打扰_p0按例外开关() {
        let c = SoundCfg::default();
        let q = Env { now_ms: 0, quiet: true, p0_exempt: true };
        let mut d = Decider::default();
        assert_eq!(d.arrivals(&[a(1, 1), a(2, 2)], &c, &q), None);
        assert_eq!(d.arrivals(&[a(3, 0)], &c, &q), Some(Act::Play(0)), "P0 例外");
        let q2 = Env { p0_exempt: false, ..q };
        let mut d2 = Decider::default();
        assert_eq!(d2.arrivals(&[a(4, 0)], &c, &q2), None);
        assert!(d2.ringing().is_empty());
    }

    #[test]
    fn 关掉或没选文件的级别不响() {
        let mut c = SoundCfg::default();
        c.levels[1].on = false;
        c.levels[2].file.clear();
        let mut d = Decider::default();
        assert_eq!(d.arrivals(&[a(1, 1), a(2, 2)], &c, &env(0)), None);
        c.levels[3] = LevelCfg { on: true, file: "x.mp3".into(), volume: 50 };
        assert_eq!(d.arrivals(&[a(3, 1), a(4, 3)], &c, &env(0)), Some(Act::Play(3)), "高级别不响时轮到响得了的那级");
    }

    #[test]
    fn 设置读写_缺字段用默认() {
        let c = SoundCfg::default();
        let s = serde_json::to_string(&c).unwrap();
        assert_eq!(serde_json::from_str::<SoundCfg>(&s).unwrap(), c);
        let old: SoundCfg = serde_json::from_str(r#"{"max_secs": 5}"#).unwrap();
        assert_eq!(old.max_secs, 5);
        assert_eq!(old.p0_repeat_secs, 15);
        assert!(old.wants(0) && !old.wants(3));
    }

    #[test]
    fn 文件夹_新建写默认音_扫描含一层子文件夹() {
        let root = std::env::temp_dir().join(format!("ws-sounds-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        assert!(ensure_dir(&root).unwrap());
        std::fs::create_dir_all(root.join("我的")).unwrap();
        std::fs::write(root.join("我的/歌.MP3"), b"x").unwrap();
        std::fs::write(root.join("封面.jpg"), b"x").unwrap();
        std::fs::remove_file(root.join("叮.wav")).unwrap();
        assert!(!ensure_dir(&root).unwrap(), "已有的文件夹不再写默认音");
        assert_eq!(scan(&root), vec!["双音.wav".to_string(), "我的/歌.MP3".into(), "警报.wav".into()]);
        let w = std::fs::read(root.join("警报.wav")).unwrap();
        assert_eq!(&w[..4], b"RIFF");
        // 能被 rodio 解开
        let dec = open_file(&root.join("双音.wav")).unwrap();
        assert!(rodio::Source::total_duration(&dec).is_some_and(|d| d > Duration::from_millis(500)));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn 系统里有的_mp3与ogg能解开() {
        // 机器上碰巧有的样本（没有就跳过）：确认 mp3 / vorbis 解码开着
        for p in ["/usr/share/sounds/gnome/default/alerts/hum.ogg", "/usr/lib/claude-desktop/resources/ion-dist/audio/voice/selection/airy.mp3"] {
            let p = Path::new(p);
            if p.is_file() {
                let dec = open_file(p).unwrap_or_else(|e| panic!("{e}"));
                assert!(rodio::Source::channels(&dec).get() > 0);
            }
        }
    }

    #[test]
    fn 只许选文件夹里的() {
        let d = Path::new("/x/sounds");
        assert_eq!(resolve(d, "a/b.mp3"), Some(PathBuf::from("/x/sounds/a/b.mp3")));
        assert_eq!(resolve(d, "../etc/passwd"), None);
        assert_eq!(resolve(d, "/etc/passwd"), None);
        assert_eq!(resolve(d, ""), None);
    }
}
