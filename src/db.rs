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
    pub parent_id: Option<i64>,
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
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
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
        )?;

        // Migration: add parent_id for task nesting
        let _ = self.conn.execute_batch(
            "ALTER TABLE tasks ADD COLUMN parent_id INTEGER REFERENCES tasks(id) ON DELETE CASCADE;",
        );
        self.conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_tasks_parent_id ON tasks(parent_id);",
        )?;

        Ok(())
    }

    pub fn is_empty(&self) -> rusqlite::Result<bool> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM tasks", [], |row| row.get(0))?;
        Ok(count == 0)
    }

    pub fn seed_sample_data(&self) -> rusqlite::Result<()> {
        // Dogfooding: use cli-todo to build cli-todo
        // Phase parents first, then nest children under them

        let phase1 = self.add_task(
            "Phase 1 — Task Management",
            Priority::High,
            &["phase1".into()],
            "Core task management features: CRUD, hierarchy, search.",
            None,
        )?;

        // Phase 1 — Done
        for (title, status, priority, tags, desc) in [
            ("TUI with task list and tab filtering", "done", "high", "ui",
             "Task table with All / Active / Blocked / Done tabs, counts per tab."),
            ("Detail panel", "done", "high", "ui",
             "Right-side panel showing status, priority, tags, description, linked sessions for selected task."),
            ("SQLite persistence", "done", "high", "core",
             "Store tasks in ~/.local/share/cli-todo/cli-todo.db via rusqlite with bundled SQLite."),
            ("Vim-style navigation + mouse support", "done", "medium", "ui",
             "j/k and arrow keys for navigation. Mouse click to select rows, click tabs, scroll wheel to navigate."),
            ("Task deletion with confirmation", "done", "medium", "ui",
             "Press x to delete, y to confirm. Popup with task title and cancel option."),
            ("Help popup", "done", "low", "ui",
             "Press ? to show keybinding reference overlay."),
        ] {
            self.insert_task(title, status, priority, tags, desc, Some(phase1))?;
        }

        // Phase 1 — Todo
        for (title, status, priority, tags, desc) in [
            ("Add tasks inline from TUI", "todo", "high", "ui",
             "Press a, type title, pick priority. Should feel as quick as typing a commit message. No modal wizard."),
            ("Edit task fields inline", "todo", "high", "ui",
             "Edit title, description, priority, tags on the selected task without leaving the TUI."),
            ("Cycle task status with keybinding", "todo", "high", "ui",
             "Press s to cycle: todo -> in_progress -> done. Maybe shift+s for blocked."),
            ("Projects as first-class concept", "todo", "high", "core",
             "Projects are logical groupings (an app, a system, a library) — not tied to directories. Add projects table, create/switch/list in TUI. Single DB, multiple projects."),
            ("Task hierarchy / subtasks", "in_progress", "medium", "core",
             "Tasks can have subtasks. Need parent_id in schema. UI should show nesting — tree view with expand/collapse."),
            ("Full-text search across tasks", "todo", "medium", "ui",
             "Press / to search. Filter task list by title and description matches."),
        ] {
            self.insert_task(title, status, priority, tags, desc, Some(phase1))?;
        }

        let phase2 = self.add_task(
            "Phase 2 — Developer Cockpit",
            Priority::High,
            &["phase2".into()],
            "Integrated terminal panes and split-pane layouts.",
            None,
        )?;

        for (title, status, priority, tags, desc) in [
            ("Integrated terminal panes", "todo", "high", "ui",
             "Embed shell sessions inside the TUI. Run commands, launch Claude Code, see output — all without leaving the app."),
            ("Split-pane layouts", "todo", "high", "ui",
             "Task board + terminal + artifact viewer side by side. Configurable splits like tmux."),
            ("Window management keybindings", "todo", "medium", "ui",
             "Split, resize, focus, close panes. Vim-style or tmux-style bindings."),
            ("Shared context across panes", "todo", "medium", "core",
             "Terminal panes know which task/project is active. Context flows between panes."),
        ] {
            self.insert_task(title, status, priority, tags, desc, Some(phase2))?;
        }

        let phase3 = self.add_task(
            "Phase 3 — Artifact System",
            Priority::Medium,
            &["phase3".into()],
            "Persistent artifacts linked to tasks, viewable in TUI.",
            None,
        )?;

        for (title, status, priority, tags, desc) in [
            ("Link markdown files to tasks", "todo", "medium", "artifacts",
             "Associate .md files on disk with tasks. Track in DB, surface in detail panel."),
            ("View/edit artifacts in TUI", "todo", "medium", "ui",
             "Read and edit markdown artifacts from within the app. Syntax highlighting."),
            ("Artifact creation flow", "todo", "medium", "artifacts",
             "Create new artifact from task context. Template with task title, description, etc."),
            ("Track artifact freshness", "todo", "low", "artifacts",
             "Compare artifact last-modified vs related code changes. Flag stale artifacts."),
        ] {
            self.insert_task(title, status, priority, tags, desc, Some(phase3))?;
        }

        let phase4 = self.add_task(
            "Phase 4 — Claude Integration via MCP",
            Priority::Medium,
            &["phase4".into()],
            "MCP server exposing tasks and artifacts to Claude Code sessions.",
            None,
        )?;

        for (title, status, priority, tags, desc) in [
            ("MCP server for task CRUD", "todo", "high", "claude",
             "Expose get_tasks, create_task, update_status, delete_task to Claude Code via MCP."),
            ("MCP server for artifact read/write", "todo", "medium", "claude",
             "Expose list_artifacts, read_artifact, write_artifact to Claude Code via MCP."),
            ("Launch Claude sessions scoped to a task", "todo", "high", "claude",
             "Start Claude Code pre-loaded with task context and relevant artifacts."),
            ("Claude can manage tasks from any session", "todo", "medium", "claude",
             "Any Claude Code session with the MCP server can create/update/query tasks."),
            ("Session history tracking per task", "todo", "low", "claude",
             "Record which Claude sessions were linked to each task. View history in detail panel."),
        ] {
            self.insert_task(title, status, priority, tags, desc, Some(phase4))?;
        }

        let phase5 = self.add_task(
            "Phase 5 — Context Engine",
            Priority::Low,
            &["phase5".into()],
            "RAG-based context retrieval and artifact-code sync.",
            None,
        )?;

        for (title, status, priority, tags, desc) in [
            ("Local vector store for artifacts", "todo", "low", "context",
             "Embed artifacts and task descriptions. Local model, no cloud dependency."),
            ("Intelligent context retrieval", "todo", "low", "context",
             "When launching a Claude session, RAG-retrieve only the relevant artifacts. Surgical context injection."),
            ("Artifact-code drift detection", "todo", "low", "context",
             "Detect when code has changed in ways that make artifacts stale. Flag for review."),
            ("Bidirectional sync (code <-> artifacts)", "todo", "low", "context",
             "Update artifact -> Claude implements code changes. Update code -> Claude updates artifacts."),
        ] {
            self.insert_task(title, status, priority, tags, desc, Some(phase5))?;
        }

        Ok(())
    }

    /// Internal helper for seed data — inserts with explicit status and parent_id.
    fn insert_task(
        &self,
        title: &str,
        status: &str,
        priority: &str,
        tags: &str,
        description: &str,
        parent_id: Option<i64>,
    ) -> rusqlite::Result<i64> {
        self.conn.execute(
            "INSERT INTO tasks (title, status, priority, tags, description, parent_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![title, status, priority, tags, description, parent_id],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn all_tasks(&self) -> rusqlite::Result<Vec<Task>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, parent_id, title, status, priority, tags, description FROM tasks ORDER BY parent_id NULLS FIRST, id",
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
        parent_id: Option<i64>,
    ) -> rusqlite::Result<i64> {
        let tags_str = tags.join(",");
        self.conn.execute(
            "INSERT INTO tasks (title, priority, tags, description, parent_id) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![title, priority.as_str(), tags_str, description, parent_id],
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
        // CASCADE handles children and sessions automatically
        self.conn
            .execute("DELETE FROM tasks WHERE id = ?1", params![task_id])?;
        Ok(())
    }

    pub fn reparent_task(&self, task_id: i64, new_parent_id: Option<i64>) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE tasks SET parent_id = ?1, updated_at = datetime('now') WHERE id = ?2",
            params![new_parent_id, task_id],
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
