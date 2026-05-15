//! 生成済み問題JSONを端末上で検索・閲覧するTUIビューア．

use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Terminal;
use ratatui::{backend::CrosstermBackend, Frame};

use flowcloze::validation::GeneratedQuestion;
use flowcloze::GeneratedDocument;

const FRAME_TICK: Duration = Duration::from_millis(200);

pub fn run_viewer(document: GeneratedDocument) -> Result<(), String> {
    let mut app = ViewerApp::new(document.questions);
    app.apply_filter();

    enable_raw_mode().map_err(|error| format!("TUIの初期化に失敗しました: {error}"))?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen)
        .map_err(|error| format!("TUI画面を開けませんでした: {error}"))?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)
        .map_err(|error| format!("TUI端末の初期化に失敗しました: {error}"))?;

    let result = run_event_loop(&mut terminal, &mut app);

    disable_raw_mode().ok();
    execute!(terminal.backend_mut(), LeaveAlternateScreen).ok();
    terminal.show_cursor().ok();

    result
}

fn run_event_loop(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    app: &mut ViewerApp,
) -> Result<(), String> {
    loop {
        terminal
            .draw(|frame| draw_ui(frame, app))
            .map_err(|error| format!("TUI描画に失敗しました: {error}"))?;

        if event::poll(FRAME_TICK).map_err(|error| error.to_string())? {
            if let Event::Key(key) = event::read().map_err(|error| error.to_string())? {
                if app.input_mode {
                    handle_filter_input(app, key)?;
                } else if handle_normal_input(app, key)? {
                    return Ok(());
                }
            }
        }
    }
}

fn handle_filter_input(app: &mut ViewerApp, key: KeyEvent) -> Result<(), String> {
    match key.code {
        KeyCode::Esc => {
            app.input_mode = false;
        }
        KeyCode::Enter => {
            app.input_mode = false;
            app.apply_filter();
        }
        KeyCode::Backspace => {
            app.filter.pop();
        }
        KeyCode::Char(ch) => {
            if key.modifiers.contains(KeyModifiers::CONTROL) {
                return Ok(());
            }
            app.filter.push(ch);
        }
        _ => {}
    }
    Ok(())
}

fn handle_normal_input(app: &mut ViewerApp, key: KeyEvent) -> Result<bool, String> {
    match key.code {
        KeyCode::Char('q') => return Ok(true),
        KeyCode::Char('r') => app.show_raw = !app.show_raw,
        KeyCode::Char('/') => app.input_mode = true,
        KeyCode::Char('c') => {
            app.filter.clear();
            app.apply_filter();
        }
        KeyCode::Char('j') => app.scroll_detail(1),
        KeyCode::Char('k') => app.scroll_detail(-1),
        KeyCode::Char('J') => app.scroll_detail(10),
        KeyCode::Char('K') => app.scroll_detail(-10),
        KeyCode::Up => app.move_selection(-1),
        KeyCode::Down => app.move_selection(1),
        KeyCode::PageUp => app.move_selection(-10),
        KeyCode::PageDown => app.move_selection(10),
        _ => {}
    }
    Ok(false)
}

fn draw_ui(frame: &mut Frame, app: &mut ViewerApp) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(5),
            Constraint::Length(3),
        ])
        .split(frame.size());

    draw_header(frame, app, layout[0]);
    draw_body(frame, app, layout[1]);
    draw_footer(frame, app, layout[2]);
}

fn draw_header(frame: &mut Frame, app: &ViewerApp, area: Rect) {
    let title = if app.input_mode {
        "ClozeView / filter"
    } else {
        "ClozeView"
    };
    let filter_label = format!("filter: {}", app.filter);
    let block = Block::default().borders(Borders::ALL).title(title);
    let text = Text::from(Line::from(vec![
        Span::styled(
            "FLOWCLOZE",
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" :: "),
        Span::styled(filter_label, Style::default().fg(Color::Cyan)),
        Span::raw("  "),
        Span::styled(
            format!("items: {}", app.filtered_indices.len()),
            Style::default().fg(Color::DarkGray),
        ),
    ]));
    let paragraph = Paragraph::new(text).block(block);
    frame.render_widget(paragraph, area);
}

fn draw_body(frame: &mut Frame, app: &mut ViewerApp, area: Rect) {
    let body_layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(16), Constraint::Min(10)])
        .split(area);

    draw_list(frame, app, body_layout[0]);
    draw_detail(frame, app, body_layout[1]);
}

fn draw_list(frame: &mut Frame, app: &mut ViewerApp, area: Rect) {
    let mut last_section: Option<String> = None;
    let mut section_index: usize = 0;
    let palette = [
        Color::Cyan,
        Color::LightBlue,
        Color::Green,
        Color::Yellow,
        Color::Magenta,
    ];
    let items: Vec<ListItem> = app
        .filtered_indices
        .iter()
        .map(|index| {
            let question = &app.questions[*index];
            let section = question.section.as_deref().unwrap_or("-");
            if last_section.as_deref() != Some(section) {
                section_index = section_index.wrapping_add(1);
                last_section = Some(section.to_string());
            }
            let color = palette[section_index % palette.len()];
            let line = Line::from(vec![Span::styled(
                question.id.as_str(),
                Style::default().fg(color),
            )]);
            ListItem::new(line)
        })
        .collect();

    let mut state = ListState::default();
    if !app.filtered_indices.is_empty() {
        state.select(Some(app.selected));
    }

    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title("Questions"))
        .highlight_style(Style::default().add_modifier(Modifier::BOLD))
        .highlight_symbol("> ");
    frame.render_stateful_widget(list, area, &mut state);
}

fn draw_detail(frame: &mut Frame, app: &ViewerApp, area: Rect) {
    let block = Block::default().borders(Borders::ALL).title("Details");
    let content = if let Some(question) = app.current_question() {
        if app.show_raw {
            let raw = serde_json::to_string_pretty(question)
                .unwrap_or_else(|_| "<invalid json>".to_string());
            Text::from(raw)
        } else {
            build_detail_text(question)
        }
    } else {
        Text::from("No items")
    };
    let visible_rows = area.height.saturating_sub(2) as usize;
    let max_scroll = content.lines.len().saturating_sub(visible_rows);
    let scroll = app.detail_scroll.min(max_scroll as u16);
    let paragraph = Paragraph::new(content)
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));
    frame.render_widget(paragraph, area);
}

fn draw_footer(frame: &mut Frame, _app: &ViewerApp, area: Rect) {
    let block = Block::default().borders(Borders::ALL);
    let line = Line::from(vec![
        Span::styled("q", Style::default().fg(Color::Yellow)),
        Span::raw(" Quit   "),
        Span::styled("/", Style::default().fg(Color::Yellow)),
        Span::raw(" Filter   "),
        Span::styled("Enter", Style::default().fg(Color::Yellow)),
        Span::raw(" Apply   "),
        Span::styled("Esc", Style::default().fg(Color::Yellow)),
        Span::raw(" Cancel   "),
        Span::styled("c", Style::default().fg(Color::Yellow)),
        Span::raw(" Clear   "),
        Span::styled("r", Style::default().fg(Color::Yellow)),
        Span::raw(" Raw   "),
        Span::styled("Up/Down", Style::default().fg(Color::Yellow)),
        Span::raw(" Move   "),
        Span::styled("PgUp/PgDn", Style::default().fg(Color::Yellow)),
        Span::raw(" Page   "),
        Span::styled("j/k", Style::default().fg(Color::Yellow)),
        Span::raw(" Detail Scroll"),
    ]);
    let paragraph = Paragraph::new(Text::from(line))
        .block(block)
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

fn build_detail_text(question: &GeneratedQuestion) -> Text<'_> {
    let mut lines = Vec::new();
    push_yaml_scalar(&mut lines, "id", &question.id, Color::Cyan);
    push_yaml_scalar(
        &mut lines,
        "section",
        question.section.as_deref().unwrap_or("-"),
        Color::Yellow,
    );
    push_yaml_scalar(&mut lines, "type", &question.question_type, Color::Magenta);
    let targets = format_targets(question);
    push_yaml_scalar(&mut lines, "targets", &targets, Color::Green);
    push_yaml_list(&mut lines, "answers", &question.answers, Color::Green);
    if !question.tags.is_empty() {
        push_yaml_list(&mut lines, "tags", &question.tags, Color::Blue);
    }
    if !question.warnings.is_empty() {
        push_yaml_list(&mut lines, "warnings", &question.warnings, Color::Red);
    }
    if let Some(source_text) = &question.source_text {
        push_yaml_block(&mut lines, "source_text", source_text, Color::White);
    }
    if let Some(explanation) = &question.explanation {
        push_yaml_block(&mut lines, "explanation", explanation, Color::White);
    }
    push_yaml_block(&mut lines, "question", &question.question, Color::White);

    Text::from(lines)
}

fn push_yaml_scalar(lines: &mut Vec<Line>, label: &str, value: &str, value_color: Color) {
    lines.push(Line::from(vec![
        Span::styled(
            format!("{label}: "),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(value.to_string(), Style::default().fg(value_color)),
    ]));
}

fn push_yaml_list(lines: &mut Vec<Line>, label: &str, values: &[String], value_color: Color) {
    lines.push(Line::from(vec![Span::styled(
        format!("{label}:"),
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    )]));
    for value in values {
        lines.push(Line::from(vec![
            Span::raw("  - "),
            Span::styled(value.to_string(), Style::default().fg(value_color)),
        ]));
    }
}

fn push_yaml_block(lines: &mut Vec<Line>, label: &str, text: &str, value_color: Color) {
    lines.push(Line::from(vec![Span::styled(
        format!("{label}:"),
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    )]));
    for line in text.lines() {
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(line.to_string(), Style::default().fg(value_color)),
        ]));
    }
}

fn format_targets(question: &GeneratedQuestion) -> String {
    if let Some(targets) = &question.targets {
        if targets.is_empty() {
            return "-".to_string();
        }
        return targets
            .iter()
            .map(|target| format!("{}({})", target.answer, target.target_type))
            .collect::<Vec<_>>()
            .join(", ");
    }

    if question.answers.is_empty() {
        return "-".to_string();
    }

    question.answers.join(", ")
}

struct ViewerApp {
    questions: Vec<GeneratedQuestion>,
    filtered_indices: Vec<usize>,
    selected: usize,
    filter: String,
    input_mode: bool,
    show_raw: bool,
    detail_scroll: u16,
}

impl ViewerApp {
    fn new(questions: Vec<GeneratedQuestion>) -> Self {
        Self {
            questions,
            filtered_indices: Vec::new(),
            selected: 0,
            filter: String::new(),
            input_mode: false,
            show_raw: false,
            detail_scroll: 0,
        }
    }

    fn apply_filter(&mut self) {
        let needle = self.filter.to_lowercase();
        self.filtered_indices = self
            .questions
            .iter()
            .enumerate()
            .filter_map(|(index, question)| {
                if needle.is_empty() {
                    return Some(index);
                }

                let section = question.section.as_deref().unwrap_or("");
                let mut haystack =
                    format!("{} {} {}", question.id, section, question.tags.join(" "));
                if let Some(targets) = &question.targets {
                    for target in targets {
                        haystack.push(' ');
                        haystack.push_str(&target.answer);
                        haystack.push(' ');
                        haystack.push_str(&target.target_type);
                    }
                }
                if haystack.to_lowercase().contains(&needle) {
                    Some(index)
                } else {
                    None
                }
            })
            .collect();

        if self.filtered_indices.is_empty() {
            self.selected = 0;
        } else if self.selected >= self.filtered_indices.len() {
            self.selected = self.filtered_indices.len() - 1;
        }
        self.detail_scroll = 0;
    }

    fn move_selection(&mut self, delta: i32) {
        if self.filtered_indices.is_empty() {
            return;
        }
        let len = self.filtered_indices.len() as i32;
        let mut next = self.selected as i32 + delta;
        if next < 0 {
            next = 0;
        }
        if next >= len {
            next = len - 1;
        }
        self.selected = next as usize;
        self.detail_scroll = 0;
    }

    fn scroll_detail(&mut self, delta: i32) {
        let next = if delta.is_negative() {
            self.detail_scroll
                .saturating_sub(delta.unsigned_abs() as u16)
        } else {
            self.detail_scroll.saturating_add(delta as u16)
        };
        self.detail_scroll = next;
    }

    fn current_question(&self) -> Option<&GeneratedQuestion> {
        self.filtered_indices
            .get(self.selected)
            .map(|index| &self.questions[*index])
    }
}
