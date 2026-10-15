//! 按品种的对象集合、命令与撤销重做、存盘（docs/46 §3.3–3.4）。
//!
//! 每个操作是一条命令：创建 / 改（之前, 之后）/ 删。执行一条命令就得到要写盘的变化（写入或删除哪个对象），
//! 撤销 = 执行它的反命令。`Book` 是纯数据结构，可单测；存盘在 `Db`。

use std::path::{Path, PathBuf};

use super::model::{Drawing, FORMAT};

/// 撤销栈上限。
pub const UNDO_LIMIT: usize = 200;

/// 一条命令。
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
             CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
        )?;
        c.execute("INSERT OR IGNORE INTO meta (key, value) VALUES ('format', ?1)", [FORMAT.to_string()])?;
        Ok(c)
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

    /// 写一批变化（一个事务）。
    pub fn write(&self, symbol: &str, fx: &[Effect]) -> Result<(), String> {
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
        assert_eq!((db.get_meta("prefs").as_deref(), db.get_meta("format").as_deref(), db.get_meta("nope")), (Some("{\"magnet\":false}"), Some("1"), None));
        db.write("BINANCE:BTCUSDT", &[Effect::Remove(b.id.clone())]).unwrap();
        let c = rusqlite::Connection::open(dir.join("drawings.sqlite")).unwrap();
        c.execute("INSERT INTO drawings VALUES ('BINANCE:BTCUSDT', 'bad', '{not json', 0)", []).unwrap();
        let (got, errs) = db.load("BINANCE:BTCUSDT");
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!((got, errs.len()), (vec![a2], 1), "坏了的那条跳过并说明，不影响别的");
    }
}
