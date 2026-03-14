# cli-todo — vision

## What is this?

A terminal-native developer control plane. Not just a task tracker — a full-screen workspace that replaces VS Code + tmux, acting as the single interface between your brain, your code, and your Claude Code sessions.

The core insight: **conversations are ephemeral, but artifacts are permanent.** Claude sessions come and go, context windows fill up and reset. But well-maintained artifacts — specs, design docs, task descriptions — survive across sessions and give any new agent instant context. This tool is the system that creates, maintains, and serves those artifacts.

---

## The problems we're solving

1. **Context fragmentation.** You spin up Claude sessions throughout the day. Each one does meaningful work, but there's no connective tissue between them. Knowledge lives in conversations that are hard to find and impossible to search.

2. **Context anxiety.** Constantly worrying about hitting context window limits. Wondering which chat has which information. Losing work when a session gets too long.

3. **Artifact-code drift.** You write a great design doc with Claude, then change the code, and now the doc is wrong. New agents read the stale doc and get confused. The reverse too — you update a spec but the code doesn't match.

4. **Tool fragmentation.** Browser for Jira, terminal for code, separate tmux panes with no shared state. Nothing talks to anything else.

---

## The vision

A full-screen terminal application that you open, maximize, and live in. Think of it as an IDE for your development workflow:

- **Left pane**: your task board — projects, tasks, subtasks, status, priority
- **Right pane**: detail view — task description, linked artifacts, session history
- **Integrated terminal panes**: run Claude Code sessions that are aware of your task context
- **Artifact management**: create, edit, and view persistent documents that stay in sync with code
- **Project switching**: like an IDE switching folders — one DB, multiple projects, each scoped to a directory

Claude sessions launched from this tool know about your tasks, your artifacts, and your project state via MCP. When Claude changes code, artifacts can update. When you update an artifact, Claude can implement the changes.

---

## What it should feel like

- **Fast.** Instant startup, compiled binary. Not a web app.
- **Keyboard-first, mouse-friendly.** Vim-style navigation, but clicking and scrolling work too.
- **Minimal friction.** Adding a task is as quick as typing a commit message.
- **Portable.** Clone the repo, `cargo build`, copy the binary. No Docker, no cloud accounts, no API keys.
- **Local-first.** SQLite on disk. Separate DB per machine. Your data is yours.
- **Full-screen.** This is your workspace, not a sidebar widget.

---

## Architecture

### Data model

```
Global DB (~/.local/share/cli-todo/cli-todo.db)
  └── Projects (logical grouping — an app, a system, a library)
       └── Tasks
            ├── Subtasks
            ├── Linked artifacts (markdown files on disk)
            └── Linked Claude sessions
```

Projects are logical, not filesystem-bound. A monorepo might contain 10 apps — each is its own project in the tool, even though they share a directory. Projects can optionally reference paths, but don't have to. Think of them like namespaces for your work, not folder shortcuts.

### Artifact system

**Phase 1 — Markdown files on disk.**
Artifacts are `.md` files stored alongside project code (e.g. `docs/design.md`, `.artifacts/auth-flow.md`). Claude reads these natively. The tool tracks which artifacts belong to which tasks and surfaces them in the detail panel.

**Phase 2 — MCP server.**
An MCP server exposes the task DB and artifact index to any Claude Code session. Claude gets tools like `get_tasks`, `create_task`, `update_status`, `list_artifacts`, `read_artifact`. Any Claude session launched from the tool — or even standalone — can interact with your task board.

**Phase 3 — Context engine (RAG).**
Embed artifacts and task descriptions into a local vector store. When starting a Claude session for a task, intelligently retrieve and inject only the relevant context — not everything, just what matters for that specific work. Solve the context window problem by being surgical about what goes in.

### Bidirectional sync

The hardest and most valuable problem:

- **Code → Artifacts**: When Claude changes code, detect affected artifacts and flag them as potentially stale. Optionally auto-update them.
- **Artifacts → Code**: When you update a spec or design doc, the tool can prompt Claude to implement the changes to match.

This is the thing that makes artifacts trustworthy. Without sync, artifacts rot. With sync, they're a reliable source of truth.

---

## Decisions made

| Question | Decision |
|----------|----------|
| Scope | Single global DB, projects are logical groupings (not tied to directories) |
| Multi-machine | Separate DB per machine, no sync |
| Primary interface | TUI (CLI commands may come later) |
| Task granularity | Hierarchical — projects → tasks → subtasks |
| Artifact storage | Markdown files on disk, indexed in DB |
| Claude integration | MCP server (Claude gets tools to interact with the system) |
| Input style | Keyboard-first, mouse-friendly |

---

## Non-goals

- Team collaboration (this is single-player)
- Sprint planning / story points / Gantt charts
- Note-taking app (artifacts are project docs, not a knowledge base)
- Replacing git
- Cloud sync / SaaS
