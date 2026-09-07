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

pub const HELP_TEXT: &str = "Comandos: /clear /history /tables /describe <tabla> /refresh /quit /help | Teclas: Enter enviar, Esc salir, ↑↓ scroll, PgUp/PgDn, Ctrl-C salir";

#[cfg(test)]
mod tests {
    use super::*;

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
}
