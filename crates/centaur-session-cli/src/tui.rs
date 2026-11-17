use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use centaur_api_server::client::{CentaurClient, SseEventStream};
use centaur_session_core::ThreadKey;
use crossterm::{
    cursor::{Hide, Show},
    event::{
        self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEvent, KeyEventKind,
        KeyModifiers,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use eyre::{Result, WrapErr};
use futures_util::StreamExt;
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph, Wrap},
};
use serde_json::Value;
use tokio::{
    sync::mpsc,
    time::{self, Duration as TokioDuration},
};

use crate::{
    SseFrame, StdinEvent, execute_input_lines, is_terminal_event, output_type_matches,
    parse_json_or_string,
};

const MAX_MAIN_LINES: usize = 1_000;
const MAX_DEBUG_LINES: usize = 1_000;

// Catppuccin Mocha, matching Pi's @ifi/oh-pi-themes palette.
const ACCENT: Color = Color::Rgb(203, 166, 247);
const MUTED: Color = Color::Rgb(108, 112, 134);
const BORDER: Color = Color::Rgb(49, 50, 68);
const MANTLE: Color = Color::Rgb(24, 24, 37);
const USER_BG: Color = Color::Rgb(49, 50, 68);
const TOOL_BG: Color = Color::Rgb(30, 30, 46);
const TOOL_SUCCESS_BG: Color = Color::Rgb(30, 46, 30);
const TOOL_ERROR_BG: Color = Color::Rgb(46, 30, 30);
const SUCCESS: Color = Color::Rgb(166, 227, 161);
const ERROR: Color = Color::Rgb(243, 139, 168);
const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

#[derive(Clone)]
struct PiMarkdown;

impl tui_markdown::StyleSheet for PiMarkdown {
    fn heading(&self, _level: u8) -> Style {
        Style::default()
            .fg(Color::Rgb(250, 179, 135))
            .add_modifier(Modifier::BOLD)
    }

    fn heading_marker(&self, _level: u8) -> &str {
        ""
    }

    fn code(&self) -> Style {
        Style::default().fg(Color::Rgb(148, 226, 213))
    }

    fn link(&self) -> Style {
        Style::default()
            .fg(Color::Rgb(137, 180, 250))
            .add_modifier(Modifier::UNDERLINED)
    }

    fn blockquote(&self) -> Style {
        Style::default().fg(MUTED)
    }

    fn list_marker(&self) -> Style {
        Style::default().fg(ACCENT)
    }
}

pub(crate) struct TuiOptions {
    pub(crate) debug_visible: bool,
    pub(crate) idle_timeout_ms: u64,
    pub(crate) max_duration_ms: u64,
    pub(crate) exit_on_terminal: bool,
    pub(crate) exit_on_output_type: Option<String>,
    pub(crate) initial_state: Option<Value>,
}

pub(crate) async fn run(
    client: CentaurClient,
    thread_key: ThreadKey,
    events: SseEventStream,
    options: TuiOptions,
) -> Result<()> {
    let _terminal_restore = TerminalRestore::enter()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    terminal.clear()?;

    let (terminal_tx, mut terminal_rx) = mpsc::channel(64);
    let _terminal_reader = TerminalEventReader::spawn(terminal_tx);
    let (frame_tx, mut frame_rx) = mpsc::channel(256);
    let (notice_tx, mut notice_rx) = mpsc::channel(64);

    spawn_stream_task(events, frame_tx, notice_tx.clone());

    let mut app = TuiApp::new(thread_key.to_string(), options.debug_visible);
    if let Some(state) = &options.initial_state {
        app.harness = state["session"]["harness_type"]
            .as_str()
            .unwrap_or("remote")
            .to_owned();
        for line in crate::terminal::prompt_history(state) {
            app.push_main_line(line);
        }
        app.history = state["messages"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|message| message["role"] == "user")
            .map(|message| {
                message["parts"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|part| part["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .collect();
        app.status = state["execution"]["status"]
            .as_str()
            .unwrap_or("idle")
            .to_owned();
        app.running = matches!(app.status.as_str(), "queued" | "running");
    }
    let mut tick = time::interval(TokioDuration::from_millis(100));

    loop {
        terminal.draw(|frame| draw(frame, &mut app))?;

        tokio::select! {
            _ = tick.tick() => app.spinner = (app.spinner + 1) % SPINNER.len(),
            Some(event) = terminal_rx.recv() => {
                if handle_terminal_event(
                    &mut app,
                    event,
                    &client,
                    &thread_key,
                    &options,
                    notice_tx.clone(),
                )? {
                    break;
                }
            }
            Some(frame) = frame_rx.recv() => {
                if app.handle_sse_frame(frame, &options) {
                    break;
                }
            }
            Some(notice) = notice_rx.recv() => {
                app.handle_notice(notice);
            }
            else => break,
        }
    }

    Ok(())
}

fn spawn_stream_task(
    stream: SseEventStream,
    frame_tx: mpsc::Sender<SseFrame>,
    notice_tx: mpsc::Sender<TuiNotice>,
) {
    tokio::spawn(async move {
        if let Err(error) = stream_sse_frames(stream, frame_tx).await {
            let _ = notice_tx
                .send(TuiNotice::Error(format!("stream error: {error:#}")))
                .await;
        }
    });
}

async fn stream_sse_frames(
    mut stream: SseEventStream,
    frame_tx: mpsc::Sender<SseFrame>,
) -> Result<()> {
    while let Some(frame) = stream.next().await {
        let frame = frame.wrap_err("read event stream")?;
        if frame_tx.send(frame).await.is_err() {
            return Ok(());
        }
    }

    Ok(())
}

fn handle_terminal_event(
    app: &mut TuiApp,
    event: TerminalInput,
    client: &CentaurClient,
    thread_key: &ThreadKey,
    options: &TuiOptions,
    notice_tx: mpsc::Sender<TuiNotice>,
) -> Result<bool> {
    if let TerminalInput::Paste(text) = &event {
        app.input
            .push_str(&text.replace("\r\n", "\n").replace('\r', "\n"));
        return Ok(false);
    }
    let TerminalInput::Key(key) = event else {
        return Ok(false);
    };

    match key.code {
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            return submit_input_line(
                app,
                "/interrupt".to_owned(),
                client,
                thread_key,
                options,
                notice_tx,
            );
        }
        KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => return Ok(true),
        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => app.input.clear(),
        KeyCode::F(2) => app.toggle_debug(),
        KeyCode::PageUp => app.scroll = app.scroll.saturating_add(10),
        KeyCode::PageDown => app.scroll = app.scroll.saturating_sub(10),
        KeyCode::Up => app.recall_history(true),
        KeyCode::Down => app.recall_history(false),
        KeyCode::Enter
            if key
                .modifiers
                .intersects(KeyModifiers::ALT | KeyModifiers::SHIFT) =>
        {
            app.input.push('\n')
        }
        KeyCode::Enter => {
            let line = app.input.trim().to_owned();
            app.input.clear();
            return submit_input_line(app, line, client, thread_key, options, notice_tx);
        }
        KeyCode::Backspace => {
            app.input.pop();
        }
        KeyCode::Char(ch) if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT => {
            app.input.push(ch);
        }
        _ => {}
    }

    Ok(false)
}

fn submit_input_line(
    app: &mut TuiApp,
    line: String,
    client: &CentaurClient,
    thread_key: &ThreadKey,
    options: &TuiOptions,
    notice_tx: mpsc::Sender<TuiNotice>,
) -> Result<bool> {
    if line.is_empty() {
        return Ok(false);
    }
    app.history.push(line.clone());
    app.history_index = None;
    app.scroll = 0;
    match line.as_str() {
        "/interrupt" => {
            let client = client.clone();
            let key = thread_key.clone();
            tokio::spawn(async move {
                let notice = match client.interrupt_session(&key).await {
                    Ok(result) => TuiNotice::Info(format!("interrupt: {result}")),
                    Err(error) => TuiNotice::Error(error.to_string()),
                };
                let _ = notice_tx.send(notice).await;
            });
            return Ok(false);
        }
        "/debug" => {
            app.toggle_debug();
            return Ok(false);
        }
        "/clear" => {
            app.clear();
            return Ok(false);
        }
        "/help" => {
            app.status =
                "Enter send | Alt-Enter newline | Up/Down history | PageUp/Down scroll | Ctrl-C interrupt | Ctrl-D detach".to_owned();
            return Ok(false);
        }
        _ => {}
    }

    let event = if line.starts_with('/') {
        match StdinEvent::parse(&line) {
            Ok(Some(event)) => event,
            Ok(None) => return Ok(false),
            Err(error) => {
                app.handle_notice(TuiNotice::Error(error.to_string()));
                return Ok(false);
            }
        }
    } else {
        // JSON and braces are ordinary prompt text in the interactive UI.
        StdinEvent::Message(line)
    };
    if matches!(event, StdinEvent::Quit) {
        return Ok(true);
    }

    app.note_submitted_event(&event);
    spawn_send_task(
        event,
        client.clone(),
        thread_key.clone(),
        options.idle_timeout_ms,
        options.max_duration_ms,
        notice_tx,
    );
    Ok(false)
}

fn spawn_send_task(
    event: StdinEvent,
    client: CentaurClient,
    thread_key: ThreadKey,
    idle_timeout_ms: u64,
    max_duration_ms: u64,
    notice_tx: mpsc::Sender<TuiNotice>,
) {
    tokio::spawn(async move {
        let notice =
            match send_stdin_event(event, client, thread_key, idle_timeout_ms, max_duration_ms)
                .await
            {
                Ok(message) => TuiNotice::Info(message),
                Err(error) => TuiNotice::Error(format!("{error:#}")),
            };
        let _ = notice_tx.send(notice).await;
    });
}

async fn send_stdin_event(
    event: StdinEvent,
    client: CentaurClient,
    thread_key: ThreadKey,
    idle_timeout_ms: u64,
    max_duration_ms: u64,
) -> Result<String> {
    match event {
        StdinEvent::Message(text) => {
            let request = uuid::Uuid::new_v4().to_string();
            crate::terminal::send_message(&client, &thread_key, &text, &request, max_duration_ms)
                .await.wrap_err_with(|| format!("request_id={request}; check state before retrying with chat send --request-id"))?;
            Ok(format!("message accepted (request_id={request})"))
        }
        StdinEvent::InputLine(line) => {
            execute_input_lines(
                &client,
                &thread_key,
                vec![line],
                idle_timeout_ms,
                max_duration_ms,
            )
            .await?;
            Ok("input line sent".to_owned())
        }
        StdinEvent::InputLines(lines) => {
            let count = lines.len();
            execute_input_lines(
                &client,
                &thread_key,
                lines,
                idle_timeout_ms,
                max_duration_ms,
            )
            .await?;
            Ok(format!("{count} input lines sent"))
        }
        StdinEvent::Quit => Ok("quit".to_owned()),
    }
}

fn draw(frame: &mut Frame<'_>, app: &mut TuiApp) {
    let area = frame.area();
    let input_height = (app.input.split('\n').count().clamp(1, 3) + 2) as u16;
    let layout = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(0),
        Constraint::Length(input_height),
        Constraint::Length(2),
    ])
    .split(area);

    draw_header(frame, layout[0], app);
    draw_body(frame, layout[1], app);
    draw_input(frame, layout[2], app);
    draw_footer(frame, layout[3], app);
}

fn draw_header(frame: &mut Frame<'_>, area: Rect, app: &TuiApp) {
    let title = Line::from(vec![
        Span::styled(
            " CENTAUR ",
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!(" · {}  ", app.harness), Style::default().fg(ACCENT)),
        Span::styled(app.thread_key.as_str(), Style::default().fg(MUTED)),
    ]);
    frame.render_widget(
        Paragraph::new(title)
            .style(Style::default().bg(MANTLE))
            .block(
                Block::default()
                    .borders(Borders::BOTTOM)
                    .border_style(Style::default().fg(BORDER)),
            ),
        area,
    );
}

fn draw_body(frame: &mut Frame<'_>, area: Rect, app: &mut TuiApp) {
    if app.debug_visible {
        let (direction, percentages) = if area.width >= 100 {
            (Direction::Horizontal, [58, 42])
        } else {
            (Direction::Vertical, [55, 45])
        };
        let chunks = Layout::default()
            .direction(direction)
            .constraints(percentages.map(Constraint::Percentage))
            .split(area);
        draw_main(frame, chunks[0], app);
        draw_debug(frame, chunks[1], app);
    } else {
        draw_main(frame, area, app);
    }
}

fn message_paragraph(message: &str, width: u16) -> Paragraph<'_> {
    let mut style = Style::default();
    let text = if let Some(body) = message.strip_prefix("assistant: ") {
        tui_markdown::from_str_with_options(
            body,
            &tui_markdown::Options::new(PiMarkdown).table_width(width),
        )
    } else if message.starts_with("you: [sandbox access: ") {
        style = style.fg(MUTED);
        Text::raw(message.trim_start_matches("you: "))
    } else if let Some(body) = message
        .strip_prefix("you: ")
        .or_else(|| message.strip_prefix("user: "))
    {
        style = style.bg(USER_BG);
        let mut lines = vec![Line::default()];
        lines.extend(body.split('\n').map(Line::raw));
        lines.push(Line::default());
        Text::from(lines)
    } else if let Some(body) = message.strip_prefix("tool: ") {
        let (title, output) = body.split_once('\n').unwrap_or((body, ""));
        let background = if title.ends_with("[exit 0]") {
            TOOL_SUCCESS_BG
        } else if title.contains("[exit ") {
            TOOL_ERROR_BG
        } else {
            TOOL_BG
        };
        style = style.bg(background);
        let mut lines = vec![
            Line::default(),
            Line::styled(
                title,
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            ),
        ];
        lines.extend(output.split('\n').map(Line::raw));
        lines.push(Line::default());
        Text::from(lines)
    } else {
        style = style.fg(
            if message.starts_with("error:") || message.starts_with("session.execution_failed:") {
                ERROR
            } else {
                MUTED
            },
        );
        Text::raw(message)
    };
    Paragraph::new(text).style(style).wrap(Wrap { trim: false })
}

fn draw_main(frame: &mut Frame<'_>, area: Rect, app: &mut TuiApp) {
    let area = Rect::new(
        area.x.saturating_add(1),
        area.y,
        area.width.saturating_sub(2),
        area.height,
    );
    if area.is_empty() {
        return;
    }
    // ponytail: parse the retained transcript on redraw; cache by revision/width if profiling warrants it.
    let messages: Vec<_> = app
        .main_lines
        .iter()
        .map(|message| {
            let paragraph = message_paragraph(message, area.width);
            let height = paragraph.line_count(area.width).max(1);
            (paragraph, height)
        })
        .collect();
    let total: usize = messages.iter().map(|(_, height)| height + 1).sum();
    let max_scroll = total.saturating_sub(area.height as usize);
    app.scroll = app.scroll.min(max_scroll);
    let offset = max_scroll.saturating_sub(app.scroll);
    let mut position = 0;
    for (paragraph, height) in messages {
        let top = position.max(offset);
        let bottom = (position + height).min(offset + area.height as usize);
        if bottom > top {
            let visible = Rect::new(
                area.x,
                area.y + (top - offset) as u16,
                area.width,
                (bottom - top) as u16,
            );
            frame.render_widget(
                paragraph.scroll(((top - position).min(u16::MAX as usize) as u16, 0)),
                visible,
            );
        }
        position += height + 1;
    }
}

fn draw_debug(frame: &mut Frame<'_>, area: Rect, app: &TuiApp) {
    let block = Block::default()
        .title(" Events ")
        .borders(Borders::LEFT)
        .border_style(Style::default().fg(BORDER));
    let inner = block.inner(area);
    let paragraph = Paragraph::new(
        app.debug_lines
            .iter()
            .map(|line| Line::raw(line.as_str()))
            .collect::<Vec<_>>(),
    )
    .style(Style::default().fg(MUTED))
    .wrap(Wrap { trim: false });
    let offset = paragraph
        .line_count(inner.width)
        .saturating_sub(inner.height as usize);
    frame.render_widget(block, area);
    frame.render_widget(
        paragraph.scroll((offset.min(u16::MAX as usize) as u16, 0)),
        inner,
    );
}

fn draw_input(frame: &mut Frame<'_>, area: Rect, app: &TuiApp) {
    let mut block = Block::default()
        .borders(Borders::TOP | Borders::BOTTOM)
        .border_style(Style::default().fg(ACCENT));
    if app.running {
        block = block.title(format!(" {} Working ", SPINNER[app.spinner]));
    }
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.is_empty() {
        return;
    }
    let content = Rect::new(
        inner.x + 1.min(inner.width - 1),
        inner.y,
        inner.width.saturating_sub(2).max(1),
        inner.height,
    );
    let visible = visible_input(&app.input, content.width.saturating_sub(1).max(1) as usize);
    let cursor_x = content.x
        + (Line::raw(visible.rsplit('\n').next().unwrap_or("")).width() as u16)
            .min(content.width - 1);
    let cursor_y = content.y + (visible.matches('\n').count() as u16).min(content.height - 1);
    frame.render_widget(Paragraph::new(visible), content);
    frame.set_cursor_position((cursor_x, cursor_y));
}

fn draw_footer(frame: &mut Frame<'_>, area: Rect, app: &TuiApp) {
    let status_color = if app.status.starts_with("error:") || app.status.contains("failed") {
        ERROR
    } else if app.running {
        ACCENT
    } else if app.status.contains("completed") {
        SUCCESS
    } else {
        MUTED
    };
    let status = app
        .status
        .strip_prefix("session.execution_")
        .unwrap_or(&app.status);
    let event = app.last_event_id.as_deref().unwrap_or("-");
    let lines = vec![
        Line::from(vec![
            Span::styled(format!(" {status}"), Style::default().fg(status_color)),
            Span::styled(
                format!("  ·  {}  ·  event {event}", app.harness),
                Style::default().fg(MUTED),
            ),
        ]),
        Line::styled(
            " Enter send · Alt-Enter newline · ↑/↓ history · PgUp/PgDn scroll · Ctrl-C interrupt · Ctrl-D detach · F2 events",
            Style::default().fg(MUTED),
        ),
    ];
    frame.render_widget(Paragraph::new(lines), area);
}

fn visible_input(input: &str, max_columns: usize) -> String {
    let lines: Vec<_> = input.split('\n').collect();
    lines[lines.len().saturating_sub(3)..]
        .iter()
        .map(|line| {
            let mut start = line
                .char_indices()
                .rev()
                .nth(max_columns.max(1) - 1)
                .map_or(0, |(offset, _)| offset);
            while Line::raw(&line[start..]).width() > max_columns.max(1) {
                start += line[start..].chars().next().map_or(0, char::len_utf8);
            }
            line[start..].to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[derive(Debug)]
enum TerminalInput {
    Key(KeyEvent),
    Resize,
    Paste(String),
}

struct TerminalEventReader {
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl TerminalEventReader {
    fn spawn(sender: mpsc::Sender<TerminalInput>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let reader_stop = Arc::clone(&stop);
        let handle = thread::spawn(move || {
            while !reader_stop.load(Ordering::Relaxed) {
                let Ok(has_event) = event::poll(Duration::from_millis(100)) else {
                    continue;
                };
                if !has_event {
                    continue;
                }
                match event::read() {
                    Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                        if sender.blocking_send(TerminalInput::Key(key)).is_err() {
                            break;
                        }
                    }
                    Ok(Event::Paste(text)) => {
                        if sender.blocking_send(TerminalInput::Paste(text)).is_err() {
                            break;
                        }
                    }
                    Ok(Event::Resize(_, _)) => {
                        if sender.blocking_send(TerminalInput::Resize).is_err() {
                            break;
                        }
                    }
                    Ok(_) | Err(_) => {}
                }
            }
        });
        Self {
            stop,
            handle: Some(handle),
        }
    }
}

impl Drop for TerminalEventReader {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

struct TerminalRestore;

impl TerminalRestore {
    fn enter() -> Result<Self> {
        enable_raw_mode().wrap_err("enable terminal raw mode")?;
        execute!(
            io::stdout(),
            EnterAlternateScreen,
            EnableBracketedPaste,
            Hide
        )
        .wrap_err("enter alternate screen")?;
        Ok(Self)
    }
}

impl Drop for TerminalRestore {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            Show,
            DisableBracketedPaste,
            LeaveAlternateScreen
        );
    }
}

enum TuiNotice {
    Info(String),
    Error(String),
}

struct TuiApp {
    thread_key: String,
    harness: String,
    running: bool,
    spinner: usize,
    input: String,
    main_lines: Vec<String>,
    debug_lines: Vec<String>,
    debug_visible: bool,
    status: String,
    last_event_id: Option<String>,
    current_agent_line: Option<usize>,
    history: Vec<String>,
    history_index: Option<usize>,
    draft: String,
    scroll: usize,
}

impl TuiApp {
    fn new(thread_key: String, debug_visible: bool) -> Self {
        Self {
            thread_key,
            harness: "remote".to_owned(),
            running: false,
            spinner: 0,
            input: String::new(),
            main_lines: Vec::new(),
            debug_lines: Vec::new(),
            debug_visible,
            status: "ready".to_owned(),
            last_event_id: None,
            current_agent_line: None,
            history: Vec::new(),
            history_index: None,
            draft: String::new(),
            scroll: 0,
        }
    }

    fn recall_history(&mut self, previous: bool) {
        if self.history.is_empty() {
            return;
        }
        let index = self.history_index.unwrap_or(self.history.len());
        if self.history_index.is_none() {
            self.draft = self.input.clone();
        }
        let next = if previous {
            index.saturating_sub(1)
        } else {
            (index + 1).min(self.history.len())
        };
        self.history_index = (next < self.history.len()).then_some(next);
        self.input = self
            .history
            .get(next)
            .cloned()
            .unwrap_or_else(|| self.draft.clone());
    }

    fn clear(&mut self) {
        self.main_lines.clear();
        self.debug_lines.clear();
        self.current_agent_line = None;
        self.status = "cleared".to_owned();
    }

    fn toggle_debug(&mut self) {
        self.debug_visible = !self.debug_visible;
        self.status = if self.debug_visible {
            "debug events visible".to_owned()
        } else {
            "debug events hidden".to_owned()
        };
    }

    fn note_submitted_event(&mut self, event: &StdinEvent) {
        match event {
            StdinEvent::Message(text) => {
                self.push_main_line(format!("you: {text}"));
                self.status = "sending message".to_owned();
            }
            StdinEvent::InputLine(_) => {
                self.status = "sending raw input line".to_owned();
            }
            StdinEvent::InputLines(lines) => {
                self.status = format!("sending {} raw input lines", lines.len());
            }
            StdinEvent::Quit => {}
        }
    }

    fn handle_notice(&mut self, notice: TuiNotice) {
        match notice {
            TuiNotice::Info(message) => self.status = message,
            TuiNotice::Error(message) => {
                self.status = format!("error: {message}");
                self.push_main_line(self.status.clone());
            }
        }
    }

    fn handle_sse_frame(&mut self, frame: SseFrame, options: &TuiOptions) -> bool {
        self.last_event_id = (!frame.id.is_empty()).then_some(frame.id.clone());
        self.push_debug_line(format_debug_frame(&frame));

        if frame.event == "session.output.line" {
            self.handle_output_line(&frame.data);
            if output_type_matches(&frame.data, options.exit_on_output_type.as_deref()) {
                self.status = format!(
                    "matched output type {}",
                    options.exit_on_output_type.as_deref().unwrap_or_default()
                );
                return true;
            }
        } else if frame.event == "session.client_reconnecting" {
            self.status = "disconnected; reconnecting (remote execution is unaffected)".to_owned();
        } else if frame.event == "session.execution_started" {
            self.running = true;
            self.status = "execution started".to_owned();
        } else if is_terminal_event(&frame.event) {
            self.running = false;
            self.current_agent_line = None;
            self.status = frame.event.clone();
            if frame.event != "session.execution_completed" {
                let data = parse_json_or_string(&frame.data);
                let message = data["error"].as_str().unwrap_or(&frame.data);
                let label = if frame.event == "session.execution_failed" {
                    "error"
                } else {
                    &frame.event
                };
                self.push_main_line(format!("{label}: {message}"));
            }
            if options.exit_on_terminal {
                return true;
            }
        }

        false
    }

    fn handle_output_line(&mut self, data: &str) {
        let Ok(value) = serde_json::from_str::<Value>(data) else {
            self.status = "received non-json output".to_owned();
            return;
        };

        let Some(event_type) = output_event_name(&value) else {
            self.status = "received output event".to_owned();
            return;
        };

        match event_type {
            "remoteControl/status/changed" => {
                if let Some(status) = value
                    .get("params")
                    .and_then(|params| params.get("status"))
                    .and_then(Value::as_str)
                {
                    self.status = format!("remote control: {status}");
                }
            }
            "thread/started" => {
                if let Some(thread_id) = value
                    .get("params")
                    .and_then(|params| params.get("thread"))
                    .and_then(|thread| thread.get("id"))
                    .and_then(Value::as_str)
                {
                    self.status = format!("codex thread {thread_id}");
                } else {
                    self.status = "codex thread started".to_owned();
                }
            }
            "thread/status/changed" => {
                if let Some(status) = value
                    .get("params")
                    .and_then(|params| params.get("status"))
                    .and_then(|status| status.get("type"))
                    .and_then(Value::as_str)
                {
                    self.status = format!("thread {status}");
                }
            }
            "turn/started" | "turn.started" => {
                self.running = true;
                self.current_agent_line = None;
                self.status = "turn started".to_owned();
            }
            "item/started" | "item.started" => {
                self.handle_item_event(&value, false);
            }
            "item/completed" | "item.completed" => {
                self.handle_item_event(&value, true);
            }
            "item/agentMessage/delta" | "item.agentMessage.delta" => {
                if let Some(delta) = app_server_delta(&value) {
                    self.append_agent_delta(delta);
                }
            }
            "thread/tokenUsage/updated" => {
                self.status = "token usage updated".to_owned();
            }
            "turn/completed" | "turn.completed" => {
                self.running = false;
                self.current_agent_line = None;
                if let Some(error) = value["params"]["turn"]["error"]["message"]
                    .as_str()
                    .or_else(|| value["turn"]["error"]["message"].as_str())
                {
                    self.handle_notice(TuiNotice::Error(error.to_owned()));
                } else {
                    self.status = "turn completed".to_owned();
                }
            }
            "result" => {
                self.current_agent_line = None;
                self.status = "result received".to_owned();
            }
            "system" => {
                let subtype = value
                    .get("subtype")
                    .and_then(Value::as_str)
                    .unwrap_or("event");
                self.status = format!("system: {subtype}");
            }
            other => {
                self.status = format!("event: {other}");
            }
        }
    }

    fn handle_item_event(&mut self, value: &Value, completed: bool) {
        let Some(item) = app_server_item(value) else {
            return;
        };
        match item.get("type").and_then(Value::as_str) {
            Some("userMessage") if completed || item.get("content").is_some() => {
                if let Some(text) = user_message_text(item) {
                    self.push_user_line_once(text);
                }
            }
            Some("agentMessage") => {
                if let Some(text) = item.get("text").and_then(Value::as_str) {
                    if !text.is_empty() && self.current_agent_line.is_none() {
                        self.push_main_line(format!("assistant: {text}"));
                    }
                } else if !completed && self.current_agent_line.is_none() {
                    self.push_main_line("assistant: ".to_owned());
                    self.current_agent_line = self.main_lines.len().checked_sub(1);
                }
                if completed {
                    self.current_agent_line = None;
                }
            }
            Some("commandExecution") => {
                let command = item["command"].as_str().unwrap_or("command");
                if completed {
                    self.push_main_line(format!(
                        "tool: {command} [exit {}]\n{}",
                        item["exitCode"],
                        item["aggregatedOutput"].as_str().unwrap_or("")
                    ));
                } else {
                    self.status = format!("running: {command}");
                }
            }
            Some("dynamicToolCall") => {
                let tool = item["tool"].as_str().unwrap_or("tool");
                if completed {
                    self.push_main_line(format!("tool: {tool}\n{}", compact_json(item, 4000)));
                } else {
                    self.status = format!("running: {tool}");
                }
            }
            _ => {}
        }
    }

    fn append_agent_delta(&mut self, delta: &str) {
        if self.current_agent_line.is_none() {
            self.push_main_line("assistant: ".to_owned());
            self.current_agent_line = self.main_lines.len().checked_sub(1);
        }
        if let Some(index) = self.current_agent_line
            && let Some(line) = self.main_lines.get_mut(index)
        {
            line.push_str(delta);
        }
    }

    fn push_user_line_once(&mut self, text: &str) {
        let line = format!("you: {text}");
        if !self
            .main_lines
            .iter()
            .rev()
            .take(8)
            .any(|existing| existing == &line)
        {
            self.push_main_line(line);
        }
    }

    fn push_main_line(&mut self, line: String) {
        self.main_lines.push(line);
        if self.main_lines.len() > MAX_MAIN_LINES {
            self.main_lines.remove(0);
            self.current_agent_line = self
                .current_agent_line
                .and_then(|index| index.checked_sub(1));
        }
    }

    fn push_debug_line(&mut self, line: String) {
        self.debug_lines.push(line);
        if self.debug_lines.len() > MAX_DEBUG_LINES {
            self.debug_lines.remove(0);
        }
    }
}

fn output_event_name(value: &Value) -> Option<&str> {
    value
        .get("method")
        .and_then(Value::as_str)
        .or_else(|| value.get("type").and_then(Value::as_str))
}

fn app_server_item(value: &Value) -> Option<&Value> {
    value
        .get("params")
        .and_then(|params| params.get("item"))
        .or_else(|| value.get("item"))
}

fn app_server_delta(value: &Value) -> Option<&str> {
    value
        .get("params")
        .and_then(|params| params.get("delta"))
        .and_then(Value::as_str)
        .or_else(|| value.get("delta").and_then(Value::as_str))
}

fn user_message_text(item: &Value) -> Option<&str> {
    item.get("content")?
        .as_array()?
        .iter()
        .find_map(|part| part.get("text").and_then(Value::as_str))
}

fn format_debug_frame(frame: &SseFrame) -> String {
    let id = if frame.id.is_empty() {
        "-"
    } else {
        frame.id.as_str()
    };
    let data = parse_json_or_string(&frame.data);
    format!(
        "{} {} {}",
        id,
        frame.event,
        truncate(&compact_json(&data, 1_000), 1_000)
    )
}

fn compact_json(value: &Value, max_chars: usize) -> String {
    let rendered = match value {
        Value::String(value) => value.clone(),
        _ => serde_json::to_string(value).unwrap_or_else(|_| "<invalid json>".to_owned()),
    };
    truncate(&rendered, max_chars)
}

fn truncate(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let truncated = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{truncated}...")
    } else {
        truncated
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, buffer::Buffer};

    fn screen(app: &mut TuiApp, width: u16, height: u16) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| draw(frame, app)).unwrap();
        terminal.backend().buffer().clone()
    }

    fn screen_text(buffer: &Buffer) -> String {
        buffer.content.iter().map(|cell| cell.symbol()).collect()
    }

    #[test]
    fn pi_style_keeps_centaur_header_and_colors_markdown_and_message_panels() {
        let mut app = TuiApp::new("api:test:styled".to_owned(), false);
        for message in [
            "you: Hello",
            "assistant: # Heading\n\n**bold** and `code`",
            "tool: printf hello [exit 0]\nhello",
            "error: provider rejected the request",
        ] {
            app.push_main_line(message.to_owned());
        }
        let buffer = screen(&mut app, 100, 32);
        let text = screen_text(&buffer);
        assert!(text.contains("CENTAUR"));
        assert!(text.contains("Heading"));
        assert!(text.contains("provider rejected the request"));
        assert!(!text.contains("┌Session") && !text.contains("┌Input"));
        assert!(!text.contains("assistant:") && !text.contains("**bold**"));
        for color in [
            Color::Rgb(203, 166, 247),
            Color::Rgb(148, 226, 213),
            Color::Rgb(243, 139, 168),
        ] {
            assert!(buffer.content.iter().any(|cell| cell.fg == color));
        }
        for color in [Color::Rgb(49, 50, 68), Color::Rgb(30, 46, 30)] {
            assert!(buffer.content.iter().any(|cell| cell.bg == color));
        }
    }

    #[test]
    fn wrapped_transcript_stays_at_the_bottom_and_scrolls_at_small_sizes() {
        let mut app = TuiApp::new("api:test:scroll".to_owned(), false);
        let body = (0..40).map(|n| format!("row {n}\n\n")).collect::<String>();
        app.push_main_line(format!("assistant: {body}LAST_LINE"));
        assert!(screen_text(&screen(&mut app, 40, 14)).contains("LAST_LINE"));
        app.scroll = 10;
        assert!(!screen_text(&screen(&mut app, 40, 14)).contains("LAST_LINE"));
        for (width, height) in [(24, 12), (10, 5), (1, 1), (0, 0)] {
            screen(&mut app, width, height);
        }
        assert_eq!(visible_input("界界界", 4), "界界");
    }

    #[test]
    fn failed_turn_displays_the_provider_error_instead_of_only_a_generic_status() {
        let mut app = TuiApp::new("api:test:error".to_owned(), false);
        app.handle_output_line(r#"{"method":"turn/completed","params":{"turn":{"status":"failed","error":{"message":"401 invalid x-api-key"}}}}"#);
        assert!(
            app.main_lines
                .iter()
                .any(|line| line.contains("401 invalid x-api-key"))
        );
    }

    #[test]
    fn multiline_paste_history_and_detach_are_client_only_operations() {
        let mut app = TuiApp::new("api:test:pi".to_owned(), false);
        app.input = "draft".to_owned();
        app.history = vec!["old prompt".to_owned()];
        app.recall_history(true);
        assert_eq!(app.input, "old prompt");
        app.recall_history(false);
        assert_eq!(app.input, "draft");
        let client = CentaurClient::new("http://127.0.0.1:1");
        let key = ThreadKey::parse("api:test:pi").unwrap();
        let options = TuiOptions {
            debug_visible: false,
            idle_timeout_ms: 1000,
            max_duration_ms: 60000,
            exit_on_terminal: false,
            exit_on_output_type: None,
            initial_state: None,
        };
        let (tx, _rx) = mpsc::channel(4);
        assert!(
            !handle_terminal_event(
                &mut app,
                TerminalInput::Paste("\r\nline2".to_owned()),
                &client,
                &key,
                &options,
                tx.clone()
            )
            .unwrap()
        );
        assert_eq!(app.input, "draft\nline2");
        assert!(
            !handle_terminal_event(
                &mut app,
                TerminalInput::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT)),
                &client,
                &key,
                &options,
                tx.clone()
            )
            .unwrap()
        );
        assert_eq!(app.input, "draft\nline2\n");
        assert!(
            handle_terminal_event(
                &mut app,
                TerminalInput::Key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL)),
                &client,
                &key,
                &options,
                tx
            )
            .unwrap()
        );
        assert_eq!(
            visible_input("one\ntwo\nthree\nfour", 80),
            "two\nthree\nfour"
        );
    }

    #[test]
    fn renders_pi_messages_without_deltas_and_tool_output() {
        let mut app = TuiApp::new("api:test:pi".to_owned(), false);
        app.handle_output_line(r#"{"method":"item/started","params":{"item":{"id":"pi-1","type":"agentMessage","text":""}}}"#);
        app.handle_output_line(r#"{"method":"item/completed","params":{"item":{"id":"pi-1","type":"agentMessage","text":"DONE"}}}"#);
        app.handle_output_line(r#"{"method":"item/completed","params":{"item":{"type":"commandExecution","command":"echo hi","aggregatedOutput":"hi\n","exitCode":0}}}"#);
        assert_eq!(
            app.main_lines,
            vec!["assistant: DONE", "tool: echo hi [exit 0]\nhi\n"]
        );
    }

    #[test]
    fn projects_codex_app_server_events_into_transcript() {
        let mut app = TuiApp::new("cli:test".to_owned(), false);
        app.clear();

        for line in [
            r#"{"method":"thread/started","params":{"thread":{"id":"thread-1"}}}"#,
            r#"{"method":"turn/started","params":{"threadId":"thread-1","turn":{"id":"turn-1"}}}"#,
            r#"{"method":"item/started","params":{"item":{"type":"userMessage","content":[{"type":"text","text":"hello"}]}}}"#,
            r#"{"method":"item/completed","params":{"item":{"type":"userMessage","content":[{"type":"text","text":"hello"}]}}}"#,
            r#"{"method":"item/started","params":{"item":{"id":"agent-1","type":"agentMessage","text":""}}}"#,
            r#"{"method":"item/agentMessage/delta","params":{"delta":"PO"}}"#,
            r#"{"method":"item/agentMessage/delta","params":{"delta":"NG"}}"#,
            r#"{"method":"item/completed","params":{"item":{"id":"agent-1","type":"agentMessage","text":"PONG"}}}"#,
            r#"{"method":"thread/tokenUsage/updated","params":{"tokenUsage":{"last":{"inputTokens":10,"cachedInputTokens":4,"outputTokens":2,"totalTokens":12}}}}"#,
            r#"{"method":"turn/completed","params":{"turn":{"id":"turn-1"}}}"#,
        ] {
            app.handle_output_line(line);
        }

        assert_eq!(app.main_lines, vec!["you: hello", "assistant: PONG"]);
    }

    #[test]
    fn still_projects_legacy_type_delta_events() {
        let mut app = TuiApp::new("cli:test".to_owned(), false);
        app.clear();

        app.handle_output_line(r#"{"type":"item.agentMessage.delta","delta":"P"}"#);
        app.handle_output_line(r#"{"type":"item.agentMessage.delta","delta":"ONG"}"#);
        app.handle_output_line(r#"{"type":"turn.completed"}"#);

        assert_eq!(app.main_lines, vec!["assistant: PONG"]);
    }

    #[test]
    fn leaves_non_message_events_out_of_transcript() {
        let mut app = TuiApp::new("cli:test".to_owned(), false);

        for line in [
            r#"{"type":"system","subtype":"wrapper_heartbeat"}"#,
            r#"{"method":"thread/started","params":{"thread":{"id":"thread-1"}}}"#,
            r#"{"method":"turn/started","params":{"turn":{"id":"turn-1"}}}"#,
            r#"{"method":"thread/tokenUsage/updated","params":{"tokenUsage":{"last":{"totalTokens":12}}}}"#,
            r#"{"method":"turn/completed","params":{"turn":{"id":"turn-1"}}}"#,
        ] {
            app.handle_output_line(line);
        }

        assert!(app.main_lines.is_empty());
    }
}
