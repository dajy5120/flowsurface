//! 按品种的对象集合、命令与撤销重做、存盘（docs/46 §3.3–3.4）。
//!
//! 每个操作是一条命令：创建 / 改（之前, 之后）/ 删。执行一条命令就得到要写盘的变化（写入或删除哪个对象），
//! 撤销 = 执行它的反命令。`Book` 是纯数据结构，可单测；存盘在 `Db`。

use std::path::{Path, PathBuf};

use super::model::{Drawing, FORMAT};

/// 撤销栈上限。
pub const UNDO_LIMIT: usize = 200;

/// 一条命令。
// 「改」带前后两份对象，比另两种大一倍；撤销栈最多 200 条，不值得为它装箱
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub enum Cmd {
    Create(Drawing),
    /// (之前, 之后)
    Update(Drawing, Drawing),
    Delete(Drawing),
}

impl Cmd {
    fn inverse(&self) -> Self {
        match self {
            Self::Create(d) => Self::Delete(d.clone()),
            Self::Update(before, after) => Self::Update(after.clone(), before.clone()),
            Self::Delete(d) => Self::Create(d.clone()),
        }
    }
}

/// 写盘要做的事。
// 一次操作只有一两条，大小不值得装箱
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    Upsert(Drawing),
    Remove(String),
}

/// 一个品种的全部对象（按画的先后，后画的在上面）与撤销重做栈。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Book {
    pub items: Vec<Drawing>,
    undo: Vec<Cmd>,
    redo: Vec<Cmd>,
}

impl Book {
    #[must_use]
    pub fn from_items(items: Vec<Drawing>) -> Self {
        Self { items, ..Self::default() }
    }

    #[must_use]
    pub fn get(&self, id: &str) -> Option<&Drawing> {
        self.items.iter().find(|d| d.id == id)
    }

    /// 改一个对象：`after` 与现在一样就什么都不做（不记撤销）。
    pub fn update(&mut self, after: Drawing) -> Vec<Effect> {
        match self.get(&after.id) {
            Some(before) if *before != after => {
                let before = before.clone();
                self.exec(Cmd::Update(before, after))
            }
            _ => Vec::new(),
        }
    }

    /// 不记撤销地换掉一个对象（到价提醒响了之后改它的状态：这不是用户的编辑，不该被 Ctrl+Z 撤掉）。
    pub fn replace_silently(&mut self, d: Drawing) -> Vec<Effect> {
        match self.items.iter_mut().find(|x| x.id == d.id) {
            Some(slot) if *slot != d => {
                *slot = d.clone();
                vec![Effect::Upsert(d)]
            }
            _ => Vec::new(),
        }
    }

    /// 不记撤销地加一个对象（另存为分析时复制画线：那是一次整体操作，不该被 Ctrl+Z 一条条撤掉；docs/52）。
    pub fn insert_silently(&mut self, d: Drawing) -> Vec<Effect> {
        if self.get(&d.id).is_some() {
            return Vec::new();
        }
        self.items.push(d.clone());
        vec![Effect::Upsert(d)]
    }

    /// 不记撤销地把一份分析的手工对象整套换掉（回退到一个版本；docs/52 A4）：先删光它现在的，再按版本里的先后放回。
    /// 撤销 / 重做栈一并清掉——栈里的命令指的是换掉之前的对象，留着会撤出东西来。
    pub fn replace_sheet(&mut self, sheet: &Option<String>, items: Vec<Drawing>) -> Vec<Effect> {
        let mine = |d: &Drawing| d.sheet == *sheet && d.source == super::model::Source::Manual;
        let mut fx: Vec<Effect> = self.items.iter().filter(|d| mine(d)).map(|d| Effect::Remove(d.id.clone())).collect();
        self.items.retain(|d| !mine(d));
        for mut d in items {
            d.sheet.clone_from(sheet);
            self.items.retain(|x| x.id != d.id);
            fx.push(Effect::Upsert(d.clone()));
            self.items.push(d);
        }
        self.undo.clear();
        self.redo.clear();
        fx
    }

    /// 执行一条新命令（清掉重做栈）。
    pub fn exec(&mut self, cmd: Cmd) -> Vec<Effect> {
        let fx = self.apply(&cmd);
        if !fx.is_empty() {
            self.undo.push(cmd);
            if self.undo.len() > UNDO_LIMIT {
                self.undo.remove(0);
            }
            self.redo.clear();
        }
        fx
    }

    pub fn undo(&mut self) -> Vec<Effect> {
        let Some(cmd) = self.undo.pop() else { return Vec::new() };
        let fx = self.apply(&cmd.inverse());
        self.redo.push(cmd);
        fx
    }

    pub fn redo(&mut self) -> Vec<Effect> {
        let Some(cmd) = self.redo.pop() else { return Vec::new() };
        let fx = self.apply(&cmd);
        self.undo.push(cmd);
        fx
    }

    #[must_use]
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    fn apply(&mut self, cmd: &Cmd) -> Vec<Effect> {
        match cmd {
            Cmd::Create(d) => {
                if self.get(&d.id).is_some() {
                    return Vec::new();
                }
                self.items.push(d.clone());
                vec![Effect::Upsert(d.clone())]
            }
            Cmd::Update(_, after) => match self.items.iter_mut().find(|d| d.id == after.id) {
                Some(slot) => {
                    *slot = after.clone();
                    vec![Effect::Upsert(after.clone())]
                }
                None => Vec::new(),
            },
            Cmd::Delete(d) => {
                let before = self.items.len();
                self.items.retain(|x| x.id != d.id);
                if self.items.len() == before { Vec::new() } else { vec![Effect::Remove(d.id.clone())] }
            }
        }
    }
}

/// 格式迁移（G5）：第 i 步把格式 i+1 的对象 JSON 改成格式 i+2。打开库时按 `meta.format` 从旧往新逐步跑，
/// 跑完写回新格式。加字段（可选、有缺省）不用改旧对象，那一步是空的；改名、拆字段才要写。
pub const MIGRATIONS: &[fn(&mut serde_json::Value)] = &[v1_to_v2];

/// 1 → 2：加了到价提醒 `alert`（可选），旧对象原样。
fn v1_to_v2(_d: &mut serde_json::Value) {}

/// 一份分析（docs/52）：一个品种上的一套画线，有名字，能另存、打开、叠加显示。
/// 画线对象用 `sheet` 记它属于哪份分析；没有 `sheet` 的对象属于默认画板（id 为空串，不进这张表）。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Analysis {
    pub id: String,
    pub symbol: String,
    pub name: String,
    /// 说明 / 结论（A3）
    #[serde(default)]
    pub note: String,
    pub created_ms: i64,
    pub updated_ms: i64,
    /// 认不得的字段（A2 起的数据段、视图、回放状态等）原样保留
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// 一个品种的画板：当前分析（新画的线归它，`None` = 默认画板）与叠加显示的分析（淡色、只读）。
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Board {
    #[serde(default)]
    pub active: Option<String>,
    /// 叠加显示的分析（`None` = 默认画板）
    #[serde(default)]
    pub shown: Vec<Option<String>>,
}

impl Board {
    /// 这份分析的线画不画：当前的、叠加的。
    #[must_use]
    pub fn visible(&self, sheet: &Option<String>) -> bool {
        self.active == *sheet || self.shown.contains(sheet)
    }
}

/// 一份分析的一个版本（docs/52 A4）：手动「存一个版本」时它的全部手工画线与结论。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Version {
    pub id: String,
    pub symbol: String,
    /// 分析 id（默认画板 = 空串）
    pub owner: String,
    pub label: String,
    pub created_ms: i64,
    #[serde(default)]
    pub note: String,
    pub drawings: Vec<Drawing>,
}

/// 现在的毫秒（分析的创建 / 修改时间）。
#[must_use]
pub fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

/// 存盘：Cockpit 数据目录下的 `drawings.sqlite`（与 `saved-state.json` 同处）。
pub struct Db {
    path: PathBuf,
}

impl Db {
    #[must_use]
    pub fn default_path() -> PathBuf {
        data::data_path(Some("drawings.sqlite"))
    }

    #[must_use]
    pub fn at(path: &Path) -> Self {
        Self { path: path.to_path_buf() }
    }

    fn open(&self) -> rusqlite::Result<rusqlite::Connection> {
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let c = rusqlite::Connection::open(&self.path)?;
        c.execute_batch(
            "CREATE TABLE IF NOT EXISTS drawings (symbol TEXT NOT NULL, id TEXT NOT NULL, body TEXT NOT NULL,
                 updated_ms INTEGER NOT NULL, PRIMARY KEY (symbol, id));
             CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS analyses (id TEXT PRIMARY KEY, symbol TEXT NOT NULL, body TEXT NOT NULL, updated_ms INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS versions (id TEXT PRIMARY KEY, symbol TEXT NOT NULL, owner TEXT NOT NULL, body TEXT NOT NULL,
                 created_ms INTEGER NOT NULL);",
        )?;
        c.execute("INSERT OR IGNORE INTO meta (key, value) VALUES ('format', ?1)", [FORMAT.to_string()])?;
        let stored: u32 = c
            .query_row("SELECT value FROM meta WHERE key='format'", [], |r| r.get::<_, String>(0))?
            .parse()
            .unwrap_or(FORMAT);
        if stored < FORMAT {
            Self::migrate(&c, stored)?;
        }
        Ok(c)
    }

    /// 把库里每个对象从 `from` 迁到当前格式（一个事务）。
    fn migrate(c: &rusqlite::Connection, from: u32) -> rusqlite::Result<()> {
        let rows: Vec<(String, String, String)> = c
            .prepare("SELECT symbol, id, body FROM drawings")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?;
        c.execute_batch("BEGIN")?;
        for (sym, id, body) in rows {
            let Ok(mut v) = serde_json::from_str::<serde_json::Value>(&body) else { continue };
            for step in MIGRATIONS.iter().skip(from.saturating_sub(1) as usize).take(FORMAT.saturating_sub(from) as usize) {
                step(&mut v);
            }
            c.execute("UPDATE drawings SET body = ?3 WHERE symbol = ?1 AND id = ?2", rusqlite::params![sym, id, v.to_string()])?;
        }
        c.execute("UPDATE meta SET value = ?1 WHERE key = 'format'", [FORMAT.to_string()])?;
        c.execute_batch("COMMIT")
    }

    /// 库的格式比这个程序新（更新版本的程序写的）：只读——读得进来，但不写回，免得把不认识的东西写坏
    /// （docs/45 C8 的教训）。
    #[must_use]
    pub fn newer_than_me(&self) -> Option<u32> {
        let c = rusqlite::Connection::open(&self.path).ok()?;
        let v: u32 = c.query_row("SELECT value FROM meta WHERE key='format'", [], |r| r.get::<_, String>(0)).ok()?.parse().ok()?;
        (v > FORMAT).then_some(v)
    }

    /// 一个品种的全部对象（按写入先后）。读不了 / 某条坏了：跳过并返回说明。
    pub fn load(&self, symbol: &str) -> (Vec<Drawing>, Vec<String>) {
        let mut errs = Vec::new();
        let rows = self.open().and_then(|c| {
            let mut st = c.prepare("SELECT id, body FROM drawings WHERE symbol = ?1 ORDER BY rowid")?;
            st.query_map([symbol], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()
        });
        let rows = match rows {
            Ok(r) => r,
            Err(e) => {
                errs.push(format!("读不了 {}：{e}", self.path.display()));
                Vec::new()
            }
        };
        let mut out = Vec::new();
        for (id, body) in rows {
            match serde_json::from_str::<Drawing>(&body) {
                Ok(d) => out.push(d),
                Err(e) => errs.push(format!("对象 {id} 读不了：{e}")),
            }
        }
        (out, errs)
    }

    /// 读一项设置（`meta` 表；绘图的偏好存在 `prefs`）。
    #[must_use]
    pub fn get_meta(&self, key: &str) -> Option<String> {
        let c = self.open().ok()?;
        c.query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get::<_, String>(0)).ok()
    }

    pub fn set_meta(&self, key: &str, value: &str) -> Result<(), String> {
        let c = self.open().map_err(|e| e.to_string())?;
        c.execute("INSERT INTO meta (key, value) VALUES (?1, ?2) ON CONFLICT (key) DO UPDATE SET value = excluded.value", [key, value])
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// 这个品种的分析（按创建先后）。读不了的那条跳过。
    #[must_use]
    pub fn analyses(&self, symbol: &str) -> Vec<Analysis> {
        let Ok(c) = self.open() else { return Vec::new() };
        let Ok(mut st) = c.prepare("SELECT body FROM analyses WHERE symbol = ?1") else { return Vec::new() };
        let rows: Vec<String> = st.query_map([symbol], |r| r.get::<_, String>(0)).map(|it| it.flatten().collect()).unwrap_or_default();
        let mut v: Vec<Analysis> = rows.iter().filter_map(|b| serde_json::from_str(b).ok()).collect();
        v.sort_by_key(|a| a.created_ms);
        v
    }

    /// 全部品种的分析（按修改时间新的在前）。
    #[must_use]
    pub fn all_analyses(&self) -> Vec<Analysis> {
        let Ok(c) = self.open() else { return Vec::new() };
        let Ok(mut st) = c.prepare("SELECT body FROM analyses ORDER BY updated_ms DESC") else { return Vec::new() };
        let rows: Vec<String> = st.query_map([], |r| r.get::<_, String>(0)).map(|it| it.flatten().collect()).unwrap_or_default();
        rows.iter().filter_map(|b| serde_json::from_str(b).ok()).collect()
    }

    pub fn put_analysis(&self, a: &Analysis) -> Result<(), String> {
        let c = self.open().map_err(|e| e.to_string())?;
        c.execute(
            "INSERT INTO analyses (id, symbol, body, updated_ms) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (id) DO UPDATE SET body = excluded.body, updated_ms = excluded.updated_ms",
            rusqlite::params![a.id, a.symbol, serde_json::to_string(a).unwrap_or_default(), a.updated_ms],
        )
        .map(|_| ())
        .map_err(|e| e.to_string())
    }

    pub fn delete_analysis(&self, id: &str) -> Result<(), String> {
        let c = self.open().map_err(|e| e.to_string())?;
        c.execute("DELETE FROM analyses WHERE id = ?1", [id]).map(|_| ()).map_err(|e| e.to_string())
    }

    /// 画板状态（`meta` 表键 `board:<品种>`）。
    #[must_use]
    pub fn board(&self, symbol: &str) -> Board {
        self.get_meta(&format!("board:{symbol}")).and_then(|j| serde_json::from_str(&j).ok()).unwrap_or_default()
    }

    pub fn set_board(&self, symbol: &str, b: &Board) -> Result<(), String> {
        self.set_meta(&format!("board:{symbol}"), &serde_json::to_string(b).unwrap_or_default())
    }

    /// 一份分析的版本（新的在前）。`owner` 空串 = 这个品种的默认画板。
    #[must_use]
    pub fn versions(&self, symbol: &str, owner: &str) -> Vec<Version> {
        let Ok(c) = self.open() else { return Vec::new() };
        let Ok(mut st) = c.prepare("SELECT body FROM versions WHERE symbol = ?1 AND owner = ?2 ORDER BY created_ms DESC, rowid DESC") else {
            return Vec::new();
        };
        let rows: Vec<String> = st.query_map([symbol, owner], |r| r.get::<_, String>(0)).map(|it| it.flatten().collect()).unwrap_or_default();
        rows.iter().filter_map(|b| serde_json::from_str(b).ok()).collect()
    }

    pub fn put_version(&self, v: &Version) -> Result<(), String> {
        let c = self.open().map_err(|e| e.to_string())?;
        c.execute(
            "INSERT OR REPLACE INTO versions (id, symbol, owner, body, created_ms) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![v.id, v.symbol, v.owner, serde_json::to_string(v).unwrap_or_default(), v.created_ms],
        )
        .map(|_| ())
        .map_err(|e| e.to_string())
    }

    /// 删一个版本（`id`），或一份分析的全部版本（`owner`，删分析时）。
    pub fn delete_versions(&self, id: Option<&str>, owner: Option<(&str, &str)>) -> Result<(), String> {
        let c = self.open().map_err(|e| e.to_string())?;
        let r = match (id, owner) {
            (Some(id), _) => c.execute("DELETE FROM versions WHERE id = ?1", [id]),
            (None, Some((sym, o))) => c.execute("DELETE FROM versions WHERE symbol = ?1 AND owner = ?2", [sym, o]),
            (None, None) => Ok(0),
        };
        r.map(|_| ()).map_err(|e| e.to_string())
    }

    /// 写一批变化（一个事务）。
    pub fn write(&self, symbol: &str, fx: &[Effect]) -> Result<(), String> {
        if let Some(v) = self.newer_than_me() {
            return Err(format!("画线库是更新版本的程序写的（格式 {v}，这个程序认到 {FORMAT}）：只读，不写盘；请更新 Cockpit"));
        }
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64);
        let mut c = self.open().map_err(|e| e.to_string())?;
        let tx = c.transaction().map_err(|e| e.to_string())?;
        for f in fx {
            let r = match f {
                // 改动保持原来的先后（rowid 不变）：ON CONFLICT 更新而不是删了重插
                Effect::Upsert(d) => tx.execute(
                    "INSERT INTO drawings (symbol, id, body, updated_ms) VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT (symbol, id) DO UPDATE SET body = excluded.body, updated_ms = excluded.updated_ms",
                    rusqlite::params![symbol, d.id, serde_json::to_string(d).unwrap_or_default(), now],
                ),
                Effect::Remove(id) => tx.execute("DELETE FROM drawings WHERE symbol = ?1 AND id = ?2", rusqlite::params![symbol, id]),
            };
            r.map_err(|e| e.to_string())?;
        }
        tx.commit().map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::super::model::{Anchor, Kind};
    use super::*;

    fn line(p: i64) -> Drawing {
        Drawing::new(Kind::HLine, vec![Anchor { t: 1, p }])
    }

    #[test]
    fn commands_undo_and_redo() {
        let mut b = Book::default();
        let d = line(100);
        assert_eq!(b.exec(Cmd::Create(d.clone())), vec![Effect::Upsert(d.clone())]);
        let mut moved = d.clone();
        moved.anchors[0].p = 120;
        assert_eq!(b.update(moved.clone()), vec![Effect::Upsert(moved.clone())]);
        assert!(b.update(moved.clone()).is_empty(), "没变不记撤销");
        assert_eq!(b.exec(Cmd::Delete(moved.clone())), vec![Effect::Remove(d.id.clone())]);
        assert!(b.items.is_empty());
        assert_eq!(b.undo(), vec![Effect::Upsert(moved.clone())], "撤销删除 = 放回来");
        assert_eq!(b.undo(), vec![Effect::Upsert(d.clone())], "撤销移动 = 回到原价");
        assert_eq!(b.items[0].anchors[0].p, 100);
        assert_eq!(b.redo(), vec![Effect::Upsert(moved.clone())]);
        assert_eq!(b.undo(), vec![Effect::Upsert(d.clone())]);
        assert_eq!(b.undo(), vec![Effect::Remove(d.id.clone())]);
        assert!(b.undo().is_empty() && !b.can_undo());
        // 新命令清掉重做栈
        b.redo();
        b.exec(Cmd::Create(line(5)));
        assert!(b.redo().is_empty());
    }

    #[test]
    fn analyses_and_boards_round_trip() {
        let dir = std::env::temp_dir().join(format!("ws-drawings-ana-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::at(&dir.join("d.sqlite"));
        let a = Analysis { id: "a1".into(), symbol: "S".into(), name: "吸收".into(), note: String::new(), created_ms: 2, updated_ms: 2, extra: Default::default() };
        let mut b = a.clone();
        b.id = "a0".into();
        b.created_ms = 1;
        db.put_analysis(&a).unwrap();
        db.put_analysis(&b).unwrap();
        assert_eq!(db.analyses("S").iter().map(|x| x.id.as_str()).collect::<Vec<_>>(), vec!["a0", "a1"], "按创建先后");
        assert!(db.analyses("T").is_empty());
        db.delete_analysis("a0").unwrap();
        assert_eq!(db.analyses("S").len(), 1);
        assert_eq!(db.board("S"), Board::default());
        let bd = Board { active: Some("a1".into()), shown: vec![None] };
        db.set_board("S", &bd).unwrap();
        assert_eq!(db.board("S"), bd);
        assert!(bd.visible(&None) && bd.visible(&Some("a1".into())) && !bd.visible(&Some("x".into())));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn versions_round_trip_and_replace_sheet() {
        let dir = std::env::temp_dir().join(format!("ws-drawings-ver-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let db = Db::at(&dir.join("d.sqlite"));
        let v = |id: &str, owner: &str, t: i64| Version { id: id.into(), symbol: "S".into(), owner: owner.into(), label: id.into(), created_ms: t, note: String::new(), drawings: vec![line(t)] };
        let v1 = v("v1", "a", 1);
        db.put_version(&v1).unwrap();
        db.put_version(&v("v2", "a", 2)).unwrap();
        db.put_version(&v("d1", "", 3)).unwrap();
        assert_eq!(db.versions("S", "a").iter().map(|x| x.id.as_str()).collect::<Vec<_>>(), vec!["v2", "v1"], "新的在前");
        assert_eq!(db.versions("S", "a")[1], v1, "逐字段读回");
        assert_eq!(db.versions("S", "").len(), 1);
        db.delete_versions(Some("v2"), None).unwrap();
        assert_eq!(db.versions("S", "a").len(), 1);
        db.delete_versions(None, Some(("S", "a"))).unwrap();
        assert!(db.versions("S", "a").is_empty() && db.versions("S", "").len() == 1);
        let _ = std::fs::remove_dir_all(&dir);

        // 换掉一份分析的画线：别的分析、系统对象不动；撤销栈清空
        let sheet = Some("a".to_string());
        let mut mine = line(1);
        mine.sheet.clone_from(&sheet);
        let other = line(2);
        let mut b = Book::default();
        b.exec(Cmd::Create(mine.clone()));
        b.exec(Cmd::Create(other.clone()));
        let fresh = line(3);
        let fx = b.replace_sheet(&sheet, vec![fresh.clone()]);
        let mut want = fresh.clone();
        want.sheet.clone_from(&sheet);
        assert_eq!(fx, vec![Effect::Remove(mine.id.clone()), Effect::Upsert(want.clone())]);
        assert_eq!(b.items, vec![other, want]);
        assert!(!b.can_undo() && b.undo().is_empty());
    }

    #[test]
    fn old_databases_migrate_and_newer_ones_are_read_only() {
        let dir = std::env::temp_dir().join(format!("ws-drawings-mig-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // 格式 1 的库（G0–G4 写的）
        let p = dir.join("old.sqlite");
        let c = rusqlite::Connection::open(&p).unwrap();
        c.execute_batch(
            "CREATE TABLE drawings (symbol TEXT NOT NULL, id TEXT NOT NULL, body TEXT NOT NULL, updated_ms INTEGER NOT NULL, PRIMARY KEY (symbol, id));
             CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             INSERT INTO meta VALUES ('format', '1');",
        )
        .unwrap();
        c.execute("INSERT INTO drawings VALUES ('S', 'a', ?1, 0)", [serde_json::to_string(&line(7)).unwrap()]).unwrap();
        drop(c);
        let db = Db::at(&p);
        let (got, errs) = db.load("S");
        assert!(errs.is_empty() && got.len() == 1 && got[0].alert.is_none());
        assert_eq!(db.get_meta("format").as_deref(), Some(FORMAT.to_string().as_str()), "迁完写回新格式");
        assert!(db.write("S", &[Effect::Upsert(line(8))]).is_ok());
        // 更新版本写的库：读得进来，写不了
        let q = dir.join("new.sqlite");
        let db2 = Db::at(&q);
        db2.write("S", &[Effect::Upsert(line(1))]).unwrap();
        db2.set_meta("format", "99").unwrap();
        assert_eq!(db2.load("S").0.len(), 1);
        let e = db2.write("S", &[Effect::Upsert(line(2))]).unwrap_err();
        assert!(e.contains("格式 99") && e.contains("只读"), "{e}");
        assert_eq!(db2.load("S").0.len(), 1, "没写进去");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn database_round_trip_keeps_order_and_skips_broken_rows() {
        let dir = std::env::temp_dir().join(format!("ws-drawings-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let db = Db::at(&dir.join("drawings.sqlite"));
        let (a, b) = (line(1), line(2));
        db.write("BINANCE:BTCUSDT", &[Effect::Upsert(a.clone()), Effect::Upsert(b.clone())]).unwrap();
        let mut a2 = a.clone();
        a2.anchors[0].p = 9;
        db.write("BINANCE:BTCUSDT", &[Effect::Upsert(a2.clone())]).unwrap();
        db.write("OTHER", &[Effect::Upsert(line(3))]).unwrap();
        let (got, errs) = db.load("BINANCE:BTCUSDT");
        assert!(errs.is_empty());
        assert_eq!(got, vec![a2.clone(), b.clone()], "改过的对象留在原来的位置（先后不变）");
        db.set_meta("prefs", "{\"magnet\":true}").unwrap();
        db.set_meta("prefs", "{\"magnet\":false}").unwrap();
        assert_eq!((db.get_meta("prefs").as_deref(), db.get_meta("format").as_deref(), db.get_meta("nope")), (Some("{\"magnet\":false}"), Some(FORMAT.to_string().as_str()), None));
        db.write("BINANCE:BTCUSDT", &[Effect::Remove(b.id.clone())]).unwrap();
        let c = rusqlite::Connection::open(dir.join("drawings.sqlite")).unwrap();
        c.execute("INSERT INTO drawings VALUES ('BINANCE:BTCUSDT', 'bad', '{not json', 0)", []).unwrap();
        let (got, errs) = db.load("BINANCE:BTCUSDT");
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!((got, errs.len()), (vec![a2], 1), "坏了的那条跳过并说明，不影响别的");
    }
}
