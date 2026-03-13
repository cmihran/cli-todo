mod db;

use crate::db::{Db, Priority, Status, Task};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
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
    show_detail: bool,
    show_help: bool,
    quit: bool,
}

// ── App logic ───────────────────────────────────────────────────────────────

impl App {
    fn new(db: Db) -> Self {
        let mut table_state = TableState::default();
        table_state.select(Some(0));
        let mut app = App {
            db,
            tasks: vec![],
            table_state,
            active_tab: ActiveTab::All,
            show_detail: true,
            show_help: false,
            quit: false,
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
            .collect()
    }

    fn selected_task_view(&self) -> Option<&TaskView> {
        let filtered = self.filtered_tasks();
        self.table_state
            .selected()
            .and_then(|i| filtered.get(i).copied())
    }

    fn handle_key(&mut self, code: KeyCode) {
        if self.show_help {
            self.show_help = false;
            return;
        }
        let filtered_len = self.filtered_tasks().len();
        match code {
            KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
            KeyCode::Char('j') | KeyCode::Down => {
                if filtered_len > 0 {
                    let i = self.table_state.selected().unwrap_or(0);
                    self.table_state.select(Some((i + 1) % filtered_len));
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if filtered_len > 0 {
                    let i = self.table_state.selected().unwrap_or(0);
                    self.table_state
                        .select(Some(i.checked_sub(1).unwrap_or(filtered_len - 1)));
                }
            }
            KeyCode::Tab => {
                self.active_tab = match self.active_tab {
                    ActiveTab::All => ActiveTab::Active,
                    ActiveTab::Active => ActiveTab::Blocked,
                    ActiveTab::Blocked => ActiveTab::Done,
                    ActiveTab::Done => ActiveTab::All,
                };
                self.table_state.select(Some(0));
            }
            KeyCode::BackTab => {
                self.active_tab = match self.active_tab {
                    ActiveTab::All => ActiveTab::Done,
                    ActiveTab::Active => ActiveTab::All,
                    ActiveTab::Blocked => ActiveTab::Active,
                    ActiveTab::Done => ActiveTab::Blocked,
                };
                self.table_state.select(Some(0));
            }
            KeyCode::Char('d') => self.show_detail = !self.show_detail,
            KeyCode::Char('?') => self.show_help = true,
            _ => {}
        }
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

    render_header(f, outer[0]);
    render_tabs(f, outer[1], app);
    render_body(f, outer[2], app);
    render_status_bar(f, outer[3], app);

    if app.show_help {
        render_help_popup(f);
    }
}

fn render_header(f: &mut Frame, area: ratatui::layout::Rect) {
    let title = vec![
        Span::styled("  cli", Style::default().fg(Color::Cyan).bold()),
        Span::styled("-", Style::default().fg(Color::DarkGray)),
        Span::styled("todo", Style::default().fg(Color::White).bold()),
        Span::styled("  ", Style::default()),
        Span::styled(
            "task tracker for developers",
            Style::default().fg(Color::DarkGray),
        ),
    ];
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
                .filter(|tv| tv.task.status == Status::InProgress || tv.task.status == Status::Todo)
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
        .divider(Span::styled(" │ ", Style::default().fg(Color::DarkGray)))
        .padding(" ", " ");

    f.render_widget(tabs, area);
}

fn render_body(f: &mut Frame, area: ratatui::layout::Rect, app: &mut App) {
    if app.show_detail {
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
            .split(area);
        render_task_table(f, chunks[0], app);
        render_detail_panel(f, chunks[1], app);
    } else {
        render_task_table(f, area, app);
    }
}

fn render_task_table(f: &mut Frame, area: ratatui::layout::Rect, app: &mut App) {
    let filtered: Vec<&TaskView> = app
        .tasks
        .iter()
        .filter(|tv| app.active_tab.filter(tv.task.status))
        .collect();
    let filtered_len = filtered.len();

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

    let rows: Vec<Row> = filtered
        .iter()
        .map(|tv| {
            let task = &tv.task;
            let status_cell = Cell::from(Span::styled(
                status_icon(task.status),
                Style::default().fg(status_color(task.status)),
            ));
            let title_cell = Cell::from(Span::styled(
                task.title.as_str(),
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

    let mut scrollbar_state = ScrollbarState::new(filtered_len.saturating_sub(1))
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
    lines.push(Line::from(Span::styled(
        "description",
        Style::default().fg(Color::DarkGray),
    )));
    lines.push(Line::from(Span::styled(
        &task.description,
        Style::default().fg(Color::White),
    )));
    lines.push(Line::from(""));

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
        Span::styled(" navigate ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            " tab ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::DarkGray)
                .bold(),
        ),
        Span::styled(" filter ", Style::default().fg(Color::DarkGray)),
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

fn render_help_popup(f: &mut Frame) {
    let area = f.area();
    let popup_width = 50u16.min(area.width.saturating_sub(4));
    let popup_height = 14u16.min(area.height.saturating_sub(4));
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
            Span::styled("  j / ↓    ", Style::default().fg(Color::Cyan)),
            Span::styled("Move down", Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("  k / ↑    ", Style::default().fg(Color::Cyan)),
            Span::styled("Move up", Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("  Tab      ", Style::default().fg(Color::Cyan)),
            Span::styled("Next filter tab", Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("  S-Tab    ", Style::default().fg(Color::Cyan)),
            Span::styled("Previous filter tab", Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("  d        ", Style::default().fg(Color::Cyan)),
            Span::styled("Toggle detail panel", Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("  Enter    ", Style::default().fg(Color::Cyan)),
            Span::styled("Open task (todo)", Style::default().fg(Color::DarkGray)),
        ]),
        Line::from(vec![
            Span::styled("  a        ", Style::default().fg(Color::Cyan)),
            Span::styled("Add task (todo)", Style::default().fg(Color::DarkGray)),
        ]),
        Line::from(vec![
            Span::styled("  s        ", Style::default().fg(Color::Cyan)),
            Span::styled(
                "Start Claude session (todo)",
                Style::default().fg(Color::DarkGray),
            ),
        ]),
        Line::from(vec![
            Span::styled("  q / Esc  ", Style::default().fg(Color::Cyan)),
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

// ── Main ────────────────────────────────────────────────────────────────────

fn main() -> io::Result<()> {
    let db = Db::open().expect("failed to open database");

    // Seed sample data on first run
    if db.is_empty().unwrap_or(false) {
        db.seed_sample_data().expect("failed to seed sample data");
    }

    stdout().execute(EnterAlternateScreen)?;
    enable_raw_mode()?;

    let mut terminal = Terminal::new(ratatui::backend::CrosstermBackend::new(stdout()))?;
    let mut app = App::new(db);

    loop {
        terminal.draw(|f| ui(f, &mut app))?;

        if let Event::Key(key) = event::read()? {
            if key.kind == KeyEventKind::Press {
                app.handle_key(key.code);
            }
        }

        if app.quit {
            break;
        }
    }

    disable_raw_mode()?;
    stdout().execute(LeaveAlternateScreen)?;
    Ok(())
}
