mod db;
mod mcp;

use crate::db::{Db, Priority, Status, Task};
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind,
        MouseEventKind,
    },
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use ratatui::{
    layout::{Constraint, Direction, Layout, Margin},
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

// ── UI helpers for Status/Priority colors ───────────────────────────────────

fn status_color(s: Status) -> Color {
    match s {
        Status::Todo => Color::DarkGray,
        Status::InProgress => Color::Yellow,
        Status::Done => Color::Green,
        Status::Blocked => Color::Red,
    }
}

fn status_icon(s: Status) -> &'static str {
    match s {
        Status::Todo => "○",
        Status::InProgress => "◐",
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
}

/// A row in the display list — either a group header or a task reference.
enum DisplayRow {
    Header(String),
    Task { idx: usize, depth: usize },
}

#[derive(Clone, Copy, PartialEq)]
enum ActiveTab {
    All,
    Active,
    Blocked,
    Done,
}

impl ActiveTab {
    fn index(self) -> usize {
        match self {
            ActiveTab::All => 0,
            ActiveTab::Active => 1,
            ActiveTab::Blocked => 2,
            ActiveTab::Done => 3,
        }
    }

    fn filter(self, status: Status) -> bool {
        match self {
            ActiveTab::All => true,
            ActiveTab::Active => status == Status::InProgress || status == Status::Todo,
            ActiveTab::Blocked => status == Status::Blocked,
            ActiveTab::Done => status == Status::Done,
        }
    }
}

struct TaskView {
    task: Task,
    session_count: usize,
    sessions: Vec<String>,
}

struct App {
    db: Db,
    tasks: Vec<TaskView>,
    table_state: TableState,
    active_tab: ActiveTab,
    group_by: GroupBy,
    tag_filter: Option<String>,
    show_tag_picker: bool,
    tag_picker_state: TableState,
    show_detail: bool,
    show_help: bool,
    confirm_delete: bool,
    collapsed: HashSet<i64>,
    quit: bool,
    // Inline task creation
    input_mode: bool,
    input_buffer: String,
    input_cursor: usize,
    input_parent_id: Option<i64>,
    // Layout areas for mouse hit-testing
    table_area: ratatui::layout::Rect,
    tab_area: ratatui::layout::Rect,
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
            tag_filter: None,
            show_tag_picker: false,
            tag_picker_state,
            show_detail: true,
            show_help: false,
            confirm_delete: false,
            collapsed: HashSet::new(),
            quit: false,
            input_mode: false,
            input_buffer: String::new(),
            input_cursor: 0,
            input_parent_id: None,
            table_area: zero_rect,
            tab_area: zero_rect,
        };
        app.reload_tasks();
        app
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

    /// Depth-first traversal of the task tree, respecting collapsed state.
    fn tree_walk(&self) -> Vec<(usize, usize)> {
        let children = self.children_map();
        let mut result = Vec::new();
        let mut visited = HashSet::new();

        fn walk(
            parent_id: Option<i64>,
            depth: usize,
            children: &HashMap<Option<i64>, Vec<usize>>,
            tasks: &[TaskView],
            collapsed: &HashSet<i64>,
            visited: &mut HashSet<i64>,
            result: &mut Vec<(usize, usize)>,
        ) {
            if let Some(kids) = children.get(&parent_id) {
                for &idx in kids {
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
        let has_filter = self.active_tab != ActiveTab::All || self.tag_filter.is_some();

        if self.group_by == GroupBy::None {
            let tree = self.tree_walk();
            return tree
                .into_iter()
                .filter(|(idx, _)| {
                    let tv = &self.tasks[*idx];
                    self.active_tab.filter(tv.task.status)
                        && match &self.tag_filter {
                            None => true,
                            Some(tag) => tv.task.tags.contains(tag),
                        }
                })
                .map(|(idx, depth)| DisplayRow::Task {
                    idx,
                    // When filters are active, flatten to avoid orphaned children
                    depth: if has_filter { 0 } else { depth },
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
            GroupBy::Status => vec!["IN PROGRESS", "TODO", "BLOCKED", "DONE"]
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
                    rows.push(DisplayRow::Task { idx, depth: 0 });
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

    // ── Key handling ────────────────────────────────────────────────────────

    fn handle_key(&mut self, code: KeyCode) {
        if self.input_mode {
            self.handle_input_key(code);
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
        if self.show_help {
            self.show_help = false;
            return;
        }
        let display = self.build_display_rows();
        let display_len = display.len();
        match code {
            KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
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
                    ActiveTab::Active => ActiveTab::Blocked,
                    ActiveTab::Blocked => ActiveTab::Done,
                    ActiveTab::Done => ActiveTab::All,
                };
                self.select_first_task();
            }
            KeyCode::BackTab => {
                self.active_tab = match self.active_tab {
                    ActiveTab::All => ActiveTab::Done,
                    ActiveTab::Active => ActiveTab::All,
                    ActiveTab::Blocked => ActiveTab::Active,
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
            KeyCode::Char('d') => self.show_detail = !self.show_detail,
            KeyCode::Char('?') => self.show_help = true,
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
                                2 => ActiveTab::Blocked,
                                3 => ActiveTab::Done,
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
    if app.input_mode {
        render_input_bar(f, outer[3], app);
    } else {
        render_status_bar(f, outer[3], app);
    }

    if app.show_help {
        render_help_popup(f);
    }
    if app.confirm_delete {
        render_delete_confirm(f, app);
    }
    if app.show_tag_picker {
        render_tag_picker(f, app);
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

fn render_body(f: &mut Frame, area: ratatui::layout::Rect, app: &mut App) {
    if app.show_detail {
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
            .split(area);
        app.table_area = chunks[0];
        render_task_table(f, chunks[0], app);
        render_detail_panel(f, chunks[1], app);
    } else {
        app.table_area = area;
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

fn render_task_table(f: &mut Frame, area: ratatui::layout::Rect, app: &mut App) {
    let display = app.build_display_rows();
    let display_len = display.len();
    let children_map = app.children_map();

    let header = Row::new(vec![
        Cell::from(" "),
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
            DisplayRow::Task { idx, .. } => {
                let tv = &app.tasks[*idx];
                let task = &tv.task;

                let status_cell = Cell::from(Span::styled(
                    status_icon(task.status),
                    Style::default().fg(status_color(task.status)),
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
                let title_cell = Cell::from(Span::styled(
                    title_text,
                    Style::default().fg(Color::White),
                ));

                let priority_cell = Cell::from(Span::styled(
                    task.priority.label(),
                    Style::default().fg(priority_color(task.priority)),
                ));
                let tags_cell = Cell::from(Span::styled(
                    task.tags.join(", "),
                    Style::default().fg(Color::DarkGray),
                ));
                let session_count = if tv.session_count == 0 {
                    String::from("—")
                } else {
                    format!("{}", tv.session_count)
                };
                let session_cell = Cell::from(Span::styled(
                    session_count,
                    if tv.session_count == 0 {
                        Style::default().fg(Color::DarkGray)
                    } else {
                        Style::default().fg(Color::Magenta)
                    },
                ));

                Row::new(vec![
                    status_cell,
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
            format!("{} {}", status_icon(task.status), task.status.label()),
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
                    status_icon(child.task.status),
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
    lines.push(Line::from(Span::styled(
        "claude sessions",
        Style::default().fg(Color::DarkGray),
    )));
    if tv.sessions.is_empty() {
        lines.push(Line::from(Span::styled(
            "no linked sessions",
            Style::default().fg(Color::DarkGray).italic(),
        )));
    } else {
        for session in &tv.sessions {
            lines.push(Line::from(vec![
                Span::styled("  ▸ ", Style::default().fg(Color::Magenta)),
                Span::styled(session.as_str(), Style::default().fg(Color::Magenta)),
            ]));
        }
    }

    let detail = Paragraph::new(Text::from(lines))
        .block(Block::default().padding(Padding::new(2, 2, 1, 0)))
        .wrap(Wrap { trim: false });

    f.render_widget(detail, area);
}

fn render_status_bar(f: &mut Frame, area: ratatui::layout::Rect, app: &App) {
    let filtered_len = app.filtered_tasks().len();
    let pos = app
        .table_state
        .selected()
        .map(|i| format!("{}/{}", i + 1, filtered_len))
        .unwrap_or_else(|| "0/0".into());

    let bar = Line::from(vec![
        Span::styled(
            " j/k ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::DarkGray)
                .bold(),
        ),
        Span::styled(" nav ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            " h/l ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::DarkGray)
                .bold(),
        ),
        Span::styled(" tree ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            " tab ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::DarkGray)
                .bold(),
        ),
        Span::styled(" status ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            " t ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::DarkGray)
                .bold(),
        ),
        Span::styled(" tag ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            " g ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::DarkGray)
                .bold(),
        ),
        Span::styled(" group ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            " a ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::DarkGray)
                .bold(),
        ),
        Span::styled(" add ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            " d ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::DarkGray)
                .bold(),
        ),
        Span::styled(" detail ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            " ? ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::DarkGray)
                .bold(),
        ),
        Span::styled(" help ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            " q ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::DarkGray)
                .bold(),
        ),
        Span::styled(" quit ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("  {} ", pos),
            Style::default().fg(Color::DarkGray),
        ),
    ]);
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

fn render_help_popup(f: &mut Frame) {
    let area = f.area();
    let popup_width = 54u16.min(area.width.saturating_sub(4));
    let popup_height = 24u16.min(area.height.saturating_sub(4));
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
            Span::styled("  x          ", Style::default().fg(Color::Cyan)),
            Span::styled("Delete task", Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("  d          ", Style::default().fg(Color::Cyan)),
            Span::styled("Toggle detail panel", Style::default().fg(Color::White)),
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

// ── Main ────────────────────────────────────────────────────────────────────

fn main() -> io::Result<()> {
    if std::env::args().nth(1).as_deref() == Some("mcp") {
        mcp::run();
        return Ok(());
    }

    let db = Db::open().expect("failed to open database");

    stdout()
        .execute(EnterAlternateScreen)?
        .execute(EnableMouseCapture)?;
    enable_raw_mode()?;

    let mut terminal = Terminal::new(ratatui::backend::CrosstermBackend::new(stdout()))?;
    let mut app = App::new(db);

    loop {
        terminal.draw(|f| ui(f, &mut app))?;

        match event::read()? {
            Event::Key(key) => {
                if key.kind == KeyEventKind::Press {
                    app.handle_key(key.code);
                }
            }
            Event::Mouse(mouse) => {
                app.handle_mouse(mouse.kind, mouse.column, mouse.row);
            }
            _ => {}
        }

        if app.quit {
            break;
        }
    }

    disable_raw_mode()?;
    stdout()
        .execute(DisableMouseCapture)?
        .execute(LeaveAlternateScreen)?;
    Ok(())
}
