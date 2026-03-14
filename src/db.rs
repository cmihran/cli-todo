use rusqlite::{Connection, params};
use serde::{Serialize, Deserialize};
use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Todo,
    InProgress,
    Done,
    Blocked,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Status::Todo => "TODO",
            Status::InProgress => "IN PROGRESS",
            Status::Done => "DONE",
            Status::Blocked => "BLOCKED",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Status::Todo => "todo",
            Status::InProgress => "in_progress",
            Status::Done => "done",
            Status::Blocked => "blocked",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Status::Todo => Status::InProgress,
            Status::InProgress => Status::Done,
            Status::Done => Status::Todo,
            Status::Blocked => Status::Todo,
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "in_progress" => Status::InProgress,
            "done" => Status::Done,
            "blocked" => Status::Blocked,
            _ => Status::Todo,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    Low,
    Medium,
    High,
    Critical,
}

impl Priority {
    pub fn label(self) -> &'static str {
        match self {
            Priority::Low => "low",
            Priority::Medium => "med",
            Priority::High => "high",
            Priority::Critical => "crit",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Priority::Low => "low",
            Priority::Medium => "medium",
            Priority::High => "high",
            Priority::Critical => "critical",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "medium" => Priority::Medium,
            "high" => Priority::High,
            "critical" => Priority::Critical,
            _ => Priority::Low,
        }
    }
}

#[derive(Clone)]
pub struct Session {
    pub session_id: String,
    pub created_at: String,
}

#[derive(Clone, Serialize)]
pub struct Task {
    pub id: i64,
    pub parent_id: Option<i64>,
    pub title: String,
    pub status: Status,
    pub priority: Priority,
    pub tags: Vec<String>,
    pub description: String,
}

pub struct Db {
    conn: Connection,
    pub path: PathBuf,
}

impl Db {
    pub fn open() -> rusqlite::Result<Self> {
        let path = db_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let conn = Connection::open(&path)?;
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        let db = Db { conn, path: path.clone() };
        db.migrate()?;
        Ok(db)
    }

    fn migrate(&self) -> rusqlite::Result<()> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS tasks (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                title       TEXT    NOT NULL,
                status      TEXT    NOT NULL DEFAULT 'todo',
                priority    TEXT    NOT NULL DEFAULT 'medium',
                tags        TEXT    NOT NULL DEFAULT '',
                description TEXT    NOT NULL DEFAULT '',
                created_at  TEXT    NOT NULL DEFAULT (datetime('now')),
                updated_at  TEXT    NOT NULL DEFAULT (datetime('now'))
            );

            CREATE TABLE IF NOT EXISTS sessions (
                id         INTEGER PRIMARY KEY AUTOINCREMENT,
                task_id    INTEGER NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
                session_id TEXT    NOT NULL,
                created_at TEXT    NOT NULL DEFAULT (datetime('now'))
            );

            CREATE INDEX IF NOT EXISTS idx_sessions_task_id ON sessions(task_id);",
        )?;

        // Migration: add parent_id for task nesting
        let _ = self.conn.execute_batch(
            "ALTER TABLE tasks ADD COLUMN parent_id INTEGER REFERENCES tasks(id) ON DELETE CASCADE;",
        );
        self.conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_tasks_parent_id ON tasks(parent_id);",
        )?;

        // Migration: add position for manual ordering
        let _ = self.conn.execute_batch(
            "ALTER TABLE tasks ADD COLUMN position INTEGER NOT NULL DEFAULT 0;",
        );
        // Backfill: assign positions based on current id order within each sibling group
        self.conn.execute_batch(
            "UPDATE tasks SET position = (
                SELECT COUNT(*) FROM tasks t2
                WHERE t2.parent_id IS tasks.parent_id AND t2.id < tasks.id
            ) WHERE position = 0;",
        )?;

        Ok(())
    }

    pub fn get_task(&self, task_id: i64) -> rusqlite::Result<Option<Task>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, parent_id, title, status, priority, tags, description FROM tasks WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map(params![task_id], |row| {
            let tags_str: String = row.get(5)?;
            let tags: Vec<String> = if tags_str.is_empty() {
                vec![]
            } else {
                tags_str.split(',').map(|s| s.trim().to_string()).collect()
            };
            Ok(Task {
                id: row.get(0)?,
                parent_id: row.get(1)?,
                title: row.get(2)?,
                status: Status::from_str(&row.get::<_, String>(3)?),
                priority: Priority::from_str(&row.get::<_, String>(4)?),
                tags,
                description: row.get(6)?,
            })
        })?;
        match rows.next() {
            Some(Ok(task)) => Ok(Some(task)),
            Some(Err(e)) => Err(e),
            None => Ok(None),
        }
    }

    pub fn all_tasks(&self) -> rusqlite::Result<Vec<Task>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, parent_id, title, status, priority, tags, description FROM tasks ORDER BY parent_id NULLS FIRST, position, id",
        )?;
        let tasks = stmt
            .query_map([], |row| {
                let tags_str: String = row.get(5)?;
                let tags: Vec<String> = if tags_str.is_empty() {
                    vec![]
                } else {
                    tags_str.split(',').map(|s| s.trim().to_string()).collect()
                };
                Ok(Task {
                    id: row.get(0)?,
                    parent_id: row.get(1)?,
                    title: row.get(2)?,
                    status: Status::from_str(&row.get::<_, String>(3)?),
                    priority: Priority::from_str(&row.get::<_, String>(4)?),
                    tags,
                    description: row.get(6)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(tasks)
    }

    pub fn session_count(&self, task_id: i64) -> rusqlite::Result<usize> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM sessions WHERE task_id = ?1",
            params![task_id],
            |row| row.get(0),
        )?;
        Ok(count as usize)
    }

    pub fn sessions_for_task(&self, task_id: i64) -> rusqlite::Result<Vec<Session>> {
        let mut stmt = self.conn.prepare(
            "SELECT session_id, created_at FROM sessions WHERE task_id = ?1 ORDER BY created_at",
        )?;
        let sessions = stmt
            .query_map(params![task_id], |row| {
                Ok(Session {
                    session_id: row.get(0)?,
                    created_at: row.get(1)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<Session>>>()?;
        Ok(sessions)
    }

    pub fn delete_session(&self, session_id: &str) -> rusqlite::Result<()> {
        self.conn.execute(
            "DELETE FROM sessions WHERE session_id = ?1",
            params![session_id],
        )?;
        Ok(())
    }

    pub fn add_task(
        &self,
        title: &str,
        priority: Priority,
        tags: &[String],
        description: &str,
        parent_id: Option<i64>,
    ) -> rusqlite::Result<i64> {
        let tags_str = tags.join(",");
        let next_pos: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(position), -1) + 1 FROM tasks WHERE parent_id IS ?1",
            params![parent_id],
            |row| row.get(0),
        )?;
        self.conn.execute(
            "INSERT INTO tasks (title, priority, tags, description, parent_id, position) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![title, priority.as_str(), tags_str, description, parent_id, next_pos],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn update_status(&self, task_id: i64, status: Status) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE tasks SET status = ?1, updated_at = datetime('now') WHERE id = ?2",
            params![status.as_str(), task_id],
        )?;
        Ok(())
    }

    pub fn update_task(
        &self,
        task_id: i64,
        title: Option<&str>,
        status: Option<Status>,
        priority: Option<Priority>,
        tags: Option<&[String]>,
        description: Option<&str>,
    ) -> rusqlite::Result<bool> {
        let current = match self.get_task(task_id)? {
            Some(t) => t,
            None => return Ok(false),
        };
        let title = title.unwrap_or(&current.title);
        let status = status.unwrap_or(current.status);
        let priority = priority.unwrap_or(current.priority);
        let tags_str = match tags {
            Some(t) => t.join(","),
            None => current.tags.join(","),
        };
        let description = description.unwrap_or(&current.description);
        self.conn.execute(
            "UPDATE tasks SET title=?1, status=?2, priority=?3, tags=?4, description=?5, updated_at=datetime('now') WHERE id=?6",
            params![title, status.as_str(), priority.as_str(), tags_str, description, task_id],
        )?;
        Ok(true)
    }

    pub fn delete_task(&self, task_id: i64) -> rusqlite::Result<()> {
        // CASCADE handles children and sessions automatically
        self.conn
            .execute("DELETE FROM tasks WHERE id = ?1", params![task_id])?;
        Ok(())
    }

    pub fn add_session(&self, task_id: i64, session_id: &str) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT INTO sessions (task_id, session_id) VALUES (?1, ?2)",
            params![task_id, session_id],
        )?;
        Ok(())
    }

    /// Swap the position of two sibling tasks.
    pub fn swap_task_order(&self, task_a: i64, task_b: i64) -> rusqlite::Result<()> {
        let pos_a: i64 = self.conn.query_row(
            "SELECT position FROM tasks WHERE id = ?1",
            params![task_a],
            |row| row.get(0),
        )?;
        let pos_b: i64 = self.conn.query_row(
            "SELECT position FROM tasks WHERE id = ?1",
            params![task_b],
            |row| row.get(0),
        )?;
        self.conn.execute(
            "UPDATE tasks SET position = ?1, updated_at = datetime('now') WHERE id = ?2",
            params![pos_b, task_a],
        )?;
        self.conn.execute(
            "UPDATE tasks SET position = ?1, updated_at = datetime('now') WHERE id = ?2",
            params![pos_a, task_b],
        )?;
        Ok(())
    }

    pub fn reparent_task(&self, task_id: i64, new_parent_id: Option<i64>) -> rusqlite::Result<()> {
        let next_pos: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(position), -1) + 1 FROM tasks WHERE parent_id IS ?1",
            params![new_parent_id],
            |row| row.get(0),
        )?;
        self.conn.execute(
            "UPDATE tasks SET parent_id = ?1, position = ?2, updated_at = datetime('now') WHERE id = ?3",
            params![new_parent_id, next_pos, task_id],
        )?;
        Ok(())
    }

    pub fn descendant_count(&self, task_id: i64) -> rusqlite::Result<usize> {
        let count: i64 = self.conn.query_row(
            "WITH RECURSIVE descendants AS (
                SELECT id FROM tasks WHERE parent_id = ?1
                UNION ALL
                SELECT t.id FROM tasks t JOIN descendants d ON t.parent_id = d.id
            )
            SELECT COUNT(*) FROM descendants",
            params![task_id],
            |row| row.get(0),
        )?;
        Ok(count as usize)
    }
}

fn db_path() -> PathBuf {
    if let Ok(dir) = std::env::var("CLI_TODO_DB_DIR") {
        return PathBuf::from(dir).join("cli-todo.db");
    }
    let data_dir = dirs_or_default();
    data_dir.join("cli-todo.db")
}

fn dirs_or_default() -> PathBuf {
    if let Some(data) = std::env::var_os("XDG_DATA_HOME") {
        return PathBuf::from(data).join("cli-todo");
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".local/share/cli-todo");
    }
    PathBuf::from(".cli-todo")
}
