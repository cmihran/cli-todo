# cli-todo

Terminal-native developer control plane built with Rust + Ratatui.

## Commands

```bash
cargo build          # Compile
cargo run            # Run TUI (creates DB on first run)
cargo run -- mcp     # Run MCP server (JSON-RPC over stdio)
cargo run -- serve   # Run web UI (default http://localhost:3000, override with PORT env var)
cargo watch -x run   # Auto-reload on code changes (requires cargo-watch)
```

## Architecture

Four-file app with Elm-like architecture:

- `src/main.rs` — TUI app: state (`App` struct), input handling (`handle_key`/`handle_mouse`), rendering (`ui()`)
- `src/db.rs` — SQLite layer: schema, migrations, CRUD, tree queries (recursive CTEs)
- `src/mcp.rs` — MCP server: JSON-RPC over stdio, exposes task CRUD to Claude Code
- `src/web.rs` — Web UI: Axum server, JSON API, rust-embed static files (`static/` dir baked into binary)

## Key Patterns

- **DisplayRow enum**: Abstracts tree/grouped views — `Header(String)` for section headers, `Task { idx, depth }` for task rows
- **Tree rendering**: `children_map()` → `tree_walk()` (depth-first) → `tree_prefix()` (Unicode box-drawing ├─/└─/│)
- **Expand/collapse**: `HashSet<i64>` of collapsed task IDs, checked during tree_walk
- **Borrow checker pattern**: Extract IDs from `selected_task_view()` before mutating state (e.g., `.map(|tv| tv.task.id)`)

## Database

- SQLite at `~/.local/share/cli-todo/cli-todo.db` (override with `CLI_TODO_DB_DIR` env var)
- `rusqlite` with `bundled` feature — zero runtime dependencies
- `PRAGMA foreign_keys = ON` — CASCADE deletes handle children/sessions
- Migrations are idempotent (CREATE IF NOT EXISTS, ALTER TABLE ignores duplicate column errors)
- Delete DB to reset: `rm ~/.local/share/cli-todo/cli-todo.db`

### Schema

```
tasks:
  id          INTEGER PRIMARY KEY AUTOINCREMENT
  parent_id   INTEGER REFERENCES tasks(id) ON DELETE CASCADE  -- nullable, enables nesting
  title       TEXT NOT NULL
  status      TEXT NOT NULL DEFAULT 'todo'        -- todo | in_progress | done | blocked
  priority    TEXT NOT NULL DEFAULT 'medium'      -- low | medium | high | critical
  tags        TEXT NOT NULL DEFAULT ''             -- comma-separated
  description TEXT NOT NULL DEFAULT ''
  created_at  TEXT NOT NULL DEFAULT datetime('now')
  updated_at  TEXT NOT NULL DEFAULT datetime('now')

sessions:
  id          INTEGER PRIMARY KEY AUTOINCREMENT
  task_id     INTEGER NOT NULL REFERENCES tasks(id) ON DELETE CASCADE
  session_id  TEXT NOT NULL
  created_at  TEXT NOT NULL DEFAULT datetime('now')
```

## MCP Server

Run `cli-todo mcp` (or `cargo run -- mcp`) to start a JSON-RPC/MCP server on stdin/stdout. Claude Code uses this to read/write tasks in the shared SQLite DB.

### MCP Tools

- `list_tasks` — all tasks, optional `status` and `parent_id` filters
- `get_task` — single task by ID with descendant/session counts
- `add_task` — create task (title required, optional priority/tags/description/parent_id)
- `update_task` — update any fields (title/status/priority/tags/description/parent_id)
- `delete_task` — delete by ID (cascades to children)

## Gotchas

- Rust 2024 edition — requires rustc 1.85+
- Tree view only shows in GroupBy::None mode; grouped modes flatten to depth 0
- When filters are active (non-All tab), tasks flatten to depth 0 to avoid orphaned children

## Tech stack

- **Language:** Rust
- **TUI:** Ratatui + Crossterm
- **Storage:** SQLite via rusqlite (bundled)
- **MCP server:** Built-in (`cargo run -- mcp`), JSON-RPC over stdio
- **Web UI:** Axum + rust-embed (vanilla JS frontend, no build step)
- **Embeddings:** TBD
