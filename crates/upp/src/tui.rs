// SPDX-License-Identifier: Apache-2.0
//! `upp watch`: live view of every table. Reads the shared store, so it works the
//! same next to `upp run`, `uppd` or `upp mcp`.

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Borders, Cell, Paragraph, Row, Table, TableState, Wrap};
use ratatui::Frame;
use std::path::Path;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use upp_core::store::{now_ms, EventRow, RallyRow, Store, TableRow};

struct App {
    run: Option<String>,
    tables: Vec<TableRow>,
    events: Vec<(EventRow, String)>,
    last_event: i64,
    selected: usize,
    rallies: Vec<RallyRow>,
    started: Instant,
    tick: u64,
    done_since: Option<Instant>,
    flash: Option<(String, Instant)>,
}

const TABLE_W: usize = 13; // width of the ping-pong lane

fn status_style(s: &str) -> Style {
    match s {
        "running" => Style::new().fg(Color::Yellow),
        "awaiting_server" => Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        "point" => Style::new().fg(Color::Green).add_modifier(Modifier::BOLD),
        "lost" | "error" => Style::new().fg(Color::Red).add_modifier(Modifier::BOLD),
        "expired" => Style::new().fg(Color::Magenta),
        "queued" => Style::new().fg(Color::Blue),
        _ => Style::new().fg(Color::DarkGray),
    }
}

/// The lane: server paddle on the left, player paddle on the right, the ball in between.
fn lane(t: &TableRow, tick: u64) -> Line<'static> {
    let inner = TABLE_W - 2;
    let mut cells: Vec<Span> = vec![Span::raw(" "); inner];
    let net = inner / 2;
    cells[net] = Span::styled("┊", Style::new().fg(Color::DarkGray));
    let ball = |c: &str, col: Color| Span::styled(c.to_string(), Style::new().fg(col).bold());
    match (t.status.as_str(), t.phase.as_str()) {
        ("point", _) => {
            return Line::from(vec![Span::styled(
                format!("{:^w$}", "✔ point", w = TABLE_W),
                Style::new().fg(Color::Green).bold(),
            )])
        }
        ("lost", _) | ("error", _) => {
            return Line::from(vec![Span::styled(
                format!("{:^w$}", format!("✘ {}", t.status), w = TABLE_W),
                Style::new().fg(Color::Red).bold(),
            )])
        }
        ("expired", _) | ("stopped", _) | ("skipped", _) => {
            return Line::from(vec![Span::styled(
                format!("{:^w$}", t.status, w = TABLE_W),
                Style::new().fg(Color::DarkGray),
            )])
        }
        ("queued", _) => {
            return Line::from(vec![Span::styled(
                format!("{:^w$}", "· waiting ·", w = TABLE_W),
                Style::new().fg(Color::Blue),
            )])
        }
        (_, "player") => {
            // Rally in progress: the ball flies across and back.
            let period = (inner as u64 - 1) * 2;
            let p = (tick / 2 + t.id.len() as u64 * 3) % period;
            let pos = if p < inner as u64 { p } else { period - p } as usize;
            cells[pos.min(inner - 1)] = ball("●", Color::Yellow);
        }
        (_, "referee") => {
            let c = if tick % 6 < 3 { "◆" } else { "◇" };
            cells[net] = ball(c, Color::Cyan);
        }
        (_, "server") => {
            let c = if tick % 6 < 3 { "●" } else { "○" };
            cells[0] = ball(c, Color::Cyan);
        }
        _ => {}
    }
    let mut v = vec![Span::styled("▌", Style::new().fg(Color::LightBlue))];
    v.extend(cells);
    v.push(Span::styled("▐", Style::new().fg(Color::LightMagenta)));
    Line::from(v)
}

fn trail(t: &TableRow) -> Line<'static> {
    let mut v: Vec<Span> = t
        .trail
        .chars()
        .map(|c| match c {
            '*' => Span::styled("●", Style::new().fg(Color::Green)),
            '!' => Span::styled("●", Style::new().fg(Color::Magenta)),
            _ => Span::styled("●", Style::new().fg(Color::Red)),
        })
        .collect();
    let left = t.max_rallies.saturating_sub(t.trail.chars().count() as u32);
    for _ in 0..left {
        v.push(Span::styled("·", Style::new().fg(Color::DarkGray)));
    }
    Line::from(v)
}

fn gauge(t: &TableRow, now: i64) -> Line<'static> {
    let el = t.elapsed_ms(now) as f64 / 1000.0;
    let frac = if t.wall_secs > 0 {
        (el / t.wall_secs as f64).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let n = (frac * 5.0).round() as usize;
    let col = if frac > 0.8 { Color::Red } else { Color::Green };
    Line::from(vec![
        Span::raw(format!("{:>5} ", fmt_dur(el))),
        Span::styled("▮".repeat(n), Style::new().fg(col)),
        Span::styled("▯".repeat(5 - n), Style::new().fg(Color::DarkGray)),
    ])
}

fn fmt_dur(s: f64) -> String {
    let s = s.max(0.0) as u64;
    if s >= 3600 {
        format!("{}h{:02}", s / 3600, (s / 60) % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

fn fmt_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1e6)
    } else if n >= 1000 {
        format!("{:.1}k", n as f64 / 1e3)
    } else {
        n.to_string()
    }
}

impl App {
    fn refresh(&mut self, store: &Store) {
        if let Ok(mut t) = store.tables(self.run.as_deref(), false) {
            t.reverse(); // creation order
            if self.run.is_none() {
                // --all: active first, then the most recent ended ones.
                t.sort_by_key(|x| x.is_terminal());
                t.truncate(60);
            }
            self.tables = t;
        }
        if self.selected >= self.tables.len() {
            self.selected = self.tables.len().saturating_sub(1);
        }
        if self.last_event == 0 {
            self.last_event = store
                .last_event_id()
                .unwrap_or(0)
                .saturating_sub(200)
                .max(0);
        }
        if let Ok(evs) = store.events_since(self.last_event, 500) {
            for e in evs {
                self.last_event = e.id;
                if let Some(t) = self.tables.iter().find(|t| t.id == e.table_id) {
                    let name = t.name.clone();
                    self.events.push((e, name));
                }
            }
            let n = self.events.len();
            if n > 300 {
                self.events.drain(..n - 300);
            }
        }
        if let Some(t) = self.tables.get(self.selected) {
            self.rallies = store.rallies(&t.id).unwrap_or_default();
        }
    }
}

fn event_line(e: &EventRow, name: &str) -> Line<'static> {
    let d = &e.data;
    let s = (e.ts / 1000) % 86400;
    let ts = format!("{:02}:{:02}:{:02} ", s / 3600, (s / 60) % 60, s % 60);
    let (txt, col) = match e.kind.as_str() {
        "queued" => ("queued".to_string(), Color::Blue),
        "started" => ("table opened".to_string(), Color::White),
        "rally" => (format!("rally {} → player", d["n"]), Color::Yellow),
        "answer" => (format!("rally {} ← answer", d["n"]), Color::Yellow),
        "fault" => (
            format!(
                "rally {} fault: {}",
                d["n"],
                first_line(d["error"].as_str().unwrap_or(""))
            ),
            Color::Magenta,
        ),
        "referee" => {
            let failed: Vec<String> = d["checks"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter(|c| c["pass"].as_bool() != Some(true))
                        .map(|c| {
                            format!(
                                "{} ({})",
                                c["name"].as_str().unwrap_or(""),
                                c["summary"].as_str().unwrap_or("")
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();
            if d["passed"].as_bool() == Some(true) {
                (format!("rally {} referee PASS", d["n"]), Color::Green)
            } else {
                (
                    format!(
                        "rally {} referee FAIL: {}",
                        d["n"],
                        first_line(&failed.join("; "))
                    ),
                    Color::Red,
                )
            }
        }
        "awaiting_server" => (
            format!("rally {} → waiting for server", d["n"]),
            Color::Cyan,
        ),
        "server_answer" => (
            format!(
                "server: {} {}",
                d["answer"]["verdict"].as_str().unwrap_or(""),
                first_line(d["answer"]["note"].as_str().unwrap_or(""))
            ),
            Color::Cyan,
        ),
        "return" => (format!("rally {} ↩ ball returned", d["n"]), Color::DarkGray),
        "point" => (
            format!("POINT  {}", d["reason"].as_str().unwrap_or("")),
            Color::Green,
        ),
        "lost" | "expired" | "stopped" | "error" | "skipped" => (
            format!(
                "{}  {}",
                e.kind.to_uppercase(),
                d["reason"].as_str().unwrap_or("")
            ),
            Color::Red,
        ),
        k => (k.to_string(), Color::DarkGray),
    };
    Line::from(vec![
        Span::styled(ts, Style::new().fg(Color::DarkGray)),
        Span::styled(
            format!("{name:<14.14} "),
            Style::new().fg(Color::White).bold(),
        ),
        Span::styled(txt, Style::new().fg(col)),
    ])
}

fn first_line(s: &str) -> String {
    let l = s.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    l.chars().take(90).collect()
}

fn draw(f: &mut Frame, app: &App) {
    let now = now_ms();
    let area = f.area();
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length((app.tables.len() as u16 + 3).clamp(5, 18)),
            Constraint::Min(8),
            Constraint::Length(9),
            Constraint::Length(1),
        ])
        .split(area);

    // Header: totals.
    let count = |s: &str| app.tables.iter().filter(|t| t.status == s).count();
    let live = app
        .tables
        .iter()
        .filter(|t| t.status == "running" || t.status == "awaiting_server")
        .count();
    let rallies: u32 = app.tables.iter().map(|t| t.rallies).sum();
    let tokens: u64 = app.tables.iter().map(|t| t.tokens).sum();
    let first = app.tables.iter().filter_map(|t| t.started_ms).min();
    let last_end = if app.tables.iter().all(|t| t.is_terminal()) {
        app.tables.iter().filter_map(|t| t.ended_ms).max()
    } else {
        None
    };
    let span_s = match first {
        Some(f) => (last_end.unwrap_or(now) - f) as f64 / 1000.0,
        None => 0.0,
    };
    let lost = count("lost") + count("expired") + count("error");
    let header = Line::from(vec![
        Span::styled(
            " ● UnumPingPong ",
            Style::new().fg(Color::Black).bg(Color::Yellow).bold(),
        ),
        Span::raw("  "),
        Span::styled(format!("{} tables", app.tables.len()), Style::new().bold()),
        Span::raw("   "),
        Span::styled(format!("▶ {live} live"), Style::new().fg(Color::Yellow)),
        Span::raw("   "),
        Span::styled(
            format!("✔ {} points", count("point")),
            Style::new().fg(Color::Green).bold(),
        ),
        Span::raw("   "),
        Span::styled(format!("✘ {lost}"), Style::new().fg(Color::Red)),
        Span::raw("   "),
        Span::raw(format!(
            "rallies {rallies}   tokens {}   ⏱ {}",
            fmt_tokens(tokens),
            fmt_dur(span_s)
        )),
    ]);
    f.render_widget(Paragraph::new(header), rows[0]);

    // Tables.
    let head = Row::new(
        [
            "TABLE",
            "PLAYER",
            "STATUS",
            "SERVER⇄PLAYER",
            "RALLY",
            "TRAIL",
            "LEASE",
            "TOKENS",
        ]
        .map(|h| Cell::from(h).style(Style::new().fg(Color::DarkGray).bold())),
    );
    let body: Vec<Row> = app
        .tables
        .iter()
        .map(|t| {
            let name = match &t.after_id {
                Some(_) => format!("↳ {}", t.name),
                None => t.name.clone(),
            };
            let status = if t.status == "awaiting_server" {
                "server?"
            } else {
                t.status.as_str()
            };
            Row::new(vec![
                Cell::from(name).style(Style::new().bold()),
                Cell::from(t.player.clone()).style(Style::new().fg(Color::Gray)),
                Cell::from(status.to_string()).style(status_style(&t.status)),
                Cell::from(lane(t, app.tick)),
                Cell::from(format!("{}/{}", t.rallies, t.max_rallies)),
                Cell::from(trail(t)),
                Cell::from(gauge(t, now)),
                Cell::from(fmt_tokens(t.tokens)),
            ])
        })
        .collect();
    let name_w = app
        .tables
        .iter()
        .map(|t| t.name.chars().count() + if t.after_id.is_some() { 2 } else { 0 })
        .max()
        .unwrap_or(5)
        .clamp(5, 32) as u16;
    let player_w = app
        .tables
        .iter()
        .map(|t| t.player.chars().count())
        .max()
        .unwrap_or(6)
        .clamp(6, 28) as u16;
    let widths = [
        Constraint::Length(name_w),
        Constraint::Length(player_w),
        Constraint::Length(9),
        Constraint::Length(TABLE_W as u16),
        Constraint::Length(5),
        Constraint::Length(10),
        Constraint::Length(12),
        Constraint::Length(7),
    ];
    let mut st = TableState::default();
    if !app.tables.is_empty() {
        st.select(Some(app.selected));
    }
    let title = match &app.run {
        Some(r) => format!(" tables · run {r} "),
        None => " tables · all runs ".to_string(),
    };
    let tbl = Table::new(body, widths)
        .header(head)
        .column_spacing(1)
        .row_highlight_style(Style::new().bg(Color::Rgb(40, 40, 60)))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .title(title),
        );
    f.render_stateful_widget(tbl, rows[1], &mut st);

    // Selected table: current ball | last answer + referee.
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(rows[2]);
    let sel = app.tables.get(app.selected);
    let (ball_title, ball_text) = match sel {
        Some(t) => (
            format!(" ball · {} · rally {} ", t.name, t.rallies.max(1)),
            t.current_ball
                .clone()
                .unwrap_or_else(|| "(not served yet)".into()),
        ),
        None => (
            " ball ".into(),
            "no tables yet: run `upp run examples/...`".into(),
        ),
    };
    f.render_widget(
        Paragraph::new(Text::from(ball_text))
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .title(ball_title),
            ),
        cols[0],
    );
    let mut right: Vec<Line> = Vec::new();
    if let Some(r) = app.rallies.last() {
        let col = if r.verdict == "point" {
            Color::Green
        } else if r.verdict == "fault" {
            Color::Magenta
        } else {
            Color::Red
        };
        right.push(Line::from(vec![
            Span::styled(
                format!("rally {} · {}", r.n, r.verdict),
                Style::new().fg(col).bold(),
            ),
            Span::raw(format!(
                "  {:.1}s",
                (r.ended_ms - r.started_ms) as f64 / 1000.0
            )),
        ]));
        if let Some(a) = r.checks.as_array() {
            for c in a {
                let pass = c["pass"].as_bool() == Some(true);
                let skipped = c["skipped"].as_bool() == Some(true);
                let (m, cc) = if skipped {
                    ("–", Color::DarkGray)
                } else if pass {
                    ("✔", Color::Green)
                } else {
                    ("✘", Color::Red)
                };
                right.push(Line::from(vec![
                    Span::styled(format!("{m} "), Style::new().fg(cc).bold()),
                    Span::raw(format!(
                        "{}: {}",
                        c["name"].as_str().unwrap_or(""),
                        c["summary"].as_str().unwrap_or("")
                    )),
                ]));
            }
        }
        if !r.note.trim().is_empty() {
            right.push(Line::from(Span::styled(
                format!("note: {}", first_line(&r.note)),
                Style::new().fg(Color::Cyan),
            )));
        }
        right.push(Line::from(Span::styled(
            "answer:",
            Style::new().fg(Color::DarkGray),
        )));
        for l in r.answer.lines().take(40) {
            right.push(Line::from(l.to_string()));
        }
    } else if let Some(t) = sel {
        if let Some(a) = &t.last_answer {
            right.push(Line::from(Span::styled(
                "answer:",
                Style::new().fg(Color::DarkGray),
            )));
            for l in a.lines().take(40) {
                right.push(Line::from(l.to_string()));
            }
        }
    }
    let rtitle = sel
        .map(|t| format!(" referee · {} ", t.name))
        .unwrap_or(" referee ".into());
    f.render_widget(
        Paragraph::new(Text::from(right))
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .title(rtitle),
            ),
        cols[1],
    );

    // Events.
    let h = rows[3].height.saturating_sub(2) as usize;
    let evs: Vec<Line> = app
        .events
        .iter()
        .rev()
        .take(h)
        .rev()
        .map(|(e, n)| event_line(e, n))
        .collect();
    f.render_widget(
        Paragraph::new(evs).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .title(" events "),
        ),
        rows[3],
    );

    // Footer.
    let mut foot = vec![
        Span::styled(" q", Style::new().bold()),
        Span::raw(" quit  "),
        Span::styled("↑↓", Style::new().bold()),
        Span::raw(" select  "),
        Span::styled("s", Style::new().bold()),
        Span::raw(" stop table  "),
        Span::styled("a", Style::new().bold()),
        Span::raw("/"),
        Span::styled("r", Style::new().bold()),
        Span::raw(" accept/return a ball waiting for the server   "),
        Span::styled("● ", Style::new().fg(Color::Red)),
        Span::raw("returned "),
        Span::styled("● ", Style::new().fg(Color::Magenta)),
        Span::raw("fault "),
        Span::styled("● ", Style::new().fg(Color::Green)),
        Span::raw("point"),
    ];
    if let Some((m, at)) = &app.flash {
        if at.elapsed() < Duration::from_secs(4) {
            foot.push(Span::styled(
                format!("   {m}"),
                Style::new().fg(Color::Yellow).bold(),
            ));
        }
    }
    f.render_widget(Paragraph::new(Line::from(foot)), rows[4]);
    let _ = app.started;
}

pub fn watch(
    db: &Path,
    run: Option<String>,
    exit_when_done: bool,
    runner: Option<&JoinHandle<Result<(), String>>>,
) -> Result<(), String> {
    let store = Store::open(db)?;
    let mut app = App {
        run,
        tables: vec![],
        events: vec![],
        last_event: 0,
        selected: 0,
        rallies: vec![],
        started: Instant::now(),
        tick: 0,
        done_since: None,
        flash: None,
    };
    let mut term = ratatui::init();
    let mut last_poll = Instant::now() - Duration::from_secs(1);
    let res: Result<(), String> = (|| loop {
        if last_poll.elapsed() >= Duration::from_millis(250) {
            app.refresh(&store);
            last_poll = Instant::now();
        }
        app.tick += 1;
        term.draw(|f| draw(f, &app)).map_err(|e| e.to_string())?;
        let all_done = !app.tables.is_empty()
            && app.tables.iter().all(|t| t.is_terminal())
            && runner.map(|h| h.is_finished()).unwrap_or(true);
        if all_done {
            let since = *app.done_since.get_or_insert_with(Instant::now);
            if exit_when_done && since.elapsed() > Duration::from_millis(2500) {
                return Ok(());
            }
        }
        if event::poll(Duration::from_millis(80)).map_err(|e| e.to_string())? {
            if let Event::Key(k) = event::read().map_err(|e| e.to_string())? {
                if k.kind != KeyEventKind::Press {
                    continue;
                }
                let sel = app.tables.get(app.selected).map(|t| t.id.clone());
                match k.code {
                    KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                    KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                        return Ok(())
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        if app.selected + 1 < app.tables.len() {
                            app.selected += 1;
                            last_poll = Instant::now() - Duration::from_secs(1);
                        }
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        app.selected = app.selected.saturating_sub(1);
                        last_poll = Instant::now() - Duration::from_secs(1);
                    }
                    KeyCode::Char('s') => {
                        if let Some(id) = sel {
                            let m = match store.request_stop(&id) {
                                Ok(()) => format!("stop requested for {id}"),
                                Err(e) => e,
                            };
                            app.flash = Some((m, Instant::now()));
                        }
                    }
                    KeyCode::Char(c @ ('a' | 'r')) => {
                        if let Some(id) = sel {
                            let verdict = if c == 'a' { "accept" } else { "return" };
                            let m = match upp_core::api::answer(&store, &id, verdict, "") {
                                Ok(_) => format!("{verdict} sent to {id}"),
                                Err(e) => e,
                            };
                            app.flash = Some((m, Instant::now()));
                        }
                    }
                    _ => {}
                }
            }
        }
    })();
    ratatui::restore();
    res
}
