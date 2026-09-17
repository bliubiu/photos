//! 存储：sqlite（WAL 模式），位于 `data/`。
//!
//! 三张表：
//! - `task_history`：任务历史（只存元数据，不存像素/人脸框/关键点坐标）
//! - `kv_cache`：模型校验等通用键值缓存
//! - `prefs`：偏好设置

use std::path::Path;

use chrono::Local;
use rusqlite::{Connection, OptionalExtension, params};

use crate::error::{CoreError, CoreResult};

/// 存储句柄（内部持有 sqlite 连接）
#[derive(Debug)]
pub struct Store {
    conn: Connection,
}

/// 任务记录（task_history 一行）
#[derive(Debug, Clone, PartialEq)]
pub struct TaskRecord {
    pub id: i64,
    pub input_path: String,
    pub mode: String,
    pub size: String,
    pub backgrounds: String,
    pub beauty: String,
    pub dress: String,
    pub rotate: Option<f64>,
    pub outputs: String,
    pub status: String,
    pub message: String,
    pub warnings: String,
    pub created_at: String,
    pub elapsed_ms: Option<i64>,
}

/// 新任务（插入用）
#[derive(Debug, Clone)]
pub struct NewTask {
    pub input_path: String,
    pub mode: String,
    pub size: String,
    pub backgrounds: String,
    pub beauty: String,
    pub dress: String,
    pub rotate: Option<f64>,
    pub outputs: String,
    pub status: String,
    pub message: String,
    pub warnings: String,
    pub elapsed_ms: Option<i64>,
}

impl Store {
    /// 打开（或创建）数据库；自动创建父目录并执行 WAL 迁移
    pub fn open(db_path: &Path) -> CoreResult<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(db_path).map_err(|e| {
            CoreError::Storage(format!("打开数据库 {} 失败：{e}", db_path.display()))
        })?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(|e| CoreError::Storage(format!("启用 WAL 失败：{e}")))?;
        conn.pragma_update(None, "busy_timeout", 5000)
            .map_err(|e| CoreError::Storage(format!("设置 busy_timeout 失败：{e}")))?;
        let store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    /// 建表迁移（幂等）
    fn migrate(&self) -> CoreResult<()> {
        self.conn
            .execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS task_history (
                    id          INTEGER PRIMARY KEY AUTOINCREMENT,
                    input_path  TEXT NOT NULL,
                    mode        TEXT NOT NULL,
                    size        TEXT NOT NULL,
                    backgrounds TEXT NOT NULL,
                    beauty      TEXT NOT NULL DEFAULT '',
                    dress       TEXT NOT NULL DEFAULT '',
                    rotate      REAL NULL,
                    outputs     TEXT NOT NULL DEFAULT '',
                    status      TEXT NOT NULL DEFAULT 'queued',
                    message     TEXT NOT NULL DEFAULT '',
                    warnings    TEXT NOT NULL DEFAULT '',
                    created_at  TEXT NOT NULL,
                    elapsed_ms  INTEGER NULL
                );
                CREATE TABLE IF NOT EXISTS kv_cache (
                    key        TEXT PRIMARY KEY,
                    value      TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS prefs (
                    key        TEXT PRIMARY KEY,
                    value      TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                );
                "#,
            )
            .map_err(|e| CoreError::Storage(format!("建表失败：{e}")))?;
        // 旧库迁移：v2 新增 dress 列（CREATE TABLE IF NOT EXISTS 不作用于已存在的表）
        let has_dress = {
            let mut stmt = self
                .conn
                .prepare("PRAGMA table_info(task_history)")
                .map_err(|e| CoreError::Storage(format!("查询任务表结构失败：{e}")))?;
            let cols = stmt
                .query_map([], |r| r.get::<_, String>(1))
                .map_err(|e| CoreError::Storage(format!("查询任务表结构失败：{e}")))?;
            cols.collect::<Result<Vec<_>, _>>()
                .map_err(|e| CoreError::Storage(format!("查询任务表结构失败：{e}")))?
        };
        if !has_dress.iter().any(|c| c == "dress") {
            self.conn
                .execute_batch("ALTER TABLE task_history ADD COLUMN dress TEXT NOT NULL DEFAULT ''")
                .map_err(|e| CoreError::Storage(format!("迁移任务表新增 dress 列失败：{e}")))?;
        }
        Ok(())
    }

    fn now() -> String {
        Local::now().format("%Y-%m-%d %H:%M:%S%.3f").to_string()
    }

    // ---- kv_cache ----

    /// 写入缓存键值
    pub fn kv_set(&self, key: &str, value: &str) -> CoreResult<()> {
        self.conn
            .execute(
                "INSERT INTO kv_cache (key, value, updated_at) VALUES (?1, ?2, ?3)
                 ON CONFLICT(key) DO UPDATE SET value = ?2, updated_at = ?3",
                params![key, value, Self::now()],
            )
            .map_err(|e| CoreError::Storage(format!("写入缓存失败：{e}")))?;
        Ok(())
    }

    /// 读取缓存键值
    pub fn kv_get(&self, key: &str) -> CoreResult<Option<String>> {
        self.conn
            .query_row(
                "SELECT value FROM kv_cache WHERE key = ?1",
                params![key],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| CoreError::Storage(format!("读取缓存失败：{e}")))
    }

    /// 删除缓存键值
    pub fn kv_delete(&self, key: &str) -> CoreResult<()> {
        self.conn
            .execute("DELETE FROM kv_cache WHERE key = ?1", params![key])
            .map_err(|e| CoreError::Storage(format!("删除缓存失败：{e}")))?;
        Ok(())
    }

    // ---- prefs ----

    /// 写入偏好
    pub fn set_pref(&self, key: &str, value: &str) -> CoreResult<()> {
        self.conn
            .execute(
                "INSERT INTO prefs (key, value, updated_at) VALUES (?1, ?2, ?3)
                 ON CONFLICT(key) DO UPDATE SET value = ?2, updated_at = ?3",
                params![key, value, Self::now()],
            )
            .map_err(|e| CoreError::Storage(format!("写入偏好失败：{e}")))?;
        Ok(())
    }

    /// 读取偏好
    pub fn get_pref(&self, key: &str) -> CoreResult<Option<String>> {
        self.conn
            .query_row(
                "SELECT value FROM prefs WHERE key = ?1",
                params![key],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| CoreError::Storage(format!("读取偏好失败：{e}")))
    }

    // ---- task_history ----

    /// 插入任务，返回自增 id
    pub fn insert_task(&self, t: &NewTask) -> CoreResult<i64> {
        self.conn
            .execute(
                "INSERT INTO task_history
                 (input_path, mode, size, backgrounds, beauty, dress, rotate, outputs, status, message, warnings, created_at, elapsed_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                params![
                    t.input_path,
                    t.mode,
                    t.size,
                    t.backgrounds,
                    t.beauty,
                    t.dress,
                    t.rotate,
                    t.outputs,
                    t.status,
                    t.message,
                    t.warnings,
                    Self::now(),
                    t.elapsed_ms,
                ],
            )
            .map_err(|e| CoreError::Storage(format!("插入任务失败：{e}")))?;
        Ok(self.conn.last_insert_rowid())
    }

    /// 更新任务状态与结果（运行中 / 成功 / 失败）
    pub fn update_task(
        &self,
        id: i64,
        status: &str,
        message: &str,
        outputs: &str,
        warnings: &str,
        elapsed_ms: Option<i64>,
    ) -> CoreResult<()> {
        self.conn
            .execute(
                "UPDATE task_history
                 SET status = ?1, message = ?2, outputs = ?3, warnings = ?4, elapsed_ms = ?5
                 WHERE id = ?6",
                params![status, message, outputs, warnings, elapsed_ms, id],
            )
            .map_err(|e| CoreError::Storage(format!("更新任务失败：{e}")))?;
        Ok(())
    }

    /// 查询单个任务
    pub fn get_task(&self, id: i64) -> CoreResult<Option<TaskRecord>> {
        self.conn
            .query_row(
                "SELECT id, input_path, mode, size, backgrounds, beauty, dress, rotate, outputs, status, message, warnings, created_at, elapsed_ms
                 FROM task_history WHERE id = ?1",
                params![id],
                row_to_task,
            )
            .optional()
            .map_err(|e| CoreError::Storage(format!("查询任务失败：{e}")))
    }

    /// 任务列表（按创建时间倒序）
    pub fn list_tasks(&self, limit: i64) -> CoreResult<Vec<TaskRecord>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, input_path, mode, size, backgrounds, beauty, dress, rotate, outputs, status, message, warnings, created_at, elapsed_ms
                 FROM task_history ORDER BY id DESC LIMIT ?1",
            )
            .map_err(|e| CoreError::Storage(format!("准备任务列表查询失败：{e}")))?;
        let rows = stmt
            .query_map(params![limit], row_to_task)
            .map_err(|e| CoreError::Storage(format!("查询任务列表失败：{e}")))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| CoreError::Storage(format!("读取任务列表失败：{e}")))
    }

    /// 任务总数
    pub fn count_tasks(&self) -> CoreResult<i64> {
        self.conn
            .query_row("SELECT COUNT(*) FROM task_history", [], |r| r.get(0))
            .map_err(|e| CoreError::Storage(format!("统计任务数失败：{e}")))
    }

    /// 任务分页列表（按 id 倒序，`limit`/`offset` 分页；供 GET /tasks 使用）
    pub fn list_tasks_paged(&self, limit: i64, offset: i64) -> CoreResult<Vec<TaskRecord>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, input_path, mode, size, backgrounds, beauty, dress, rotate, outputs, status, message, warnings, created_at, elapsed_ms
                 FROM task_history ORDER BY id DESC LIMIT ?1 OFFSET ?2",
            )
            .map_err(|e| CoreError::Storage(format!("准备任务分页查询失败：{e}")))?;
        let rows = stmt
            .query_map(params![limit, offset], row_to_task)
            .map_err(|e| CoreError::Storage(format!("查询任务分页失败：{e}")))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| CoreError::Storage(format!("读取任务分页失败：{e}")))
    }
}

fn row_to_task(row: &rusqlite::Row<'_>) -> rusqlite::Result<TaskRecord> {
    Ok(TaskRecord {
        id: row.get(0)?,
        input_path: row.get(1)?,
        mode: row.get(2)?,
        size: row.get(3)?,
        backgrounds: row.get(4)?,
        beauty: row.get(5)?,
        dress: row.get(6)?,
        rotate: row.get(7)?,
        outputs: row.get(8)?,
        status: row.get(9)?,
        message: row.get(10)?,
        warnings: row.get(11)?,
        created_at: row.get(12)?,
        elapsed_ms: row.get(13)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_temp() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("photos.db")).unwrap();
        (dir, store)
    }

    fn new_task() -> NewTask {
        NewTask {
            input_path: "in.jpg".into(),
            mode: "balanced".into(),
            size: "one_inch".into(),
            backgrounds: "white".into(),
            beauty: String::new(),
            dress: String::new(),
            rotate: Some(2.5),
            outputs: r#"[{"kind":"证件照","path":"data/out/task_1_one_inch_white.jpg"}]"#.into(),
            status: "succeeded".into(),
            message: "处理成功".into(),
            warnings: String::new(),
            elapsed_ms: Some(123),
        }
    }

    #[test]
    fn 三张表存在且wal启用() {
        let (_d, store) = open_temp();
        let mut stmt = store
            .conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name IN ('task_history','kv_cache','prefs')")
            .unwrap();
        let names: Vec<String> = stmt
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(names.len(), 3);
        let wal: String = store
            .conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(wal.to_lowercase(), "wal");
    }

    #[test]
    fn kv缓存增删查() {
        let (_d, store) = open_temp();
        store.kv_set("sha256:x:1", "abc").unwrap();
        assert_eq!(store.kv_get("sha256:x:1").unwrap().as_deref(), Some("abc"));
        store.kv_set("sha256:x:1", "def").unwrap(); // 覆盖
        assert_eq!(store.kv_get("sha256:x:1").unwrap().as_deref(), Some("def"));
        store.kv_delete("sha256:x:1").unwrap();
        assert_eq!(store.kv_get("sha256:x:1").unwrap(), None);
    }

    #[test]
    fn 偏好读写() {
        let (_d, store) = open_temp();
        store.set_pref("last_mode", "quality").unwrap();
        assert_eq!(
            store.get_pref("last_mode").unwrap().as_deref(),
            Some("quality")
        );
        assert_eq!(store.get_pref("不存在").unwrap(), None);
    }

    #[test]
    fn 任务增查改() {
        let (_d, store) = open_temp();
        let id = store.insert_task(&new_task()).unwrap();
        let t = store.get_task(id).unwrap().unwrap();
        assert_eq!(t.status, "succeeded");
        assert_eq!(t.rotate, Some(2.5));
        assert_eq!(t.mode, "balanced");
        assert!(t.created_at.starts_with("20"));

        // 更新为运行中 → 失败
        store
            .update_task(id, "running", "处理中", "", "", None)
            .unwrap();
        assert_eq!(store.get_task(id).unwrap().unwrap().status, "running");
        store
            .update_task(id, "failed", "处理失败：示例错误", "", "", Some(1))
            .unwrap();
        let failed = store.get_task(id).unwrap().unwrap();
        assert_eq!(failed.status, "failed");
        assert!(failed.message.contains("示例错误"));

        // 列表倒序
        let id2 = store.insert_task(&new_task()).unwrap();
        let list = store.list_tasks(10).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, id2);
        assert!(list[0].id > list[1].id);
    }

    #[test]
    fn 插入任务默认状态queued() {
        let (_d, store) = open_temp();
        let mut t = new_task();
        t.status = "queued".into();
        let id = store.insert_task(&t).unwrap();
        assert_eq!(store.get_task(id).unwrap().unwrap().status, "queued");
    }

    #[test]
    fn 任务分页与总数() {
        let (_d, store) = open_temp();
        for _ in 0..5 {
            store.insert_task(&new_task()).unwrap();
        }
        assert_eq!(store.count_tasks().unwrap(), 5);
        // 第一页 2 条（id 倒序 5,4）
        let page1 = store.list_tasks_paged(2, 0).unwrap();
        assert_eq!(page1.len(), 2);
        assert_eq!(page1[0].id, 5);
        assert_eq!(page1[1].id, 4);
        // 第二页偏移 2 → 3,2
        let page2 = store.list_tasks_paged(2, 2).unwrap();
        assert_eq!(page2[0].id, 3);
        assert_eq!(page2[1].id, 2);
        // 偏移 4 → 只剩 1 条
        let page3 = store.list_tasks_paged(2, 4).unwrap();
        assert_eq!(page3.len(), 1);
        assert_eq!(page3[0].id, 1);
    }
}
