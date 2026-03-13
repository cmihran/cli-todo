use rusqlite::{Connection, params};
use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq, Debug)]
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

    pub fn from_str(s: &str) -> Self {
        match s {
            "in_progress" => Status::InProgress,
            "done" => Status::Done,
            "blocked" => Status::Blocked,
            _ => Status::Todo,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
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
pub struct Task {
    pub id: i64,
    pub title: String,
    pub status: Status,
    pub priority: Priority,
    pub tags: Vec<String>,
    pub description: String,
}

pub struct Db {
    conn: Connection,
}

impl Db {
    pub fn open() -> rusqlite::Result<Self> {
        let path = db_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let conn = Connection::open(&path)?;
        let db = Db { conn };
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
        )
    }

    pub fn is_empty(&self) -> rusqlite::Result<bool> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM tasks", [], |row| row.get(0))?;
        Ok(count == 0)
    }

    pub fn seed_sample_data(&self) -> rusqlite::Result<()> {
        let tasks = vec![
            ("Set up Rust project with ratatui", "done", "high", "setup,infra",
             "Initialize cargo project, add ratatui + crossterm deps, get basic TUI rendering."),
            ("Design task data model + SQLite schema", "in_progress", "high", "core,database",
             "Define Task, Tag, Session tables. Use rusqlite. Need migrations strategy."),
            ("Implement Claude Code session tracking", "todo", "critical", "core,claude",
             "Track which Claude Code sessions are associated with each task. Parse session IDs from ~/.claude/projects/. Allow linking sessions to tasks via CLI command."),
            ("Add keyboard-driven task creation flow", "todo", "medium", "ui",
             "Inline task creation with title, priority picker, tag input. Should feel snappy — no modal dialogs."),
            ("Build CLI subcommands (add, list, start)", "todo", "medium", "cli",
             "Support both TUI mode and direct CLI commands: `todo add 'fix parser'`, `todo list --status blocked`, `todo start 3` (launches Claude with task context)."),
            ("Fix rendering glitch on terminal resize", "blocked", "low", "bug,ui",
             "Table columns don't reflow properly when terminal is resized below 80 cols. Need to handle resize events and set minimum widths."),
            ("Export tasks to markdown", "todo", "low", "feature",
             "Generate a markdown summary of all tasks, grouped by status. Useful for pasting into PRs or docs."),
            ("Session replay / summary view", "todo", "medium", "feature,claude",
             "Show a summary of what happened in each linked Claude Code session — files changed, commands run, key decisions. Pull from session JSON logs."),
        ];

        for (title, status, priority, tags, desc) in tasks {
            self.conn.execute(
                "INSERT INTO tasks (title, status, priority, tags, description) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![title, status, priority, tags, desc],
            )?;
        }

        // Add some sample sessions
        self.conn.execute(
            "INSERT INTO sessions (task_id, session_id) VALUES (1, 'ses_01JA3K...')",
            [],
        )?;
        self.conn.execute(
            "INSERT INTO sessions (task_id, session_id) VALUES (2, 'ses_01JA4M...')",
            [],
        )?;
        self.conn.execute(
            "INSERT INTO sessions (task_id, session_id) VALUES (2, 'ses_01JA5N...')",
            [],
        )?;
        self.conn.execute(
            "INSERT INTO sessions (task_id, session_id) VALUES (6, 'ses_01JA6P...')",
            [],
        )?;

        Ok(())
    }

    pub fn all_tasks(&self) -> rusqlite::Result<Vec<Task>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, title, status, priority, tags, description FROM tasks ORDER BY id",
        )?;
        let tasks = stmt
            .query_map([], |row| {
                let tags_str: String = row.get(4)?;
                let tags: Vec<String> = if tags_str.is_empty() {
                    vec![]
                } else {
                    tags_str.split(',').map(|s| s.trim().to_string()).collect()
                };
                Ok(Task {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    status: Status::from_str(&row.get::<_, String>(2)?),
                    priority: Priority::from_str(&row.get::<_, String>(3)?),
                    tags,
                    description: row.get(5)?,
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

    pub fn sessions_for_task(&self, task_id: i64) -> rusqlite::Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT session_id FROM sessions WHERE task_id = ?1 ORDER BY created_at")?;
        let sessions = stmt
            .query_map(params![task_id], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        Ok(sessions)
    }

    pub fn add_task(
        &self,
        title: &str,
        priority: Priority,
        tags: &[String],
        description: &str,
    ) -> rusqlite::Result<i64> {
        let tags_str = tags.join(",");
        self.conn.execute(
            "INSERT INTO tasks (title, priority, tags, description) VALUES (?1, ?2, ?3, ?4)",
            params![title, priority.as_str(), tags_str, description],
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

    pub fn delete_task(&self, task_id: i64) -> rusqlite::Result<()> {
        self.conn
            .execute("DELETE FROM sessions WHERE task_id = ?1", params![task_id])?;
        self.conn
            .execute("DELETE FROM tasks WHERE id = ?1", params![task_id])?;
        Ok(())
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
