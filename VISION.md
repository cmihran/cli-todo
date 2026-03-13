# cli-todo — vision

A terminal-native task tracker built for developers who use Claude Code as their daily driver.

---

## The problem

You're working across multiple projects, spinning up Claude Code sessions throughout the day. Each session does meaningful work — but there's no connective tissue between them. You lose track of:
- What you asked Claude to do and why
- Which sessions relate to which goals
- What's blocked, what's done, what's next
- The bigger picture across days and weeks

Existing tools (Jira, Linear, Notion) are browser-based, heavyweight, and disconnected from your terminal workflow.

---

## Core idea

A CLI tool that lives where you work — the terminal — and acts as the bridge between your task planning and your Claude Code sessions.

---

## What it should feel like

- **Fast.** Instant startup, no loading spinners. A compiled binary, not a web app.
- **Keyboard-first.** Vim-style navigation. No mouse needed, ever.
- **Minimal friction.** Adding a task should be as quick as typing a commit message.
- **Portable.** Clone the repo, `cargo build`, done. No Docker, no cloud accounts, no API keys.
- **Local-first.** SQLite file on disk. Your data is yours.

---

## Features — what exists today

- [x] TUI with task list, tab filtering (All / Active / Blocked / Done)
- [x] Detail panel with status, priority, tags, description, linked sessions
- [x] SQLite persistence (~/.local/share/cli-todo/cli-todo.db)
- [x] Vim-style navigation (j/k), help popup
- [x] Sample data seeding on first run

---

## Features — next up

### Task management (the basics)
- [ ] Add tasks inline from TUI (press `a`, type title, pick priority)
- [ ] Edit task title/description/priority/tags inline
- [ ] Cycle task status with a keybinding (e.g. `s` to cycle todo → in_progress → done)
- [ ] Delete/archive tasks
- [ ] Reorder tasks (manual priority ordering within a status group?)

### Claude Code integration
- [ ] Link a Claude Code session to a task (`todo link <task_id> <session_id>`)
- [ ] Launch a new Claude Code session scoped to a task (`todo start <task_id>`)
  - Pre-populates Claude with task context (title, description, related files?)
- [ ] View session history per task — what files were touched, what happened
- [ ] Auto-detect current session and suggest task linking

### CLI mode (non-TUI)
- [ ] `todo add "fix the parser" --priority high --tags core,parser`
- [ ] `todo list` / `todo list --status blocked`
- [ ] `todo done <id>` / `todo block <id>`
- [ ] Pipe-friendly output for scripting

### Search and filtering
- [ ] Full-text search across task titles and descriptions
- [ ] Filter by tag
- [ ] Filter by date range (created/updated)

---

## Features — someday/maybe

- [ ] Project scoping (different task databases per project directory?)
- [ ] Time tracking (how long was a task in "in_progress"?)
- [ ] Task dependencies / subtasks
- [ ] Markdown export for standup notes or PR descriptions
- [ ] Git integration (link tasks to branches/commits)
- [ ] Sync across machines (git-backed SQLite? CRDTs? Or just keep it simple and use one machine?)
- [ ] Notifications/reminders for blocked tasks
- [ ] Dashboard view — summary stats, burndown-style visualization

---

## Open questions

Things to think about before building further:

1. **Scope per project vs global?**
   One database for everything, or a .cli-todo.db per project directory? Global is simpler. Per-project is more contained. Could support both?

2. **How deep should Claude Code integration go?**
   Minimal: just store session IDs as metadata on tasks.
   Medium: parse session logs to show summaries of what happened.
   Deep: auto-create tasks from Claude conversations, two-way sync.

3. **Multi-machine story?**
   You mentioned wanting to clone and run this on your work PC. The binary is portable, but what about the task data? Options:
   - Just accept separate databases per machine
   - Store the DB in a git repo or synced folder
   - Build actual sync (ambitious)

4. **CLI vs TUI — primary interface?**
   Is the TUI the main way to use this, with CLI as a convenience? Or should CLI be first-class for scripting/automation?

5. **Task granularity?**
   Are these high-level goals ("implement auth system") or small todos ("fix null check in parser.rs")? Both? Does the UI need to support hierarchy?

6. **What makes this better than a TODO.md file?**
   The answer should be: Claude Code session tracking, status management, and the TUI. If those aren't compelling, keep it simpler.

---

## Non-goals

Things this tool should NOT try to be:

- A team collaboration tool (this is single-player)
- A project management platform (no sprints, no story points, no Gantt charts)
- A note-taking app (use Obsidian/Notion for that)
- A replacement for git (no branching, no version control of tasks)

---

## Tech stack

- **Language:** Rust
- **TUI:** Ratatui + Crossterm
- **Storage:** SQLite via rusqlite (bundled, no system deps)
- **CLI parsing:** TBD (clap? Just args?)
