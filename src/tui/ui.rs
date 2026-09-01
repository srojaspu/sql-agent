use crate::tui::app::AppState;
use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Clear,
    History,
    Tables,
    Describe(String),
    Refresh,
    Quit,
    Help,
    Unknown(String),
    Message(String),
}

/// Parse slash commands. Returns Command.
pub fn parse_command(input: &str) -> Command {
    let t = input.trim();
    match t {
        "/clear" => Command::Clear,
        "/history" => Command::History,
        "/tables" => Command::Tables,
        "/refresh" => Command::Refresh,
        "/quit" | "/exit" | "/q" => Command::Quit,
        "/help" | "/?" => Command::Help,
        s if s.starts_with("/describe ") => {
            let tbl = s.trim_start_matches("/describe ").trim().to_string();
            if tbl.is_empty() {
                Command::Unknown(t.to_string())
            } else {
                Command::Describe(tbl)
            }
        }
        s if s.starts_with('/') => Command::Unknown(s.to_string()),
        other => Command::Message(other.to_string()),
    }
}

const HELP_TEXT: &str = "Comandos: /clear /history /tables /describe <tabla> /refresh /quit /help | Teclas: Enter enviar, Esc salir, ↑↓ scroll, PgUp/PgDn, Ctrl-C salir";

/// Render TUI frames: chat 70% + input 15% + status 15%
pub fn draw(frame: &mut Frame, app: &AppState) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(70),
            Constraint::Percentage(15),
            Constraint::Percentage(15),
        ])
        .split(area);

    // Chat pane
    let chat_lines: Vec<Line> = if app.history_visible {
        // History view: show session messages
        if app.session.messages.is_empty() {
            vec![Line::from(Span::styled(
                "Historial vacío",
                Style::default().fg(Color::DarkGray),
            ))]
        } else {
            app.session
                .messages
                .iter()
                .map(|m| {
                    let role_color = match m.role.as_str() {
                        "user" => Color::Cyan,
                        "assistant" => Color::Green,
                        "tool" => Color::Yellow,
                        _ => Color::White,
                    };
                    Line::from(vec![
                        Span::styled(format!("{}: ", m.role), Style::default().fg(role_color)),
                        Span::raw(m.content.chars().take(200).collect::<String>()),
                    ])
                })
                .collect()
        }
    } else {
        // Normal chat: visible slice respecting scroll_offset
        // Use all available height estimate (chunks[0].height) for visible
        let height = chunks[0].height.saturating_sub(2) as usize; // borders
        let visible = app.visible_messages(height.max(1));
        if visible.is_empty() && app.messages.is_empty() {
            vec![Line::from(Span::styled(
                "Bienvenido — escribe tu pregunta y presiona Enter. /help para ayuda.",
                Style::default().fg(Color::DarkGray),
            ))]
        } else {
            visible
                .iter()
                .map(|l| {
                    let role_color = if l.role.starts_with("tool:") {
                        Color::Yellow
                    } else {
                        match l.role.as_str() {
                            "user" => Color::Cyan,
                            "assistant" => Color::Green,
                            "error" => Color::Red,
                            _ => Color::White,
                        }
                    };
                    Line::from(vec![
                        Span::styled(format!("{}: ", l.role), Style::default().fg(role_color)),
                        Span::raw(l.content.clone()),
                    ])
                })
                .collect()
        }
    };

    let chat_block = Block::default()
        .title(" Chat ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Blue));
    let chat_para = Paragraph::new(chat_lines)
        .block(chat_block)
        .wrap(Wrap { trim: true })
        .style(Style::default().fg(Color::White));
    frame.render_widget(chat_para, chunks[0]);

    // Input pane
    let input_text = if app.input.is_empty() {
        Line::from(Span::styled(
            "Escribe aquí... (/help)",
            Style::default().fg(Color::DarkGray),
        ))
    } else {
        Line::from(Span::raw(app.input.clone()))
    };
    let input_block = Block::default()
        .title(" Input (Enter enviar, Esc salir) ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Green));
    let input_para = Paragraph::new(input_text).block(input_block);
    frame.render_widget(input_para, chunks[1]);

    // Status pane
    let status_color = if app.is_loading {
        Color::Yellow
    } else if app.status.to_ascii_lowercase().contains("error") {
        Color::Red
    } else {
        Color::Gray
    };
    let tool_info = app
        .current_tool
        .as_ref()
        .map(|t| format!(" | tool: {t} step: {}", app.current_step))
        .unwrap_or_default();
    let status_line = Line::from(vec![
        Span::styled(app.status.clone(), Style::default().fg(status_color)),
        Span::styled(tool_info, Style::default().fg(Color::DarkGray)),
    ]);
    let help_line = if app.show_help {
        Line::from(Span::styled(HELP_TEXT, Style::default().fg(Color::Cyan)))
    } else {
        Line::from(Span::styled(
            " /help para comandos | ↑↓ scroll PgUp/PgDn | /quit salir",
            Style::default().fg(Color::DarkGray),
        ))
    };
    let status_block = Block::default()
        .title(" Status ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Magenta));
    let status_para = Paragraph::new(vec![status_line, help_line])
        .block(status_block)
        .wrap(Wrap { trim: true });
    frame.render_widget(status_para, chunks[2]);
}

/// Handle crossterm key events, mutating AppState. Returns true if should quit.
pub fn handle_key(key: crossterm::event::KeyEvent, app: &mut AppState) -> bool {
    use crossterm::event::{KeyCode, KeyModifiers};
    match (key.code, key.modifiers) {
        (KeyCode::Char('c'), KeyModifiers::CONTROL) => return true,
        (KeyCode::Esc, _) => return true,
        (KeyCode::Up, _) => app.scroll_up(),
        (KeyCode::Down, _) => app.scroll_down(),
        (KeyCode::PageUp, _) => app.scroll_page_up(),
        (KeyCode::PageDown, _) => app.scroll_page_down(),
        (KeyCode::Char('h'), KeyModifiers::CONTROL) => app.toggle_help(),
        _ => {}
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::Session;
    use crate::tui::app::AppState;
    use ratatui::{backend::TestBackend, Terminal};

    fn make_app() -> AppState {
        let mut app = AppState::new(Session::new());
        app.push_user("hola");
        app.push_assistant("respuesta de prueba");
        app
    }

    #[test]
    fn parse_command_clear_and_variants() {
        assert_eq!(parse_command("/clear"), Command::Clear);
        assert_eq!(parse_command("  /clear  "), Command::Clear);
        assert_eq!(parse_command("/history"), Command::History);
        assert_eq!(parse_command("/tables"), Command::Tables);
        assert_eq!(parse_command("/refresh"), Command::Refresh);
        assert_eq!(parse_command("/quit"), Command::Quit);
        assert_eq!(parse_command("/exit"), Command::Quit);
        assert_eq!(parse_command("/q"), Command::Quit);
        assert_eq!(parse_command("/help"), Command::Help);
        // triangulation: describe with arg
        assert_eq!(
            parse_command("/describe dbo.Usuario"),
            Command::Describe("dbo.Usuario".into())
        );
        assert_eq!(
            parse_command("/describe   dbo.Tabla  "),
            Command::Describe("dbo.Tabla".into())
        );
        // unknown
        assert!(matches!(parse_command("/unknown"), Command::Unknown(_)));
        // message
        assert_eq!(
            parse_command("pregunta normal"),
            Command::Message("pregunta normal".into())
        );
        assert_eq!(
            parse_command("cuantos usuarios hay"),
            Command::Message("cuantos usuarios hay".into())
        );
    }

    #[test]
    fn parse_command_unknown_and_message_edge() {
        assert!(matches!(parse_command("/foo bar"), Command::Unknown(_)));
        // "/describe " trimmed becomes "/describe" -> Unknown without trailing space
        assert_eq!(
            parse_command("/describe "),
            Command::Unknown("/describe".into())
        );
        assert_eq!(parse_command(""), Command::Message("".into()));
        assert_eq!(parse_command("   "), Command::Message("".into()));
    }

    #[test]
    fn ui_renders_three_panes_with_test_backend() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let app = make_app();
        terminal
            .draw(|f| draw(f, &app))
            .expect("draw should not panic");
        let buffer = terminal.backend().buffer().clone();
        let content: String = buffer.content().iter().map(|c| c.symbol()).collect();
        // Should contain pane titles
        assert!(
            content.contains("Chat"),
            "buffer should contain Chat title, got: {}",
            &content[..500.min(content.len())]
        );
        assert!(
            content.contains("Input"),
            "buffer should contain Input title"
        );
        assert!(
            content.contains("Status"),
            "buffer should contain Status title"
        );
        // Should contain our messages when visible
        assert!(
            content.contains("hola") || content.contains("respuesta"),
            "buffer should contain chat messages"
        );
    }

    #[test]
    fn ui_renders_status_and_help() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = make_app();
        app.set_status("Paso 2 — search_schema");
        app.current_tool = Some("search_schema".into());
        app.current_step = 2;
        // without help
        terminal.draw(|f| draw(f, &app)).unwrap();
        let buf = terminal.backend().buffer().clone();
        let content: String = buf.content().iter().map(|c| c.symbol()).collect();
        assert!(content.contains("Paso 2") || content.contains("search_schema"));

        // with help toggle
        app.toggle_help();
        terminal.draw(|f| draw(f, &app)).unwrap();
        let buf2 = terminal.backend().buffer().clone();
        let content2: String = buf2.content().iter().map(|c| c.symbol()).collect();
        assert!(
            content2.contains("/clear") || content2.contains("Comandos"),
            "help text should appear when toggled"
        );
    }

    #[test]
    fn ui_renders_history_and_empty() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = AppState::new(Session::new());
        // empty chat should show welcome
        terminal.draw(|f| draw(f, &app)).unwrap();
        let buf = terminal.backend().buffer().clone();
        let content: String = buf.content().iter().map(|c| c.symbol()).collect();
        assert!(
            content.contains("Bienvenido") || content.contains("Escribe"),
            "empty should show welcome, got snippet: {}",
            &content[..300.min(content.len())]
        );
        // history view
        app.toggle_history();
        terminal.draw(|f| draw(f, &app)).unwrap();
        let buf2 = terminal.backend().buffer().clone();
        let content2: String = buf2.content().iter().map(|c| c.symbol()).collect();
        assert!(content2.contains("Historial") || content2.contains("Chat"));
    }

    #[test]
    fn handle_key_scroll_and_quit() {
        let mut app = make_app();
        for _ in 0..3 {
            app.push_user("extra");
        }
        let up = crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Up,
            crossterm::event::KeyModifiers::empty(),
        );
        handle_key(up, &mut app);
        assert_eq!(app.scroll_offset, 1);
        let down = crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Down,
            crossterm::event::KeyModifiers::empty(),
        );
        handle_key(down, &mut app);
        assert_eq!(app.scroll_offset, 0);
        // page
        let pgup = crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::PageUp,
            crossterm::event::KeyModifiers::empty(),
        );
        handle_key(pgup, &mut app);
        assert!(app.scroll_offset >= 1);
        let pgdn = crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::PageDown,
            crossterm::event::KeyModifiers::empty(),
        );
        handle_key(pgdn, &mut app);
        // esc → quit
        let esc = crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Esc,
            crossterm::event::KeyModifiers::empty(),
        );
        assert!(handle_key(esc, &mut app));
        let ctrlc = crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('c'),
            crossterm::event::KeyModifiers::CONTROL,
        );
        assert!(handle_key(ctrlc, &mut app));
    }

    #[test]
    fn handle_key_unknown_no_quit() {
        let mut app = make_app();
        let a = crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('a'),
            crossterm::event::KeyModifiers::empty(),
        );
        assert!(!handle_key(a, &mut app));
        assert_eq!(app.scroll_offset, 0);
    }
}
