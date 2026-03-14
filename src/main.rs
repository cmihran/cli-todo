mod db;
mod mcp;
mod pty;
mod web;

use crate::db::{Db, Priority, Session, Status, Task};
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind,
        MouseEventKind,
    },
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use ratatui::{
    layout::{Constraint, Direction, Layout, Margin, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{
        Block, BorderType, Borders, Cell, Clear, Padding, Paragraph, Row, Scrollbar,
        ScrollbarOrientation, ScrollbarState, Table, TableState, Tabs, Wrap,
    },
    Frame, Terminal,
};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::{self, stdout};
use unicode_width::UnicodeWidthStr;

// ── UI helpers for Status/Priority colors ───────────────────────────────────

fn status_color(s: Status) -> Color {
    match s {
        Status::Todo => Color::DarkGray,
        Status::InProgress => Color::Yellow,
        Status::InReview => Color::Magenta,
        Status::Done => Color::Green,
        Status::Blocked => Color::Red,
    }
}

const IN_PROGRESS_FRAMES: &[&str] = &["◐", "◓", "◑", "◒"];

fn status_icon(s: Status, tick: usize) -> &'static str {
    match s {
        Status::Todo => "○",
        Status::InProgress => IN_PROGRESS_FRAMES[tick % IN_PROGRESS_FRAMES.len()],
        Status::InReview => "◑",
        Status::Done => "●",
        Status::Blocked => "✕",
    }
}

fn priority_color(p: Priority) -> Color {
    match p {
        Priority::Low => Color::DarkGray,
        Priority::Medium => Color::Blue,
        Priority::High => Color::Yellow,
        Priority::Critical => Color::Red,
    }
}

// ── Active session detection ─────────────────────────────────────────────────

/// Scan /proc for running `claude` processes and return their session IDs.
fn detect_active_session_ids() -> HashSet<String> {
    let mut active = HashSet::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return active;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if !name.to_string_lossy().chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let cmdline_path = entry.path().join("cmdline");
        let Ok(data) = std::fs::read(&cmdline_path) else {
            continue;
        };
        let args: Vec<String> = data
            .split(|&b| b == 0)
            .filter(|s| !s.is_empty())
            .map(|s| String::from_utf8_lossy(s).to_string())
            .collect();
        if args.is_empty() {
            continue;
        }
        // Check if any of the first few args indicate a claude process
        let has_claude = args.iter().take(3).any(|a| {
            a.rsplit('/').next().unwrap_or(a) == "claude"
        });
        if !has_claude {
            continue;
        }
        // Extract session IDs from --session-id or --resume flags
        let mut i = 0;
        while i < args.len() - 1 {
            if args[i] == "--session-id" || args[i] == "--resume" {
                active.insert(args[i + 1].clone());
                i += 2;
            } else {
                i += 1;
            }
        }
    }
    active
}

// ── App types ───────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq)]
enum GroupBy {
    None,
    Status,
    Priority,
    Tag,
}

impl GroupBy {
    fn label(self) -> &'static str {
        match self {
            GroupBy::None => "none",
            GroupBy::Status => "status",
            GroupBy::Priority => "priority",
            GroupBy::Tag => "tag",
        }
    }

    fn next(self) -> Self {
        match self {
            GroupBy::None => GroupBy::Status,
            GroupBy::Status => GroupBy::Priority,
            GroupBy::Priority => GroupBy::Tag,
            GroupBy::Tag => GroupBy::None,
        }
    }

    fn from_str(s: &str) -> Self {
        match s {
            "status" => GroupBy::Status,
            "priority" => GroupBy::Priority,
            "tag" => GroupBy::Tag,
            _ => GroupBy::None,
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum SortBy {
    Manual,
    Status,
    Alphabetical,
    LastModified,
}

impl SortBy {
    fn label(self) -> &'static str {
        match self {
            SortBy::Manual => "manual",
            SortBy::Status => "status",
            SortBy::Alphabetical => "A-Z",
            SortBy::LastModified => "modified",
        }
    }

    fn next(self) -> Self {
        match self {
            SortBy::Manual => SortBy::Status,
            SortBy::Status => SortBy::Alphabetical,
            SortBy::Alphabetical => SortBy::LastModified,
            SortBy::LastModified => SortBy::Manual,
        }
    }

    fn from_str(s: &str) -> Self {
        match s {
            "status" => SortBy::Status,
            "alphabetical" => SortBy::Alphabetical,
            _ => SortBy::Manual,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            SortBy::Manual => "manual",
            SortBy::Status => "status",
            SortBy::Alphabetical => "alphabetical",
        }
    }
}

/// A row in the display list — either a group header or a task reference.
enum DisplayRow {
    Header(String),
    Task { idx: usize, depth: usize, dimmed: bool },
}

#[derive(Clone, Copy, PartialEq)]
enum EditField {
    Title,
    Priority,
    Tags,
    Description,
}

impl EditField {
    fn label(self) -> &'static str {
        match self {
            EditField::Title => "title",
            EditField::Priority => "priority",
            EditField::Tags => "tags",
            EditField::Description => "description",
        }
    }

    fn all() -> [EditField; 4] {
        [EditField::Title, EditField::Priority, EditField::Tags, EditField::Description]
    }
}

#[derive(Clone, Copy, PartialEq)]
enum ActiveTab {
    All,
    Active,
    InReview,
    Blocked,
    Done,
}

impl ActiveTab {
    fn index(self) -> usize {
        match self {
            ActiveTab::All => 0,
            ActiveTab::Active => 1,
            ActiveTab::InReview => 2,
            ActiveTab::Blocked => 3,
            ActiveTab::Done => 4,
        }
    }

    fn filter(self, status: Status) -> bool {
        match self {
            ActiveTab::All => true,
            ActiveTab::Active => status == Status::InProgress || status == Status::Todo,
            ActiveTab::InReview => status == Status::InReview,
            ActiveTab::Blocked => status == Status::Blocked,
            ActiveTab::Done => status == Status::Done,
        }
    }

    fn from_str(s: &str) -> Self {
        match s {
            "active" => ActiveTab::Active,
            "in_review" => ActiveTab::InReview,
            "blocked" => ActiveTab::Blocked,
            "done" => ActiveTab::Done,
            _ => ActiveTab::All,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            ActiveTab::All => "all",
            ActiveTab::Active => "active",
            ActiveTab::InReview => "in_review",
            ActiveTab::Blocked => "blocked",
            ActiveTab::Done => "done",
        }
    }
}

struct TaskView {
    task: Task,
    session_count: usize,
    sessions: Vec<Session>,
}

struct App {
    db: Db,
    tasks: Vec<TaskView>,
    table_state: TableState,
    active_tab: ActiveTab,
    group_by: GroupBy,
    sort_by: SortBy,
    tag_filter: Option<String>,
    show_tag_picker: bool,
    tag_picker_state: TableState,
    show_detail: bool,
    show_help: bool,
    confirm_delete: bool,
    confirm_quit: bool,
    collapsed: HashSet<i64>,
    quit: bool,
    // Inline task creation
    input_mode: bool,
    input_buffer: String,
    input_cursor: usize,
    input_parent_id: Option<i64>,
    // Layout areas for mouse hit-testing
    table_area: Rect,
    tab_area: Rect,
    // Inline editing
    edit_mode: bool,
    edit_field: EditField,
    edit_buffer: String,
    edit_cursor: usize,
    edit_task_id: Option<i64>,
    show_edit_picker: bool,
    edit_picker_state: TableState,
    // Claude panes (task_id → active pane)
    claude_panes: HashMap<i64, pty::ClaudePane>,
    claude_focus: bool,
    claude_pane_area: Rect,
    show_claude_picker: bool,
    claude_picker_state: TableState,
    // Active Claude session IDs (detected from running processes)
    active_session_ids: HashSet<String>,
    // Animation tick counter for spinner effects
    animation_tick: usize,
    last_animation: std::time::Instant,
    // Full-text search
    search_mode: bool,
    search_query: String,
    search_cursor: usize,
}

// ── App logic ───────────────────────────────────────────────────────────────

impl App {
    fn new(db: Db) -> Self {
        let mut table_state = TableState::default();
        table_state.select(Some(0));
        let zero_rect = ratatui::layout::Rect::default();
        let mut tag_picker_state = TableState::default();
        tag_picker_state.select(Some(0));
        let mut app = App {
            db,
            tasks: vec![],
            table_state,
            active_tab: ActiveTab::All,
            group_by: GroupBy::None,
            sort_by: SortBy::Manual,
            tag_filter: None,
            show_tag_picker: false,
            tag_picker_state,
            show_detail: true,
            show_help: false,
            confirm_delete: false,
            confirm_quit: false,
            collapsed: HashSet::new(),
            quit: false,
            input_mode: false,
            input_buffer: String::new(),
            input_cursor: 0,
            input_parent_id: None,
            edit_mode: false,
            edit_field: EditField::Title,
            edit_buffer: String::new(),
            edit_cursor: 0,
            edit_task_id: None,
            show_edit_picker: false,
            edit_picker_state: {
                let mut s = TableState::default();
                s.select(Some(0));
                s
            },
            table_area: zero_rect,
            tab_area: zero_rect,
            claude_panes: HashMap::new(),
            claude_focus: false,
            show_claude_picker: false,
            claude_picker_state: {
                let mut s = TableState::default();
                s.select(Some(0));
                s
            },
            claude_pane_area: zero_rect,
            active_session_ids: HashSet::new(),
            animation_tick: 0,
            last_animation: std::time::Instant::now(),
            search_mode: false,
            search_query: String::new(),
            search_cursor: 0,
        };
        app.reload_tasks();
        app.restore_view_state();
        app.refresh_active_sessions();
        app
    }

    fn save_view_state(&self) {
        let _ = self.db.set_view_state("active_tab", self.active_tab.as_str());
        let _ = self.db.set_view_state("group_by", self.group_by.label());
        let _ = self.db.set_view_state("sort_by", self.sort_by.as_str());
        let _ = self.db.set_view_state("show_detail", if self.show_detail { "1" } else { "0" });
        let _ = self.db.set_view_state(
            "tag_filter",
            self.tag_filter.as_deref().unwrap_or(""),
        );
        let collapsed_str: String = self
            .collapsed
            .iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let _ = self.db.set_view_state("collapsed", &collapsed_str);
        // Save selected task ID so we can restore cursor position
        if let Some(tv) = self.selected_task_view() {
            let _ = self.db.set_view_state("selected_task_id", &tv.task.id.to_string());
        }
    }

    fn restore_view_state(&mut self) {
        if let Ok(Some(v)) = self.db.get_view_state("active_tab") {
            self.active_tab = ActiveTab::from_str(&v);
        }
        if let Ok(Some(v)) = self.db.get_view_state("group_by") {
            self.group_by = GroupBy::from_str(&v);
        }
        if let Ok(Some(v)) = self.db.get_view_state("sort_by") {
            self.sort_by = SortBy::from_str(&v);
        }
        if let Ok(Some(v)) = self.db.get_view_state("show_detail") {
            self.show_detail = v == "1";
        }
        if let Ok(Some(v)) = self.db.get_view_state("tag_filter") {
            self.tag_filter = if v.is_empty() { None } else { Some(v) };
        }
        if let Ok(Some(v)) = self.db.get_view_state("collapsed") {
            self.collapsed = v
                .split(',')
                .filter(|s| !s.is_empty())
                .filter_map(|s| s.parse::<i64>().ok())
                .collect();
        }
        // Restore cursor to previously selected task
        if let Ok(Some(v)) = self.db.get_view_state("selected_task_id") {
            if let Ok(id) = v.parse::<i64>() {
                let display = self.build_display_rows();
                if let Some(pos) = display.iter().position(|dr| {
                    matches!(dr, DisplayRow::Task { idx, .. } if self.tasks[*idx].task.id == id)
                }) {
                    self.table_state.select(Some(pos));
                }
            }
        }
    }

    fn refresh_active_sessions(&mut self) {
        self.active_session_ids = detect_active_session_ids();
        // Also include sessions from active Claude panes managed by this TUI
        for pane in self.claude_panes.values() {
            if !pane.exited {
                self.active_session_ids.insert(pane.session_id.clone());
            }
        }
    }

    fn reload_tasks(&mut self) {
        let tasks = self.db.all_tasks().unwrap_or_default();
        self.tasks = tasks
            .into_iter()
            .map(|task| {
                let session_count = self.db.session_count(task.id).unwrap_or(0);
                let sessions = self.db.sessions_for_task(task.id).unwrap_or_default();
                TaskView {
                    task,
                    session_count,
                    sessions,
                }
            })
            .collect();
    }

    fn filtered_tasks(&self) -> Vec<&TaskView> {
        self.tasks
            .iter()
            .filter(|tv| self.active_tab.filter(tv.task.status))
            .filter(|tv| match &self.tag_filter {
                None => true,
                Some(tag) => tv.task.tags.contains(tag),
            })
            .filter(|tv| self.matches_search(tv))
            .collect()
    }

    // ── Tree helpers ────────────────────────────────────────────────────────

    /// Build a map from parent_id -> list of child indices into self.tasks.
    fn children_map(&self) -> HashMap<Option<i64>, Vec<usize>> {
        let mut map: HashMap<Option<i64>, Vec<usize>> = HashMap::new();
        for (i, tv) in self.tasks.iter().enumerate() {
            map.entry(tv.task.parent_id).or_default().push(i);
        }
        map
    }

    /// Depth-first traversal of the task tree, respecting collapsed state and sort order.
    fn tree_walk(&self) -> Vec<(usize, usize)> {
        let children = self.children_map();
        let mut result = Vec::new();
        let mut visited = HashSet::new();
        let sort_by = self.sort_by;

        fn walk(
            parent_id: Option<i64>,
            depth: usize,
            children: &HashMap<Option<i64>, Vec<usize>>,
            tasks: &[TaskView],
            collapsed: &HashSet<i64>,
            visited: &mut HashSet<i64>,
            result: &mut Vec<(usize, usize)>,
            sort_by: SortBy,
        ) {
            if let Some(kids) = children.get(&parent_id) {
                let sorted: Vec<usize> = match sort_by {
                    SortBy::Manual => kids.clone(),
                    SortBy::Status => {
                        let mut v = kids.clone();
                        v.sort_by_key(|&idx| {
                            match tasks[idx].task.status {
                                Status::InProgress => 0,
                                Status::Todo => 1,
                                Status::InReview => 2,
                                Status::Blocked => 3,
                                Status::Done => 4,
                            }
                        });
                        v
                    }
                    SortBy::Alphabetical => {
                        let mut v = kids.clone();
                        v.sort_by(|&a, &b| {
                            tasks[a].task.title.to_lowercase().cmp(&tasks[b].task.title.to_lowercase())
                        });
                        v
                    }
                    SortBy::LastModified => {
                        let mut v = kids.clone();
                        v.sort_by(|&a, &b| {
                            tasks[b].task.updated_at.cmp(&tasks[a].task.updated_at)
                        });
                        v
                    }
                };
                for idx in sorted {
                    let task_id = tasks[idx].task.id;
                    if !visited.insert(task_id) {
                        continue; // cycle guard
                    }
                    result.push((idx, depth));
                    if !collapsed.contains(&task_id) {
                        walk(
                            Some(task_id),
                            depth + 1,
                            children,
                            tasks,
                            collapsed,
                            visited,
                            result,
                            sort_by,
                        );
                    }
                }
            }
        }

        walk(
            None,
            0,
            &children,
            &self.tasks,
            &self.collapsed,
            &mut visited,
            &mut result,
            sort_by,
        );
        result
    }

    fn has_children(&self, task_id: i64) -> bool {
        self.tasks
            .iter()
            .any(|tv| tv.task.parent_id == Some(task_id))
    }

    // ── Display rows ────────────────────────────────────────────────────────

    fn build_display_rows(&self) -> Vec<DisplayRow> {
        let has_filter = self.active_tab != ActiveTab::All
            || self.tag_filter.is_some()
            || !self.search_query.is_empty();

        if self.group_by == GroupBy::None {
            let tree = self.tree_walk();

            if !has_filter {
                return tree
                    .into_iter()
                    .map(|(idx, depth)| DisplayRow::Task { idx, depth, dimmed: false })
                    .collect();
            }

            // Filter is active — include matching tasks plus their ancestors
            // for tree structure. Ancestors that don't match are shown dimmed.
            let matches_filter = |idx: usize| -> bool {
                let tv = &self.tasks[idx];
                self.active_tab.filter(tv.task.status)
                    && match &self.tag_filter {
                        None => true,
                        Some(tag) => tv.task.tags.contains(tag),
                    }
                    && self.matches_search(tv)
            };

            // Collect IDs of tasks that directly match the filter
            let matching_ids: HashSet<i64> = tree
                .iter()
                .filter(|(idx, _)| matches_filter(*idx))
                .map(|(idx, _)| self.tasks[*idx].task.id)
                .collect();

            // Also include ancestors of matching tasks (for tree structure)
            let mut visible_ids: HashSet<i64> = matching_ids.clone();
            for &id in &matching_ids {
                let mut current_parent = self.tasks.iter()
                    .find(|tv| tv.task.id == id)
                    .and_then(|tv| tv.task.parent_id);
                while let Some(pid) = current_parent {
                    if !visible_ids.insert(pid) {
                        break; // already added this ancestor chain
                    }
                    current_parent = self.tasks.iter()
                        .find(|tv| tv.task.id == pid)
                        .and_then(|tv| tv.task.parent_id);
                }
            }

            return tree
                .into_iter()
                .filter(|(idx, _)| visible_ids.contains(&self.tasks[*idx].task.id))
                .map(|(idx, depth)| DisplayRow::Task {
                    idx,
                    depth,
                    dimmed: !matching_ids.contains(&self.tasks[idx].task.id),
                })
                .collect();
        }

        // Grouped view: flat (depth 0) within groups
        let filtered: Vec<usize> = self
            .tasks
            .iter()
            .enumerate()
            .filter(|(_, tv)| {
                self.active_tab.filter(tv.task.status)
                    && match &self.tag_filter {
                        None => true,
                        Some(tag) => tv.task.tags.contains(tag),
                    }
                    && self.matches_search(tv)
            })
            .map(|(i, _)| i)
            .collect();

        let mut group_map: std::collections::BTreeMap<String, Vec<usize>> =
            std::collections::BTreeMap::new();

        for &idx in &filtered {
            let task = &self.tasks[idx].task;
            let keys = match self.group_by {
                GroupBy::Status => vec![task.status.label().to_string()],
                GroupBy::Priority => vec![task.priority.label().to_string()],
                GroupBy::Tag => {
                    if task.tags.is_empty() {
                        vec!["untagged".to_string()]
                    } else {
                        task.tags.clone()
                    }
                }
                GroupBy::None => unreachable!(),
            };
            for key in keys {
                group_map.entry(key).or_default().push(idx);
            }
        }

        let ordered_keys: Vec<String> = match self.group_by {
            GroupBy::Status => vec!["IN PROGRESS", "TODO", "IN REVIEW", "BLOCKED", "DONE"]
                .into_iter()
                .map(String::from)
                .filter(|k| group_map.contains_key(k))
                .collect(),
            GroupBy::Priority => vec!["crit", "high", "med", "low"]
                .into_iter()
                .map(String::from)
                .filter(|k| group_map.contains_key(k))
                .collect(),
            _ => group_map.keys().cloned().collect(),
        };

        let mut rows = Vec::new();
        for key in ordered_keys {
            if let Some(indices) = group_map.remove(&key) {
                rows.push(DisplayRow::Header(key));
                for idx in indices {
                    rows.push(DisplayRow::Task { idx, depth: 0, dimmed: false });
                }
            }
        }
        rows
    }

    fn selected_task_view(&self) -> Option<&TaskView> {
        let display = self.build_display_rows();
        self.table_state.selected().and_then(|i| {
            display.get(i).and_then(|row| match row {
                DisplayRow::Task { idx, .. } => Some(&self.tasks[*idx]),
                DisplayRow::Header(_) => None,
            })
        })
    }

    fn select_first_task(&mut self) {
        let display = self.build_display_rows();
        let first = display
            .iter()
            .position(|r| matches!(r, DisplayRow::Task { .. }));
        self.table_state.select(first.or(Some(0)));
    }

    /// Check if a task matches the current search query (title or description).
    fn matches_search(&self, tv: &TaskView) -> bool {
        if self.search_query.is_empty() {
            return true;
        }
        let query = self.search_query.to_lowercase();
        tv.task.title.to_lowercase().contains(&query)
            || tv.task.description.to_lowercase().contains(&query)
    }

    fn all_tags(&self) -> Vec<String> {
        let set: BTreeSet<&str> = self
            .tasks
            .iter()
            .flat_map(|tv| tv.task.tags.iter().map(|s| s.as_str()))
            .collect();
        set.into_iter().map(String::from).collect()
    }

    // ── Actions ─────────────────────────────────────────────────────────────

    fn delete_selected(&mut self) {
        if let Some(tv) = self.selected_task_view() {
            let id = tv.task.id;
            let _ = self.db.delete_task(id);
            self.collapsed.remove(&id);
            self.reload_tasks();
            self.select_first_task();
        }
    }

    // ── Claude pane ───────────────────────────────────────────────────────

    fn selected_task_id(&self) -> Option<i64> {
        self.selected_task_view().map(|tv| tv.task.id)
    }

    fn spawn_claude_pane(&mut self) {
        let (task, subtasks) = match self.selected_task_view() {
            Some(tv) => {
                let task = tv.task.clone();
                let subtasks: Vec<Task> = self
                    .tasks
                    .iter()
                    .filter(|t| t.task.parent_id == Some(task.id))
                    .map(|t| t.task.clone())
                    .collect();
                (task, subtasks)
            }
            None => return,
        };

        // Kill existing pane for this task if any
        if let Some(mut old) = self.claude_panes.remove(&task.id) {
            old.kill();
        }

        let area = self.claude_pane_area;
        let cols = if area.width > 2 { area.width - 2 } else { 80 };
        let rows = if area.height > 2 { area.height - 2 } else { 24 };

        if let Ok(pane) = pty::ClaudePane::spawn(&task, &subtasks, cols, rows) {
            let _ = self.db.add_session(pane.task_id, &pane.session_id);
            let task_id = pane.task_id;
            // Mark the task as in_progress when an agent starts working on it
            let _ = self.db.update_status(task_id, Status::InProgress);
            self.claude_panes.insert(task_id, pane);
            self.claude_focus = true;
            self.show_detail = false;
            self.reload_tasks(); // refresh session counts + status change
        }
    }

    fn resume_claude_pane_by_id(&mut self, task_id: i64, session_id: String) {
        // Kill existing pane for this task if any
        if let Some(mut old) = self.claude_panes.remove(&task_id) {
            old.kill();
        }

        let area = self.claude_pane_area;
        let cols = if area.width > 2 { area.width - 2 } else { 80 };
        let rows = if area.height > 2 { area.height - 2 } else { 24 };

        if let Ok(pane) = pty::ClaudePane::resume(&session_id, task_id, cols, rows) {
            self.claude_panes.insert(task_id, pane);
            self.claude_focus = true;
            self.show_detail = false;
        }
    }

    fn close_claude_pane(&mut self) {
        if let Some(task_id) = self.selected_task_id()
            && let Some(mut pane) = self.claude_panes.remove(&task_id)
        {
            pane.kill();
        }
        if self.claude_panes.is_empty() {
            self.claude_focus = false;
        }
    }

    fn close_all_claude_panes(&mut self) {
        for (_, mut pane) in self.claude_panes.drain() {
            pane.kill();
        }
        self.claude_focus = false;
    }

    // ── Key handling ────────────────────────────────────────────────────────

    fn handle_key(&mut self, code: KeyCode) {
        if self.search_mode {
            self.handle_search_key(code);
            return;
        }
        if self.edit_mode {
            self.handle_edit_key(code);
            return;
        }
        if self.input_mode {
            self.handle_input_key(code);
            return;
        }
        if self.show_edit_picker {
            self.handle_edit_picker_key(code);
            return;
        }
        if self.confirm_quit {
            if code == KeyCode::Char('y') {
                self.close_all_claude_panes();
                self.save_view_state();
                self.quit = true;
            }
            self.confirm_quit = false;
            return;
        }
        if self.confirm_delete {
            if code == KeyCode::Char('y') {
                self.delete_selected();
            }
            self.confirm_delete = false;
            return;
        }
        if self.show_tag_picker {
            self.handle_tag_picker_key(code);
            return;
        }
        if self.show_claude_picker {
            self.handle_claude_picker_key(code);
            return;
        }
        if self.show_help {
            self.show_help = false;
            return;
        }
        let display = self.build_display_rows();
        let display_len = display.len();
        match code {
            KeyCode::Esc => {
                if !self.search_query.is_empty() {
                    self.search_query.clear();
                    self.select_first_task();
                } else {
                    let running = self.claude_panes.values().any(|p| !p.exited);
                    if running {
                        self.confirm_quit = true;
                    } else {
                        self.quit = true;
                    }
                }
            }
            KeyCode::Char('q') => {
                let running = self.claude_panes.values().any(|p| !p.exited);
                if running {
                    self.confirm_quit = true;
                } else {
                    self.save_view_state();
                    self.quit = true;
                }
            }
            KeyCode::Char('j') | KeyCode::Down => {
                if display_len > 0 {
                    let mut i = self.table_state.selected().unwrap_or(0);
                    loop {
                        i = (i + 1) % display_len;
                        if matches!(display[i], DisplayRow::Task { .. }) {
                            break;
                        }
                    }
                    self.table_state.select(Some(i));
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if display_len > 0 {
                    let mut i = self.table_state.selected().unwrap_or(0);
                    loop {
                        i = i.checked_sub(1).unwrap_or(display_len - 1);
                        if matches!(display[i], DisplayRow::Task { .. }) {
                            break;
                        }
                    }
                    self.table_state.select(Some(i));
                }
            }
            KeyCode::Tab => {
                self.active_tab = match self.active_tab {
                    ActiveTab::All => ActiveTab::Active,
                    ActiveTab::Active => ActiveTab::InReview,
                    ActiveTab::InReview => ActiveTab::Blocked,
                    ActiveTab::Blocked => ActiveTab::Done,
                    ActiveTab::Done => ActiveTab::All,
                };
                self.select_first_task();
            }
            KeyCode::BackTab => {
                self.active_tab = match self.active_tab {
                    ActiveTab::All => ActiveTab::Done,
                    ActiveTab::Active => ActiveTab::All,
                    ActiveTab::InReview => ActiveTab::Active,
                    ActiveTab::Blocked => ActiveTab::InReview,
                    ActiveTab::Done => ActiveTab::Blocked,
                };
                self.select_first_task();
            }
            // Expand
            KeyCode::Char('l') | KeyCode::Enter => {
                if let Some(task_id) = self.selected_task_view().map(|tv| tv.task.id) {
                    self.collapsed.remove(&task_id);
                }
            }
            // Collapse / go to parent
            KeyCode::Char('h') => {
                if let Some(tv) = self.selected_task_view() {
                    let task_id = tv.task.id;
                    let has_kids = self.has_children(task_id);
                    if has_kids && !self.collapsed.contains(&task_id) {
                        self.collapsed.insert(task_id);
                    } else if let Some(parent_id) = tv.task.parent_id {
                        // Jump to parent
                        let display = self.build_display_rows();
                        if let Some(pos) = display.iter().position(|dr| {
                            matches!(dr, DisplayRow::Task { idx, .. }
                                if self.tasks[*idx].task.id == parent_id)
                        }) {
                            self.table_state.select(Some(pos));
                        }
                    }
                }
            }
            // Expand all
            KeyCode::Char('L') => {
                self.collapsed.clear();
            }
            // Collapse all
            KeyCode::Char('H') => {
                let children = self.children_map();
                for tv in &self.tasks {
                    if children
                        .get(&Some(tv.task.id))
                        .map_or(false, |c| !c.is_empty())
                    {
                        self.collapsed.insert(tv.task.id);
                    }
                }
                self.select_first_task();
            }
            // Indent: reparent under previous sibling
            KeyCode::Char('>') => {
                if let Some(tv) = self.selected_task_view() {
                    let task_id = tv.task.id;
                    let current_parent = tv.task.parent_id;
                    // Find the previous sibling in task list order
                    let my_pos = self.tasks.iter().position(|t| t.task.id == task_id);
                    if let Some(my_pos) = my_pos {
                        let prev_sibling = self
                            .tasks
                            .iter()
                            .enumerate()
                            .filter(|(i, t)| {
                                *i < my_pos
                                    && t.task.parent_id == current_parent
                                    && t.task.id != task_id
                            })
                            .last();
                        if let Some((_, prev_tv)) = prev_sibling {
                            let new_parent_id = prev_tv.task.id;
                            let _ = self.db.reparent_task(task_id, Some(new_parent_id));
                            self.collapsed.remove(&new_parent_id);
                            self.reload_tasks();
                        }
                    }
                }
            }
            // Outdent: move to grandparent level
            KeyCode::Char('<') => {
                if let Some(tv) = self.selected_task_view() {
                    let task_id = tv.task.id;
                    if let Some(parent_id) = tv.task.parent_id {
                        // Find grandparent
                        let grandparent = self
                            .tasks
                            .iter()
                            .find(|t| t.task.id == parent_id)
                            .and_then(|t| t.task.parent_id);
                        let _ = self.db.reparent_task(task_id, grandparent);
                        self.reload_tasks();
                    }
                }
            }
            // Move task up among siblings
            KeyCode::Char('K') => {
                if let Some(tv) = self.selected_task_view() {
                    let task_id = tv.task.id;
                    let parent_id = tv.task.parent_id;
                    let my_global = self.tasks.iter().position(|t| t.task.id == task_id);
                    if let Some(my_global) = my_global {
                        // Find the sibling just before us (same parent, earlier in list)
                        let prev = self
                            .tasks
                            .iter()
                            .enumerate()
                            .filter(|(i, t)| {
                                *i < my_global
                                    && t.task.parent_id == parent_id
                                    && t.task.id != task_id
                            })
                            .next_back();
                        if let Some((_, prev_tv)) = prev {
                            let prev_id = prev_tv.task.id;
                            let _ = self.db.swap_task_order(task_id, prev_id);
                            self.reload_tasks();
                            // Follow the task
                            let display = self.build_display_rows();
                            if let Some(pos) = display.iter().position(|dr| {
                                matches!(dr, DisplayRow::Task { idx, .. } if self.tasks[*idx].task.id == task_id)
                            }) {
                                self.table_state.select(Some(pos));
                            }
                        }
                    }
                }
            }
            // Move task down among siblings
            KeyCode::Char('J') => {
                if let Some(tv) = self.selected_task_view() {
                    let task_id = tv.task.id;
                    let parent_id = tv.task.parent_id;
                    let my_global = self.tasks.iter().position(|t| t.task.id == task_id);
                    if let Some(my_global) = my_global {
                        let next = self
                            .tasks
                            .iter()
                            .enumerate()
                            .find(|(i, t)| {
                                *i > my_global
                                    && t.task.parent_id == parent_id
                                    && t.task.id != task_id
                            });
                        if let Some((_, next_tv)) = next {
                            let next_id = next_tv.task.id;
                            let _ = self.db.swap_task_order(task_id, next_id);
                            self.reload_tasks();
                            // Follow the task
                            let display = self.build_display_rows();
                            if let Some(pos) = display.iter().position(|dr| {
                                matches!(dr, DisplayRow::Task { idx, .. } if self.tasks[*idx].task.id == task_id)
                            }) {
                                self.table_state.select(Some(pos));
                            }
                        }
                    }
                }
            }
            KeyCode::Char('s') => {
                if let Some(id) = self.selected_task_view().map(|tv| tv.task.id) {
                    if let Some(idx) = self.tasks.iter().position(|t| t.task.id == id) {
                        let new_status = self.tasks[idx].task.status.next();
                        if self.db.update_status(id, new_status).is_ok() {
                            self.tasks[idx].task.status = new_status;
                        }
                    }
                }
            }
            KeyCode::Char('S') => {
                if let Some(id) = self.selected_task_view().map(|tv| tv.task.id) {
                    if let Some(idx) = self.tasks.iter().position(|t| t.task.id == id) {
                        let new_status = if self.tasks[idx].task.status == Status::Blocked {
                            Status::Todo
                        } else {
                            Status::Blocked
                        };
                        if self.db.update_status(id, new_status).is_ok() {
                            self.tasks[idx].task.status = new_status;
                        }
                    }
                }
            }
            KeyCode::Char('g') => {
                self.group_by = self.group_by.next();
                self.select_first_task();
            }
            KeyCode::Char('o') => {
                self.sort_by = self.sort_by.next();
                self.select_first_task();
            }
            KeyCode::Char('t') => {
                self.show_tag_picker = true;
                self.tag_picker_state.select(Some(0));
            }
            KeyCode::Char('x') | KeyCode::Delete => {
                if self.selected_task_view().is_some() {
                    self.confirm_delete = true;
                }
            }
            // Add sibling task (same parent as selected)
            KeyCode::Char('a') => {
                let parent_id = self
                    .selected_task_view()
                    .and_then(|tv| tv.task.parent_id);
                self.input_parent_id = parent_id;
                self.input_buffer.clear();
                self.input_cursor = 0;
                self.input_mode = true;
            }
            // Add child task (under selected)
            KeyCode::Char('A') => {
                let parent_id = self.selected_task_view().map(|tv| tv.task.id);
                self.input_parent_id = parent_id;
                self.input_buffer.clear();
                self.input_cursor = 0;
                self.input_mode = true;
            }
            KeyCode::Char('e') => {
                if self.selected_task_view().is_some() {
                    self.show_edit_picker = true;
                    self.edit_picker_state.select(Some(0));
                }
            }
            KeyCode::Char('d') => {
                self.show_detail = true;
            }
            KeyCode::Char('c') | KeyCode::Char('C') => {
                if let Some(task_id) = self.selected_task_id() {
                    if self.claude_panes.contains_key(&task_id) {
                        // Running pane exists — just show it
                        self.show_detail = false;
                    } else {
                        // No running pane — open picker to start or resume
                        self.show_claude_picker = true;
                        self.claude_picker_state.select(Some(0));
                    }
                }
            }
            KeyCode::Char('/') => {
                self.search_mode = true;
                self.search_query.clear();
                self.search_cursor = 0;
            }
            KeyCode::Char('?') => self.show_help = true,
            _ => {}
        }
    }

    fn handle_search_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc => {
                self.search_mode = false;
                self.search_query.clear();
                self.select_first_task();
            }
            KeyCode::Enter => {
                self.search_mode = false;
                // Keep the search query active as a filter
                self.select_first_task();
            }
            KeyCode::Backspace => {
                if self.search_cursor > 0 {
                    self.search_cursor -= 1;
                    self.search_query.remove(self.search_cursor);
                    self.select_first_task();
                }
            }
            KeyCode::Delete => {
                if self.search_cursor < self.search_query.len() {
                    self.search_query.remove(self.search_cursor);
                    self.select_first_task();
                }
            }
            KeyCode::Left => {
                if self.search_cursor > 0 {
                    self.search_cursor -= 1;
                }
            }
            KeyCode::Right => {
                if self.search_cursor < self.search_query.len() {
                    self.search_cursor += 1;
                }
            }
            KeyCode::Home => self.search_cursor = 0,
            KeyCode::End => self.search_cursor = self.search_query.len(),
            KeyCode::Char(c) => {
                self.search_query.insert(self.search_cursor, c);
                self.search_cursor += 1;
                self.select_first_task();
            }
            _ => {}
        }
    }

    fn handle_input_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc => {
                self.input_mode = false;
                self.input_buffer.clear();
            }
            KeyCode::Enter => {
                let title = self.input_buffer.trim().to_string();
                if !title.is_empty() {
                    let _ = self.db.add_task(
                        &title,
                        Priority::Medium,
                        &[],
                        "",
                        self.input_parent_id,
                    );
                    // Expand parent so the new task is visible
                    if let Some(pid) = self.input_parent_id {
                        self.collapsed.remove(&pid);
                    }
                    self.reload_tasks();
                    // Select the newly created task (last task with matching parent)
                    let display = self.build_display_rows();
                    if let Some(pos) = display.iter().rposition(|dr| {
                        matches!(dr, DisplayRow::Task { idx, .. }
                            if self.tasks[*idx].task.title == title
                                && self.tasks[*idx].task.parent_id == self.input_parent_id)
                    }) {
                        self.table_state.select(Some(pos));
                    }
                }
                self.input_mode = false;
                self.input_buffer.clear();
            }
            KeyCode::Backspace => {
                if self.input_cursor > 0 {
                    self.input_cursor -= 1;
                    self.input_buffer.remove(self.input_cursor);
                }
            }
            KeyCode::Delete => {
                if self.input_cursor < self.input_buffer.len() {
                    self.input_buffer.remove(self.input_cursor);
                }
            }
            KeyCode::Left => {
                if self.input_cursor > 0 {
                    self.input_cursor -= 1;
                }
            }
            KeyCode::Right => {
                if self.input_cursor < self.input_buffer.len() {
                    self.input_cursor += 1;
                }
            }
            KeyCode::Home => self.input_cursor = 0,
            KeyCode::End => self.input_cursor = self.input_buffer.len(),
            KeyCode::Char(c) => {
                self.input_buffer.insert(self.input_cursor, c);
                self.input_cursor += 1;
            }
            _ => {}
        }
    }

    fn handle_tag_picker_key(&mut self, code: KeyCode) {
        let tags = self.all_tags();
        let item_count = tags.len() + 1;
        match code {
            KeyCode::Esc | KeyCode::Char('t') => {
                self.show_tag_picker = false;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                let i = self.tag_picker_state.selected().unwrap_or(0);
                self.tag_picker_state.select(Some((i + 1) % item_count));
            }
            KeyCode::Char('k') | KeyCode::Up => {
                let i = self.tag_picker_state.selected().unwrap_or(0);
                self.tag_picker_state
                    .select(Some(i.checked_sub(1).unwrap_or(item_count - 1)));
            }
            KeyCode::Enter => {
                let i = self.tag_picker_state.selected().unwrap_or(0);
                if i == 0 {
                    self.tag_filter = None;
                } else {
                    self.tag_filter = tags.get(i - 1).cloned();
                }
                self.show_tag_picker = false;
                self.table_state.select(Some(0));
            }
            _ => {}
        }
    }

    fn handle_claude_picker_key(&mut self, code: KeyCode) {
        let sessions = self
            .selected_task_view()
            .map(|tv| tv.sessions.clone())
            .unwrap_or_default();
        // Items: "New session" + each existing session (most recent first)
        let item_count = 1 + sessions.len();
        match code {
            KeyCode::Esc | KeyCode::Char('c') => {
                self.show_claude_picker = false;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                let i = self.claude_picker_state.selected().unwrap_or(0);
                self.claude_picker_state.select(Some((i + 1) % item_count));
            }
            KeyCode::Char('k') | KeyCode::Up => {
                let i = self.claude_picker_state.selected().unwrap_or(0);
                self.claude_picker_state
                    .select(Some(i.checked_sub(1).unwrap_or(item_count - 1)));
            }
            KeyCode::Enter => {
                let i = self.claude_picker_state.selected().unwrap_or(0);
                self.show_claude_picker = false;
                if i == 0 {
                    self.spawn_claude_pane();
                } else {
                    // Sessions are displayed most-recent-first, so reverse index
                    let rev_idx = sessions.len() - i;
                    if let Some(session) = sessions.get(rev_idx) {
                        let task_id = self
                            .selected_task_view()
                            .map(|tv| tv.task.id)
                            .unwrap_or(0);
                        self.resume_claude_pane_by_id(task_id, session.session_id.clone());
                    }
                }
            }
            KeyCode::Char('x') | KeyCode::Delete => {
                let i = self.claude_picker_state.selected().unwrap_or(0);
                if i > 0 {
                    let rev_idx = sessions.len() - i;
                    if let Some(session) = sessions.get(rev_idx) {
                        let _ = self.db.delete_session(&session.session_id);
                        self.reload_tasks();
                        // Clamp selection
                        let new_count = 1 + sessions.len() - 1;
                        if i >= new_count {
                            self.claude_picker_state.select(Some(new_count.saturating_sub(1)));
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn handle_edit_picker_key(&mut self, code: KeyCode) {
        let fields = EditField::all();
        let item_count = fields.len();
        match code {
            KeyCode::Esc | KeyCode::Char('e') => {
                self.show_edit_picker = false;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                let i = self.edit_picker_state.selected().unwrap_or(0);
                self.edit_picker_state.select(Some((i + 1) % item_count));
            }
            KeyCode::Char('k') | KeyCode::Up => {
                let i = self.edit_picker_state.selected().unwrap_or(0);
                self.edit_picker_state
                    .select(Some(i.checked_sub(1).unwrap_or(item_count - 1)));
            }
            KeyCode::Enter => {
                let i = self.edit_picker_state.selected().unwrap_or(0);
                let field = fields[i];
                if let Some(tv) = self.selected_task_view() {
                    let task = &tv.task;
                    let task_id = task.id;
                    match field {
                        EditField::Priority => {
                            // Cycle priority directly
                            let new_priority = match task.priority {
                                Priority::Low => Priority::Medium,
                                Priority::Medium => Priority::High,
                                Priority::High => Priority::Critical,
                                Priority::Critical => Priority::Low,
                            };
                            if let Some(idx) = self.tasks.iter().position(|t| t.task.id == task_id)
                            {
                                let _ = self.db.update_task(
                                    task_id,
                                    None,
                                    None,
                                    Some(new_priority),
                                    None,
                                    None,
                                );
                                self.tasks[idx].task.priority = new_priority;
                            }
                        }
                        _ => {
                            // Text fields: enter edit mode with pre-filled buffer
                            let value = match field {
                                EditField::Title => task.title.clone(),
                                EditField::Tags => task.tags.join(", "),
                                EditField::Description => task.description.clone(),
                                EditField::Priority => unreachable!(),
                            };
                            self.edit_task_id = Some(task_id);
                            self.edit_field = field;
                            self.edit_buffer = value;
                            self.edit_cursor = self.edit_buffer.len();
                            self.edit_mode = true;
                            self.show_edit_picker = false;
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn handle_edit_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc => {
                self.edit_mode = false;
                self.edit_buffer.clear();
                self.edit_task_id = None;
            }
            KeyCode::Enter => {
                if let Some(task_id) = self.edit_task_id {
                    let value = self.edit_buffer.clone();
                    match self.edit_field {
                        EditField::Title => {
                            if !value.trim().is_empty() {
                                let _ = self.db.update_task(
                                    task_id,
                                    Some(value.trim()),
                                    None,
                                    None,
                                    None,
                                    None,
                                );
                            }
                        }
                        EditField::Tags => {
                            let tags: Vec<String> = value
                                .split(',')
                                .map(|s| s.trim().to_string())
                                .filter(|s| !s.is_empty())
                                .collect();
                            let _ =
                                self.db
                                    .update_task(task_id, None, None, None, Some(&tags), None);
                        }
                        EditField::Description => {
                            let _ = self.db.update_task(
                                task_id,
                                None,
                                None,
                                None,
                                None,
                                Some(&value),
                            );
                        }
                        EditField::Priority => unreachable!(),
                    }
                    self.reload_tasks();
                }
                self.edit_mode = false;
                self.edit_buffer.clear();
                self.edit_task_id = None;
            }
            KeyCode::Backspace => {
                if self.edit_cursor > 0 {
                    self.edit_cursor -= 1;
                    self.edit_buffer.remove(self.edit_cursor);
                }
            }
            KeyCode::Delete => {
                if self.edit_cursor < self.edit_buffer.len() {
                    self.edit_buffer.remove(self.edit_cursor);
                }
            }
            KeyCode::Left => {
                if self.edit_cursor > 0 {
                    self.edit_cursor -= 1;
                }
            }
            KeyCode::Right => {
                if self.edit_cursor < self.edit_buffer.len() {
                    self.edit_cursor += 1;
                }
            }
            KeyCode::Home => self.edit_cursor = 0,
            KeyCode::End => self.edit_cursor = self.edit_buffer.len(),
            KeyCode::Char(c) => {
                self.edit_buffer.insert(self.edit_cursor, c);
                self.edit_cursor += 1;
            }
            _ => {}
        }
    }

    fn handle_mouse(&mut self, kind: MouseEventKind, column: u16, row: u16) {
        match kind {
            MouseEventKind::Down(_) => {
                if row > self.table_area.y
                    && column >= self.table_area.x
                    && column < self.table_area.x + self.table_area.width
                {
                    let row_index = (row - self.table_area.y - 1) as usize;
                    let display = self.build_display_rows();
                    if row_index < display.len()
                        && matches!(display[row_index], DisplayRow::Task { .. })
                    {
                        self.table_state.select(Some(row_index));
                    }
                }
                if row >= self.tab_area.y && row < self.tab_area.y + self.tab_area.height {
                    let x = column as usize;
                    let tab_texts = self.tab_labels();
                    let mut offset = 1;
                    for (i, label) in tab_texts.iter().enumerate() {
                        let tab_width = label.len() + 2;
                        let sep_width = 3;
                        if x >= offset && x < offset + tab_width {
                            self.active_tab = match i {
                                0 => ActiveTab::All,
                                1 => ActiveTab::Active,
                                2 => ActiveTab::InReview,
                                3 => ActiveTab::Blocked,
                                4 => ActiveTab::Done,
                                _ => self.active_tab,
                            };
                            self.select_first_task();
                            break;
                        }
                        offset += tab_width + sep_width;
                    }
                }
            }
            MouseEventKind::ScrollUp => {
                let display = self.build_display_rows();
                if !display.is_empty() {
                    let mut i = self.table_state.selected().unwrap_or(0);
                    loop {
                        i = i.checked_sub(1).unwrap_or(display.len() - 1);
                        if matches!(display[i], DisplayRow::Task { .. }) {
                            break;
                        }
                    }
                    self.table_state.select(Some(i));
                }
            }
            MouseEventKind::ScrollDown => {
                let display = self.build_display_rows();
                if !display.is_empty() {
                    let mut i = self.table_state.selected().unwrap_or(0);
                    loop {
                        i = (i + 1) % display.len();
                        if matches!(display[i], DisplayRow::Task { .. }) {
                            break;
                        }
                    }
                    self.table_state.select(Some(i));
                }
            }
            _ => {}
        }
    }

    fn tab_labels(&self) -> Vec<String> {
        vec![
            format!("All ({})", self.tasks.len()),
            format!(
                "Active ({})",
                self.tasks
                    .iter()
                    .filter(
                        |tv| tv.task.status == Status::InProgress || tv.task.status == Status::Todo
                    )
                    .count()
            ),
            format!(
                "Review ({})",
                self.tasks
                    .iter()
                    .filter(|tv| tv.task.status == Status::InReview)
                    .count()
            ),
            format!(
                "Blocked ({})",
                self.tasks
                    .iter()
                    .filter(|tv| tv.task.status == Status::Blocked)
                    .count()
            ),
            format!(
                "Done ({})",
                self.tasks
                    .iter()
                    .filter(|tv| tv.task.status == Status::Done)
                    .count()
            ),
        ]
    }
}

// ── Rendering ───────────────────────────────────────────────────────────────

fn ui(f: &mut Frame, app: &mut App) {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // header
            Constraint::Length(2), // tabs
            Constraint::Min(8),   // body
            Constraint::Length(1), // status bar
        ])
        .split(f.area());

    app.tab_area = outer[1];
    render_header(f, outer[0], app);
    render_tabs(f, outer[1], app);
    render_body(f, outer[2], app);
    if app.edit_mode {
        render_edit_bar(f, outer[3], app);
    } else if app.input_mode {
        render_input_bar(f, outer[3], app);
    } else if app.search_mode {
        render_search_bar(f, outer[3], app);
    } else {
        render_status_bar(f, outer[3], app);
    }

    if app.show_help {
        render_help_popup(f);
    }
    if app.confirm_delete {
        render_delete_confirm(f, app);
    }
    if app.confirm_quit {
        render_quit_confirm(f, app);
    }
    if app.show_tag_picker {
        render_tag_picker(f, app);
    }
    if app.show_claude_picker {
        render_claude_picker(f, app);
    }
    if app.show_edit_picker {
        render_edit_picker(f, app);
    }
}

fn render_header(f: &mut Frame, area: ratatui::layout::Rect, app: &App) {
    let mut title = vec![
        Span::styled("  cli", Style::default().fg(Color::Cyan).bold()),
        Span::styled("-", Style::default().fg(Color::DarkGray)),
        Span::styled("todo", Style::default().fg(Color::White).bold()),
        Span::styled("  ", Style::default()),
        Span::styled(
            "developer control plane",
            Style::default().fg(Color::DarkGray),
        ),
    ];
    if app.group_by != GroupBy::None {
        title.push(Span::styled(
            "  group: ",
            Style::default().fg(Color::DarkGray),
        ));
        title.push(Span::styled(
            format!(" {} ", app.group_by.label()),
            Style::default()
                .fg(Color::Cyan)
                .bg(Color::Rgb(20, 40, 50)),
        ));
    }
    if app.sort_by != SortBy::Manual {
        title.push(Span::styled(
            "  sort: ",
            Style::default().fg(Color::DarkGray),
        ));
        title.push(Span::styled(
            format!(" {} ", app.sort_by.label()),
            Style::default()
                .fg(Color::Cyan)
                .bg(Color::Rgb(20, 40, 50)),
        ));
    }
    if let Some(tag) = &app.tag_filter {
        title.push(Span::styled(
            "  filter: ",
            Style::default().fg(Color::DarkGray),
        ));
        title.push(Span::styled(
            format!(" {} ", tag),
            Style::default()
                .fg(Color::Cyan)
                .bg(Color::Rgb(20, 40, 50)),
        ));
    }
    if !app.search_query.is_empty() {
        title.push(Span::styled(
            "  search: ",
            Style::default().fg(Color::DarkGray),
        ));
        title.push(Span::styled(
            format!(" {} ", app.search_query),
            Style::default()
                .fg(Color::Cyan)
                .bg(Color::Rgb(20, 40, 50)),
        ));
    }
    let header = Paragraph::new(Line::from(title)).block(
        Block::default()
            .borders(Borders::BOTTOM)
            .border_style(Style::default().fg(Color::DarkGray))
            .border_type(BorderType::Plain)
            .padding(Padding::new(1, 0, 1, 0)),
    );
    f.render_widget(header, area);
}

fn render_tabs(f: &mut Frame, area: ratatui::layout::Rect, app: &App) {
    let tab_titles: Vec<Line> = vec![
        format!(" All ({}) ", app.tasks.len()),
        format!(
            " Active ({}) ",
            app.tasks
                .iter()
                .filter(
                    |tv| tv.task.status == Status::InProgress || tv.task.status == Status::Todo
                )
                .count()
        ),
        format!(
            " Review ({}) ",
            app.tasks
                .iter()
                .filter(|tv| tv.task.status == Status::InReview)
                .count()
        ),
        format!(
            " Blocked ({}) ",
            app.tasks
                .iter()
                .filter(|tv| tv.task.status == Status::Blocked)
                .count()
        ),
        format!(
            " Done ({}) ",
            app.tasks
                .iter()
                .filter(|tv| tv.task.status == Status::Done)
                .count()
        ),
    ]
    .into_iter()
    .map(Line::from)
    .collect();

    let tabs = Tabs::new(tab_titles)
        .select(app.active_tab.index())
        .style(Style::default().fg(Color::DarkGray))
        .highlight_style(Style::default().fg(Color::Cyan).bold())
        .divider(Span::styled(
            " │ ",
            Style::default().fg(Color::DarkGray),
        ))
        .padding(" ", " ");

    f.render_widget(tabs, area);
}

fn render_body(f: &mut Frame, area: Rect, app: &mut App) {
    if !app.claude_panes.is_empty() && !app.show_detail {
        // Claude pane view
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
            .split(area);
        app.table_area = chunks[0];
        app.claude_pane_area = chunks[1];
        render_task_table(f, chunks[0], app);
        render_claude_pane(f, chunks[1], app);
    } else if app.show_detail {
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
            .split(area);
        app.table_area = chunks[0];
        app.claude_pane_area = area; // store for initial spawn sizing
        render_task_table(f, chunks[0], app);
        render_detail_panel(f, chunks[1], app);
    } else {
        app.table_area = area;
        app.claude_pane_area = area;
        render_task_table(f, area, app);
    }
}

/// Compute tree-line prefix for a task row at a given position in the display list.
fn tree_prefix(display: &[DisplayRow], row_index: usize) -> String {
    let depth = match &display[row_index] {
        DisplayRow::Task { depth, .. } => *depth,
        _ => return String::new(),
    };
    if depth == 0 {
        return String::new();
    }

    let mut prefix = String::new();

    // For each ancestor depth level (1..depth-1), check if there's a subsequent
    // sibling at that depth, meaning we need a │ connector.
    for d in 1..depth {
        let has_future_sibling = display[row_index + 1..]
            .iter()
            .take_while(|r| match r {
                DisplayRow::Task { depth: rd, .. } => *rd >= d,
                DisplayRow::Header(_) => true,
            })
            .any(|r| matches!(r, DisplayRow::Task { depth: rd, .. } if *rd == d));
        if has_future_sibling {
            prefix.push_str("│  ");
        } else {
            prefix.push_str("   ");
        }
    }

    // Check if this is the last child at its depth level
    let is_last = !display[row_index + 1..]
        .iter()
        .take_while(|r| match r {
            DisplayRow::Task { depth: rd, .. } => *rd >= depth,
            DisplayRow::Header(_) => true,
        })
        .any(|r| matches!(r, DisplayRow::Task { depth: rd, .. } if *rd == depth));

    if is_last {
        prefix.push_str("└─ ");
    } else {
        prefix.push_str("├─ ");
    }

    prefix
}

/// Truncate a string to fit within `max_width` display columns, appending "…" if truncated.
fn truncate_to_width(s: &str, max_width: usize) -> String {
    let width = s.width();
    if width <= max_width {
        return s.to_string();
    }
    if max_width == 0 {
        return String::new();
    }
    let mut result = String::new();
    let mut current_width = 0;
    let target = max_width.saturating_sub(1); // reserve 1 column for "…"
    for ch in s.chars() {
        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if current_width + cw > target {
            break;
        }
        result.push(ch);
        current_width += cw;
    }
    result.push('…');
    result
}

fn render_task_table(f: &mut Frame, area: ratatui::layout::Rect, app: &mut App) {
    let display = app.build_display_rows();
    let display_len = display.len();
    let children_map = app.children_map();

    // Compute available width for the title column:
    // inner = area.width - border_right(1) - padding_left(1) - padding_right(1)
    // title_width = inner - fixed_columns(2+4+6+14+10) - column_gaps(5×1)
    let title_col_width = (area.width as usize).saturating_sub(44);

    let header = Row::new(vec![
        Cell::from(" "),
        Cell::from("ID"),
        Cell::from("Task"),
        Cell::from("Priority"),
        Cell::from("Tags"),
        Cell::from("Sessions"),
    ])
    .style(
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    )
    .height(1);

    let rows: Vec<Row> = display
        .iter()
        .enumerate()
        .map(|(i, dr)| match dr {
            DisplayRow::Header(label) => {
                let header_text = format!("── {} ──", label);
                Row::new(vec![
                    Cell::from(""),
                    Cell::from(""),
                    Cell::from(Span::styled(
                        header_text,
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    )),
                    Cell::from(""),
                    Cell::from(""),
                    Cell::from(""),
                ])
                .height(1)
            }
            DisplayRow::Task { idx, dimmed, .. } => {
                let tv = &app.tasks[*idx];
                let task = &tv.task;
                let is_dimmed = *dimmed;

                let status_cell = Cell::from(Span::styled(
                    status_icon(task.status, app.animation_tick),
                    Style::default().fg(if is_dimmed { Color::DarkGray } else { status_color(task.status) }),
                ));

                // Build title with tree prefix and collapse indicator
                let prefix = tree_prefix(&display, i);
                let has_kids = children_map
                    .get(&Some(task.id))
                    .map_or(false, |c| !c.is_empty());
                let collapse_ind = if has_kids {
                    if app.collapsed.contains(&task.id) {
                        "▸ "
                    } else {
                        "▾ "
                    }
                } else {
                    "  "
                };
                let title_text = format!("{}{}{}", prefix, collapse_ind, task.title);
                let title_text = truncate_to_width(&title_text, title_col_width);
                let title_cell = Cell::from(Span::styled(
                    title_text,
                    Style::default().fg(if is_dimmed { Color::DarkGray } else { Color::White }),
                ));

                let priority_cell = Cell::from(Span::styled(
                    task.priority.label(),
                    Style::default().fg(if is_dimmed { Color::DarkGray } else { priority_color(task.priority) }),
                ));
                let tags_cell = Cell::from(Span::styled(
                    task.tags.join(", "),
                    Style::default().fg(Color::DarkGray),
                ));
                let has_active = tv.sessions.iter().any(|s| app.active_session_ids.contains(&s.session_id));
                let session_display = if has_active {
                    format!("▶ {}", tv.session_count)
                } else if tv.session_count == 0 {
                    String::from("—")
                } else {
                    format!("{}", tv.session_count)
                };
                let session_style = if has_active {
                    Style::default().fg(Color::Green)
                } else if tv.session_count == 0 {
                    Style::default().fg(Color::DarkGray)
                } else {
                    Style::default().fg(Color::Magenta)
                };
                let session_cell = Cell::from(Span::styled(session_display, session_style));

                let id_cell = Cell::from(Span::styled(
                    format!("{}", task.id),
                    Style::default().fg(Color::DarkGray),
                ));

                Row::new(vec![
                    status_cell,
                    id_cell,
                    title_cell,
                    priority_cell,
                    tags_cell,
                    session_cell,
                ])
                .height(1)
            }
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(2),
            Constraint::Length(4),
            Constraint::Min(20),
            Constraint::Length(6),
            Constraint::Length(14),
            Constraint::Length(10),
        ],
    )
    .header(header)
    .block(
        Block::default()
            .borders(Borders::RIGHT)
            .border_style(Style::default().fg(Color::DarkGray))
            .border_type(BorderType::Plain)
            .padding(Padding::new(1, 1, 0, 0)),
    )
    .row_highlight_style(
        Style::default()
            .bg(Color::Rgb(30, 40, 55))
            .add_modifier(Modifier::BOLD),
    );

    let mut scrollbar_state = ScrollbarState::new(display_len.saturating_sub(1))
        .position(app.table_state.selected().unwrap_or(0));

    f.render_stateful_widget(table, area, &mut app.table_state);
    f.render_stateful_widget(
        Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .style(Style::default().fg(Color::DarkGray)),
        area.inner(Margin {
            vertical: 1,
            horizontal: 0,
        }),
        &mut scrollbar_state,
    );
}

fn render_detail_panel(f: &mut Frame, area: ratatui::layout::Rect, app: &App) {
    let tv = match app.selected_task_view() {
        Some(tv) => tv,
        None => {
            let empty = Paragraph::new("No task selected")
                .style(Style::default().fg(Color::DarkGray))
                .block(Block::default().padding(Padding::new(2, 2, 1, 0)));
            f.render_widget(empty, area);
            return;
        }
    };

    let task = &tv.task;
    let mut lines: Vec<Line> = Vec::new();

    // Title
    lines.push(Line::from(Span::styled(
        &task.title,
        Style::default().fg(Color::White).bold(),
    )));
    lines.push(Line::from(""));

    // Breadcrumb path
    if task.parent_id.is_some() {
        let mut crumbs: Vec<String> = Vec::new();
        let mut current_parent = task.parent_id;
        while let Some(pid) = current_parent {
            if let Some(parent_tv) = app.tasks.iter().find(|tv| tv.task.id == pid) {
                crumbs.push(parent_tv.task.title.clone());
                current_parent = parent_tv.task.parent_id;
            } else {
                break;
            }
        }
        crumbs.reverse();
        let breadcrumb = crumbs.join(" > ");
        lines.push(Line::from(vec![
            Span::styled("path   ", Style::default().fg(Color::DarkGray)),
            Span::styled(breadcrumb, Style::default().fg(Color::DarkGray).italic()),
        ]));
        lines.push(Line::from(""));
    }

    // Status + Priority row
    lines.push(Line::from(vec![
        Span::styled("status ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("{} {}", status_icon(task.status, app.animation_tick), task.status.label()),
            Style::default().fg(status_color(task.status)),
        ),
        Span::styled("   priority ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            task.priority.label(),
            Style::default().fg(priority_color(task.priority)).bold(),
        ),
    ]));
    lines.push(Line::from(""));

    // Tags
    if !task.tags.is_empty() {
        let mut tag_spans = vec![Span::styled("tags   ", Style::default().fg(Color::DarkGray))];
        for (i, tag) in task.tags.iter().enumerate() {
            if i > 0 {
                tag_spans.push(Span::styled(" ", Style::default()));
            }
            tag_spans.push(Span::styled(
                format!(" {} ", tag),
                Style::default()
                    .fg(Color::Cyan)
                    .bg(Color::Rgb(20, 40, 50)),
            ));
        }
        lines.push(Line::from(tag_spans));
        lines.push(Line::from(""));
    }

    // Description
    if !task.description.is_empty() {
        lines.push(Line::from(Span::styled(
            "description",
            Style::default().fg(Color::DarkGray),
        )));
        lines.push(Line::from(Span::styled(
            &task.description,
            Style::default().fg(Color::White),
        )));
        lines.push(Line::from(""));
    }

    // Subtasks
    let children: Vec<&TaskView> = app
        .tasks
        .iter()
        .filter(|tv| tv.task.parent_id == Some(task.id))
        .collect();
    if !children.is_empty() {
        lines.push(Line::from(Span::styled(
            format!("subtasks ({})", children.len()),
            Style::default().fg(Color::DarkGray),
        )));
        for child in &children {
            lines.push(Line::from(vec![
                Span::styled("  ", Style::default()),
                Span::styled(
                    status_icon(child.task.status, app.animation_tick),
                    Style::default().fg(status_color(child.task.status)),
                ),
                Span::styled(
                    format!(" {}", child.task.title),
                    Style::default().fg(Color::White),
                ),
            ]));
        }
        lines.push(Line::from(""));
    }

    // Claude sessions
    let any_active = tv.sessions.iter().any(|s| app.active_session_ids.contains(&s.session_id));
    lines.push(Line::from(Span::styled(
        if any_active { "claude sessions (active)" } else { "claude sessions" },
        Style::default().fg(if any_active { Color::Green } else { Color::DarkGray }),
    )));
    if tv.sessions.is_empty() {
        lines.push(Line::from(Span::styled(
            "no linked sessions",
            Style::default().fg(Color::DarkGray).italic(),
        )));
    } else {
        for session in &tv.sessions {
            let is_active = app.active_session_ids.contains(&session.session_id);
            let (icon, color) = if is_active {
                ("▶", Color::Green)
            } else {
                ("▸", Color::Magenta)
            };
            lines.push(Line::from(vec![
                Span::styled(format!("  {} ", icon), Style::default().fg(color)),
                Span::styled(&session.session_id, Style::default().fg(color)),
                Span::styled(
                    format!("  {}", session.created_at),
                    Style::default().fg(Color::DarkGray),
                ),
            ]));
        }
    }

    let detail = Paragraph::new(Text::from(lines))
        .block(Block::default().padding(Padding::new(2, 2, 1, 0)))
        .wrap(Wrap { trim: false });

    f.render_widget(detail, area);
}

fn render_claude_pane(f: &mut Frame, area: Rect, app: &App) {
    let selected_id = app.selected_task_id();
    let pane = selected_id.and_then(|id| app.claude_panes.get(&id));

    let border_color = if app.claude_focus && pane.is_some() {
        Color::Cyan
    } else {
        Color::DarkGray
    };

    let pane_count = app.claude_panes.len();
    let scrollback_offset = pane
        .and_then(|p| p.parser.lock().ok().map(|parser| parser.screen().scrollback()))
        .unwrap_or(0);
    let scroll_indicator = if scrollback_offset > 0 {
        format!(" [scrolled +{}]", scrollback_offset)
    } else {
        String::new()
    };
    let title = if let Some(p) = pane {
        format!(
            " Claude - Task #{} ({} active){} ",
            p.task_id, pane_count, scroll_indicator
        )
    } else {
        format!(" Claude ({} active) ", pane_count)
    };

    let block = Block::default()
        .title(Span::styled(
            title,
            Style::default().fg(border_color).bold(),
        ))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border_color));

    let inner_area = block.inner(area);
    f.render_widget(block, area);

    if let Some(p) = pane {
        // Sync PTY dimensions to match the rendered area — prevents text cutoff
        // when the layout changes (e.g., first spawn uses stale full-width area,
        // but actual panel is only 55% wide after the split).
        {
            let needs_resize = p.parser.lock().ok().map_or(false, |parser| {
                let (rows, cols) = parser.screen().size();
                cols != inner_area.width || rows != inner_area.height
            });
            if needs_resize && inner_area.width > 0 && inner_area.height > 0 {
                p.resize(inner_area.width, inner_area.height);
            }
        }
        if let Ok(parser) = p.parser.lock() {
            let screen = parser.screen();
            let pseudo_term = tui_term::widget::PseudoTerminal::new(screen);
            f.render_widget(pseudo_term, inner_area);
        }
    } else {
        // Empty state — no active session for this task
        let hint = Paragraph::new(Line::from(vec![
            Span::styled("No active session", Style::default().fg(Color::DarkGray)),
            Span::styled(" — press ", Style::default().fg(Color::DarkGray)),
            Span::styled("c", Style::default().fg(Color::Cyan).bold()),
            Span::styled(" to start", Style::default().fg(Color::DarkGray)),
        ]))
        .block(Block::default().padding(Padding::new(2, 0, 1, 0)));
        f.render_widget(hint, inner_area);
    }
}

fn render_status_bar(f: &mut Frame, area: ratatui::layout::Rect, app: &App) {
    let filtered_len = app.filtered_tasks().len();
    let pos = app
        .table_state
        .selected()
        .map(|i| format!("{}/{}", i + 1, filtered_len))
        .unwrap_or_else(|| "0/0".into());

    let key_style = Style::default()
        .fg(Color::Black)
        .bg(Color::DarkGray)
        .bold();
    let label_style = Style::default().fg(Color::DarkGray);

    let has_pane_for_selected = app
        .selected_task_id()
        .is_some_and(|id| app.claude_panes.contains_key(&id));

    let bar = if app.claude_focus {
        Line::from(vec![
            Span::styled(" Alt+←/→ ", key_style),
            Span::styled(" focus tasks ", label_style),
            Span::styled(" d ", key_style),
            Span::styled(" detail ", label_style),
            Span::styled(
                format!("  {} ", pos),
                label_style,
            ),
        ])
    } else if !app.claude_panes.is_empty() && !app.show_detail {
        let focus_hint = if has_pane_for_selected {
            " focus Claude "
        } else {
            " focus Claude (no session) "
        };
        Line::from(vec![
            Span::styled(" Alt+←/→ ", key_style),
            Span::styled(focus_hint, label_style),
            Span::styled(" d ", key_style),
            Span::styled(" detail ", label_style),
            Span::styled(" j/k ", key_style),
            Span::styled(" nav ", label_style),
            Span::styled(" s ", key_style),
            Span::styled(" status ", label_style),
            Span::styled(" ? ", key_style),
            Span::styled(" help ", label_style),
            Span::styled(
                format!("  {} ", pos),
                label_style,
            ),
        ])
    } else {
        Line::from(vec![
            Span::styled(" j/k ", key_style),
            Span::styled(" nav ", label_style),
            Span::styled(" h/l ", key_style),
            Span::styled(" tree ", label_style),
            Span::styled(" tab ", key_style),
            Span::styled(" status ", label_style),
            Span::styled(" t ", key_style),
            Span::styled(" tag ", label_style),
            Span::styled(" g ", key_style),
            Span::styled(" group ", label_style),
            Span::styled(" o ", key_style),
            Span::styled(" sort ", label_style),
            Span::styled(" / ", key_style),
            Span::styled(" search ", label_style),
            Span::styled(" a ", key_style),
            Span::styled(" add ", label_style),
            Span::styled(" e ", key_style),
            Span::styled(" edit ", label_style),
            Span::styled(" c ", key_style),
            Span::styled(" claude ", label_style),
            Span::styled(" d ", key_style),
            Span::styled(" detail ", label_style),
            Span::styled(" ? ", key_style),
            Span::styled(" help ", label_style),
            Span::styled(" q ", key_style),
            Span::styled(" quit ", label_style),
            Span::styled(
                format!("  {} ", pos),
                label_style,
            ),
        ])
    };
    f.render_widget(Paragraph::new(bar), area);
}

fn render_input_bar(f: &mut Frame, area: ratatui::layout::Rect, app: &App) {
    let label = if app.input_parent_id.is_some() {
        "new subtask: "
    } else {
        "new task: "
    };
    let bar = Line::from(vec![
        Span::styled(
            label,
            Style::default().fg(Color::Cyan).bold(),
        ),
        Span::styled(&app.input_buffer, Style::default().fg(Color::White)),
        Span::styled("█", Style::default().fg(Color::DarkGray)),
        Span::styled(
            "  (Enter to save, Esc to cancel)",
            Style::default().fg(Color::DarkGray),
        ),
    ]);
    f.render_widget(Paragraph::new(bar), area);
    // Place terminal cursor at the input position
    #[allow(clippy::cast_possible_truncation)]
    f.set_cursor_position((
        area.x + label.len() as u16 + app.input_cursor as u16,
        area.y,
    ));
}

fn render_edit_bar(f: &mut Frame, area: ratatui::layout::Rect, app: &App) {
    let label = format!("edit {}: ", app.edit_field.label());
    let bar = Line::from(vec![
        Span::styled(
            &label,
            Style::default().fg(Color::Yellow).bold(),
        ),
        Span::styled(&app.edit_buffer, Style::default().fg(Color::White)),
        Span::styled("█", Style::default().fg(Color::DarkGray)),
        Span::styled(
            "  (Enter to save, Esc to cancel)",
            Style::default().fg(Color::DarkGray),
        ),
    ]);
    f.render_widget(Paragraph::new(bar), area);
    #[allow(clippy::cast_possible_truncation)]
    f.set_cursor_position((
        area.x + label.len() as u16 + app.edit_cursor as u16,
        area.y,
    ));
}

fn render_search_bar(f: &mut Frame, area: ratatui::layout::Rect, app: &App) {
    let label = "/";
    let bar = Line::from(vec![
        Span::styled(
            label,
            Style::default().fg(Color::Cyan).bold(),
        ),
        Span::styled(&app.search_query, Style::default().fg(Color::White)),
        Span::styled("█", Style::default().fg(Color::DarkGray)),
        Span::styled(
            "  (Enter to filter, Esc to clear)",
            Style::default().fg(Color::DarkGray),
        ),
    ]);
    f.render_widget(Paragraph::new(bar), area);
    #[allow(clippy::cast_possible_truncation)]
    f.set_cursor_position((
        area.x + label.len() as u16 + app.search_cursor as u16,
        area.y,
    ));
}

fn render_edit_picker(f: &mut Frame, app: &mut App) {
    let tv = match app.selected_task_view() {
        Some(tv) => tv,
        None => return,
    };
    let task = &tv.task;

    let fields = EditField::all();
    let values: Vec<String> = fields
        .iter()
        .map(|field| match field {
            EditField::Title => task.title.clone(),
            EditField::Priority => task.priority.label().to_string(),
            EditField::Tags => task.tags.join(", "),
            EditField::Description => {
                let d = &task.description;
                if d.len() > 30 {
                    format!("{}...", &d[..27])
                } else {
                    d.clone()
                }
            }
        })
        .collect();

    let area = f.area();
    let popup_height = (fields.len() as u16 + 3).min(area.height.saturating_sub(4));
    let popup_width = 50u16.min(area.width.saturating_sub(4));
    let popup_area = Rect {
        x: (area.width.saturating_sub(popup_width)) / 2,
        y: (area.height.saturating_sub(popup_height)) / 2,
        width: popup_width,
        height: popup_height,
    };

    f.render_widget(Clear, popup_area);

    let rows: Vec<Row> = fields
        .iter()
        .zip(values.iter())
        .map(|(field, value)| {
            let hint = if *field == EditField::Priority {
                " (Enter to cycle)"
            } else {
                ""
            };
            let val_display = if value.is_empty() {
                "(empty)".to_string()
            } else {
                value.clone()
            };
            Row::new(vec![
                Cell::from(Span::styled(
                    format!("  {:<13}", field.label()),
                    Style::default().fg(Color::Cyan),
                )),
                Cell::from(Span::styled(
                    format!("{}{}", val_display, hint),
                    Style::default().fg(Color::White),
                )),
            ])
        })
        .collect();

    let table = Table::new(rows, [Constraint::Length(15), Constraint::Min(20)])
        .block(
            Block::default()
                .title(Span::styled(
                    " Edit task ",
                    Style::default().fg(Color::Yellow).bold(),
                ))
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(Color::Yellow))
                .style(Style::default().bg(Color::Rgb(15, 15, 25))),
        )
        .row_highlight_style(
            Style::default()
                .bg(Color::Rgb(30, 40, 55))
                .add_modifier(Modifier::BOLD),
        );

    f.render_stateful_widget(table, popup_area, &mut app.edit_picker_state);
}

fn render_help_popup(f: &mut Frame) {
    let area = f.area();
    let popup_width = 54u16.min(area.width.saturating_sub(4));
    let popup_height = 33u16.min(area.height.saturating_sub(4));
    let popup_area = ratatui::layout::Rect {
        x: (area.width.saturating_sub(popup_width)) / 2,
        y: (area.height.saturating_sub(popup_height)) / 2,
        width: popup_width,
        height: popup_height,
    };

    f.render_widget(Clear, popup_area);

    let help_text = vec![
        Line::from(""),
        Line::from(vec![
            Span::styled("  j / ↓      ", Style::default().fg(Color::Cyan)),
            Span::styled("Move down", Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("  k / ↑      ", Style::default().fg(Color::Cyan)),
            Span::styled("Move up", Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("  l / Enter  ", Style::default().fg(Color::Cyan)),
            Span::styled("Expand subtasks", Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("  h          ", Style::default().fg(Color::Cyan)),
            Span::styled("Collapse / go to parent", Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("  L          ", Style::default().fg(Color::Cyan)),
            Span::styled("Expand all", Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("  H          ", Style::default().fg(Color::Cyan)),
            Span::styled("Collapse all", Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("  > / <      ", Style::default().fg(Color::Cyan)),
            Span::styled("Indent / outdent (reparent)", Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("  J / K      ", Style::default().fg(Color::Cyan)),
            Span::styled("Move task down / up", Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("  Tab        ", Style::default().fg(Color::Cyan)),
            Span::styled("Next status tab", Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("  S-Tab      ", Style::default().fg(Color::Cyan)),
            Span::styled("Previous status tab", Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("  g          ", Style::default().fg(Color::Cyan)),
            Span::styled("Cycle group-by", Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("  o          ", Style::default().fg(Color::Cyan)),
            Span::styled("Cycle sort order", Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("  /          ", Style::default().fg(Color::Cyan)),
            Span::styled("Search tasks", Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("  t          ", Style::default().fg(Color::Cyan)),
            Span::styled("Filter by tag", Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("  s          ", Style::default().fg(Color::Cyan)),
            Span::styled("Cycle status", Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("  S          ", Style::default().fg(Color::Cyan)),
            Span::styled("Toggle blocked", Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("  a          ", Style::default().fg(Color::Cyan)),
            Span::styled("Add sibling task", Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("  A          ", Style::default().fg(Color::Cyan)),
            Span::styled("Add child task", Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("  e          ", Style::default().fg(Color::Cyan)),
            Span::styled("Edit task fields", Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("  x          ", Style::default().fg(Color::Cyan)),
            Span::styled("Delete task", Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("  d          ", Style::default().fg(Color::Cyan)),
            Span::styled("Show detail panel", Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("  c          ", Style::default().fg(Color::Cyan)),
            Span::styled("Show/start Claude", Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("  C          ", Style::default().fg(Color::Cyan)),
            Span::styled("Session picker (resume)", Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("  Alt+←/→    ", Style::default().fg(Color::Cyan)),
            Span::styled("Toggle Claude focus", Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("  q / Esc    ", Style::default().fg(Color::Cyan)),
            Span::styled("Quit", Style::default().fg(Color::White)),
        ]),
        Line::from(""),
    ];

    let help = Paragraph::new(help_text).block(
        Block::default()
            .title(Span::styled(
                " Keybindings ",
                Style::default().fg(Color::Cyan).bold(),
            ))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(Color::Cyan))
            .style(Style::default().bg(Color::Rgb(15, 15, 25))),
    );

    f.render_widget(help, popup_area);
}

fn render_tag_picker(f: &mut Frame, app: &mut App) {
    let tags = app.all_tags();
    let item_count = tags.len() + 1;
    let area = f.area();
    let popup_height = (item_count as u16 + 3).min(area.height.saturating_sub(4));
    let popup_width = 30u16.min(area.width.saturating_sub(4));
    let popup_area = ratatui::layout::Rect {
        x: (area.width.saturating_sub(popup_width)) / 2,
        y: (area.height.saturating_sub(popup_height)) / 2,
        width: popup_width,
        height: popup_height,
    };

    f.render_widget(Clear, popup_area);

    let mut rows: Vec<Row> = vec![{
        let label = if app.tag_filter.is_none() {
            "  * All tags"
        } else {
            "    All tags"
        };
        Row::new(vec![Cell::from(Span::styled(
            label,
            Style::default().fg(Color::White),
        ))])
    }];

    for tag in &tags {
        let is_active = app.tag_filter.as_ref() == Some(tag);
        let prefix = if is_active { "  * " } else { "    " };
        rows.push(Row::new(vec![Cell::from(Span::styled(
            format!("{}{}", prefix, tag),
            if is_active {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default().fg(Color::White)
            },
        ))]));
    }

    let table = Table::new(rows, [Constraint::Min(1)])
        .block(
            Block::default()
                .title(Span::styled(
                    " Filter by tag ",
                    Style::default().fg(Color::Cyan).bold(),
                ))
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(Color::Cyan))
                .style(Style::default().bg(Color::Rgb(15, 15, 25))),
        )
        .row_highlight_style(
            Style::default()
                .bg(Color::Rgb(30, 40, 55))
                .add_modifier(Modifier::BOLD),
        );

    f.render_stateful_widget(table, popup_area, &mut app.tag_picker_state);
}

fn render_claude_picker(f: &mut Frame, app: &mut App) {
    let sessions = app
        .selected_task_view()
        .map(|tv| tv.sessions.clone())
        .unwrap_or_default();
    let item_count = 1 + sessions.len(); // "New session" + existing sessions
    let area = f.area();
    let popup_height = (item_count as u16 + 3).min(area.height.saturating_sub(4));
    let popup_width = 48u16.min(area.width.saturating_sub(4));
    let popup_area = Rect {
        x: (area.width.saturating_sub(popup_width)) / 2,
        y: (area.height.saturating_sub(popup_height)) / 2,
        width: popup_width,
        height: popup_height,
    };

    f.render_widget(Clear, popup_area);

    let mut rows: Vec<Row> = vec![Row::new(vec![Cell::from(Span::styled(
        "  + New session",
        Style::default().fg(Color::Cyan),
    ))])];

    // Show sessions most-recent-first
    for session in sessions.iter().rev() {
        let id_short = if session.session_id.len() > 8 {
            &session.session_id[..8]
        } else {
            &session.session_id
        };
        let display = format!("    {} - {}", session.created_at, id_short);
        rows.push(Row::new(vec![Cell::from(Span::styled(
            display,
            Style::default().fg(Color::Magenta),
        ))]));
    }

    let table = Table::new(rows, [Constraint::Min(1)])
        .block(
            Block::default()
                .title(Line::from(vec![
                    Span::styled(
                        " Claude sessions ",
                        Style::default().fg(Color::Cyan).bold(),
                    ),
                    Span::styled(
                        "x=remove ",
                        Style::default().fg(Color::DarkGray),
                    ),
                ]))
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(Color::Cyan))
                .style(Style::default().bg(Color::Rgb(15, 15, 25))),
        )
        .row_highlight_style(
            Style::default()
                .bg(Color::Rgb(30, 40, 55))
                .add_modifier(Modifier::BOLD),
        );

    f.render_stateful_widget(table, popup_area, &mut app.claude_picker_state);
}

fn render_delete_confirm(f: &mut Frame, app: &App) {
    let (title, desc_count) = app
        .selected_task_view()
        .map(|tv| {
            let count = app.db.descendant_count(tv.task.id).unwrap_or(0);
            (tv.task.title.as_str(), count)
        })
        .unwrap_or(("this task", 0));

    let area = f.area();
    let popup_width = 55u16.min(area.width.saturating_sub(4));
    let popup_height = 5u16;
    let popup_area = ratatui::layout::Rect {
        x: (area.width.saturating_sub(popup_width)) / 2,
        y: (area.height.saturating_sub(popup_height)) / 2,
        width: popup_width,
        height: popup_height,
    };

    f.render_widget(Clear, popup_area);

    let mut display_title = title.to_string();
    let max_len = (popup_width as usize).saturating_sub(10);
    if display_title.len() > max_len {
        display_title.truncate(max_len.saturating_sub(3));
        display_title.push_str("...");
    }

    let delete_msg = if desc_count > 0 {
        format!(
            "  Delete {} and {} subtask{}?",
            display_title,
            desc_count,
            if desc_count == 1 { "" } else { "s" }
        )
    } else {
        format!("  Delete {}?", display_title)
    };

    let text = vec![
        Line::from(""),
        Line::from(Span::styled(
            delete_msg,
            Style::default().fg(Color::Red),
        )),
        Line::from(vec![
            Span::styled("  Press ", Style::default().fg(Color::DarkGray)),
            Span::styled("y", Style::default().fg(Color::Red).bold()),
            Span::styled(
                " to confirm, any other key to cancel",
                Style::default().fg(Color::DarkGray),
            ),
        ]),
    ];

    let popup = Paragraph::new(text).block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(Color::Red))
            .style(Style::default().bg(Color::Rgb(25, 10, 10))),
    );

    f.render_widget(popup, popup_area);
}

fn render_quit_confirm(f: &mut Frame, app: &App) {
    let running = app.claude_panes.values().filter(|p| !p.exited).count();

    let area = f.area();
    let popup_width = 55u16.min(area.width.saturating_sub(4));
    let popup_height = 5u16;
    let popup_area = Rect {
        x: (area.width.saturating_sub(popup_width)) / 2,
        y: (area.height.saturating_sub(popup_height)) / 2,
        width: popup_width,
        height: popup_height,
    };

    f.render_widget(Clear, popup_area);

    let text = vec![
        Line::from(""),
        Line::from(Span::styled(
            format!(
                "  {} Claude session{} still running — kill and quit?",
                running,
                if running == 1 { "" } else { "s" }
            ),
            Style::default().fg(Color::Red),
        )),
        Line::from(vec![
            Span::styled("  Press ", Style::default().fg(Color::DarkGray)),
            Span::styled("y", Style::default().fg(Color::Red).bold()),
            Span::styled(
                " to confirm, any other key to cancel",
                Style::default().fg(Color::DarkGray),
            ),
        ]),
    ];

    let popup = Paragraph::new(text).block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(Color::Red))
            .style(Style::default().bg(Color::Rgb(25, 10, 10))),
    );

    f.render_widget(popup, popup_area);
}

// ── Main ────────────────────────────────────────────────────────────────────

fn main() -> io::Result<()> {
    match std::env::args().nth(1).as_deref() {
        Some("mcp") => { mcp::run(); return Ok(()); }
        Some("serve") => { return web::run(); }
        _ => {}
    }

    let db = Db::open().expect("failed to open database");

    stdout()
        .execute(EnterAlternateScreen)?
        .execute(EnableMouseCapture)?;
    enable_raw_mode()?;

    let mut terminal = Terminal::new(ratatui::backend::CrosstermBackend::new(stdout()))?;
    let db_path = db.path.clone();
    let mut app = App::new(db);
    let mut last_mtime = std::fs::metadata(&db_path)
        .and_then(|m| m.modified())
        .ok();
    let mut last_session_check = std::time::Instant::now();

    loop {
        terminal.draw(|f| ui(f, &mut app))?;

        // Advance animation tick every 250ms for spinning status icons
        if app.last_animation.elapsed() >= std::time::Duration::from_millis(250) {
            app.animation_tick = app.animation_tick.wrapping_add(1);
            app.last_animation = std::time::Instant::now();
        }

        // Faster refresh when any Claude pane is active for smooth rendering,
        // or when we have in-progress tasks that need animation
        let has_in_progress = app.tasks.iter().any(|tv| tv.task.status == Status::InProgress);
        let poll_timeout = if !app.claude_panes.is_empty() {
            std::time::Duration::from_millis(16)
        } else if has_in_progress {
            std::time::Duration::from_millis(250)
        } else {
            std::time::Duration::from_secs(1)
        };

        if event::poll(poll_timeout)? {
            match event::read()? {
                Event::Key(key) => {
                    if key.kind == KeyEventKind::Press {
                        // Alt+Left/Right or F2 toggle focus between task list and Claude pane
                        let is_focus_toggle = key.code == KeyCode::F(2)
                            || (key.modifiers.contains(crossterm::event::KeyModifiers::ALT)
                                && matches!(key.code, KeyCode::Left | KeyCode::Right));
                        if is_focus_toggle {
                            if !app.claude_panes.is_empty() {
                                app.claude_focus = !app.claude_focus;
                                // Auto-release focus if no pane for selected task
                                if app.claude_focus {
                                    let has_pane = app
                                        .selected_task_id()
                                        .is_some_and(|id| app.claude_panes.contains_key(&id));
                                    if !has_pane {
                                        app.claude_focus = false;
                                    }
                                }
                            }
                        } else if app.claude_focus {
                            // Forward all keys to PTY for the visible pane
                            // Except: q/Esc closes pane when Claude has exited
                            let selected_id = app.selected_task_id();
                            let visible_exited = selected_id
                                .and_then(|id| app.claude_panes.get_mut(&id))
                                .is_some_and(|p| p.try_wait());
                            if visible_exited {
                                match key.code {
                                    KeyCode::Char('q') | KeyCode::Esc => {
                                        app.close_claude_pane();
                                    }
                                    _ => {}
                                }
                            } else if let Some(pane) = selected_id
                                .and_then(|id| app.claude_panes.get_mut(&id))
                            {
                                let bytes = pty::key_to_bytes(&key);
                                if !bytes.is_empty() {
                                    pane.write(&bytes);
                                }
                            } else {
                                // No pane for selected task — release focus
                                app.claude_focus = false;
                            }
                        } else {
                            // Don't pass Ctrl-modified character keys to TUI handlers —
                            // e.g. Ctrl+C should not trigger the 'c' keybind
                            let dominated = matches!(key.code, KeyCode::Char(_))
                                && key.modifiers.contains(
                                    crossterm::event::KeyModifiers::CONTROL,
                                );
                            if !dominated {
                                app.handle_key(key.code);
                            }
                        }
                    }
                }
                Event::Mouse(mouse) => {
                    // Check if scroll event is over the Claude pane area
                    let over_claude = !app.claude_panes.is_empty()
                        && mouse.column >= app.claude_pane_area.x
                        && mouse.column
                            < app.claude_pane_area.x + app.claude_pane_area.width
                        && mouse.row >= app.claude_pane_area.y
                        && mouse.row
                            < app.claude_pane_area.y + app.claude_pane_area.height;

                    if over_claude {
                        match mouse.kind {
                            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                                let scroll_lines: usize = 3;
                                if let Some(pane) = app
                                    .selected_task_id()
                                    .and_then(|id| app.claude_panes.get(&id))
                                {
                                    if let Ok(mut parser) = pane.parser.lock() {
                                        let screen = parser.screen_mut();
                                        let current = screen.scrollback();
                                        match mouse.kind {
                                            MouseEventKind::ScrollUp => {
                                                screen
                                                    .set_scrollback(current + scroll_lines);
                                            }
                                            MouseEventKind::ScrollDown => {
                                                screen.set_scrollback(
                                                    current.saturating_sub(scroll_lines),
                                                );
                                            }
                                            _ => {}
                                        }
                                    }
                                }
                            }
                            _ => {}
                        }
                    } else if !app.claude_focus {
                        app.handle_mouse(mouse.kind, mouse.column, mouse.row);
                    }
                }
                Event::Resize(_cols, _rows) => {
                    if !app.claude_panes.is_empty() {
                        let area = app.claude_pane_area;
                        let inner_cols = if area.width > 2 { area.width - 2 } else { 1 };
                        let inner_rows = if area.height > 2 { area.height - 2 } else { 1 };
                        for pane in app.claude_panes.values() {
                            pane.resize(inner_cols, inner_rows);
                        }
                    }
                }
                _ => {}
            }
        }

        // Check if any Claude processes have exited
        for pane in app.claude_panes.values_mut() {
            pane.try_wait();
        }
        // If the visible pane exited, release focus
        if app.claude_focus {
            let visible_exited = app
                .selected_task_id()
                .and_then(|id| app.claude_panes.get(&id))
                .is_some_and(|p| p.exited);
            if visible_exited {
                app.claude_focus = false;
            }
        }

        // Reload if DB was modified externally
        let current_mtime = std::fs::metadata(&db_path)
            .and_then(|m| m.modified())
            .ok();
        if current_mtime != last_mtime {
            last_mtime = current_mtime;
            let selected_id = app.selected_task_view().map(|tv| tv.task.id);
            app.reload_tasks();
            // Restore selection by task ID
            if let Some(id) = selected_id {
                let display = app.build_display_rows();
                if let Some(pos) = display.iter().position(|dr| {
                    matches!(dr, DisplayRow::Task { idx, .. } if app.tasks[*idx].task.id == id)
                }) {
                    app.table_state.select(Some(pos));
                }
            }
        }

        // Refresh active Claude session detection every 2 seconds
        if last_session_check.elapsed() >= std::time::Duration::from_secs(2) {
            app.refresh_active_sessions();
            last_session_check = std::time::Instant::now();
        }

        if app.quit {
            break;
        }
    }

    // Clean up all Claude panes
    for (_, mut pane) in app.claude_panes.drain() {
        pane.kill();
    }

    disable_raw_mode()?;
    stdout()
        .execute(DisableMouseCapture)?
        .execute(LeaveAlternateScreen)?;
    Ok(())
}
