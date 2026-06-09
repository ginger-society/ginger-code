//! Token-based log line syntax highlighting.
//!
//! Splits each log line into styled spans without a full parser.
//! Uses syntect only for its bundled themes (Monokai / base16) so we get
//! a coherent palette that matches a real code editor.

use eframe::egui;
use syntect::highlighting::{ThemeSet, Theme};
use syntect::easy::HighlightLines;
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;

// ── Theme colours (extracted once at startup) ─────────────────────────────────

pub struct LogPalette {
    pub error:     egui::Color32,   // ERROR / panic / fatal
    pub warn:      egui::Color32,   // WARN
    pub info:      egui::Color32,   // INFO
    pub debug:     egui::Color32,   // DEBUG / TRACE
    pub timestamp: egui::Color32,   // ISO-8601 / epoch timestamps
    pub bracket:   egui::Color32,   // [foo] {bar}
    pub path:      egui::Color32,   // file::module::paths
    pub number:    egui::Color32,   // bare numbers / durations
    pub muted:     egui::Color32,   // dim foreground (default)
    pub fg:        egui::Color32,   // normal foreground
}

impl LogPalette {
    /// Build from syntect's bundled Monokai theme.
    pub fn from_monokai() -> Self {
        // Monokai colours (hardcoded — avoids loading the full theme at runtime
        // and keeps things deterministic across syntect versions).
        Self {
            error:     egui::Color32::from_rgb(249,  38,  114), // Monokai red
            warn:      egui::Color32::from_rgb(230, 219,  116), // Monokai yellow
            info:      egui::Color32::from_rgb( 97, 214, 214),  // Monokai cyan  
            debug:     egui::Color32::from_rgb(117, 113, 185),  // Monokai purple
            timestamp: egui::Color32::from_rgb(102, 217, 239),  // Monokai light-blue
            bracket:   egui::Color32::from_rgb(117, 113, 185),  // Monokai purple
            path:      egui::Color32::from_rgb(166, 226,  46),  // Monokai green
            number:    egui::Color32::from_rgb(174, 129, 255),  // Monokai light-purple
            muted:     egui::Color32::from_rgb(117, 117, 117),
            fg:        egui::Color32::from_rgb(248, 248, 242),  // Monokai fg
        }
    }
}

// ── Token types ───────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct LogSpan {
    pub text:  String,
    pub color: egui::Color32,
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Tokenise a single log line into coloured spans.
pub fn highlight_line(line: &str, palette: &LogPalette) -> Vec<LogSpan> {
    // Determine the base "level" colour for the whole line so that
    // unrecognised tokens inherit a sensible tint.
    let line_color = line_level_color(line, palette);

    let mut spans = Vec::new();
    let mut rest  = line;

    while !rest.is_empty() {
        // ── Timestamp: 2024-01-15T12:34:56 / 2024-01-15 12:34:56 / epoch ms ──
        if let Some(m) = match_prefix(rest, is_timestamp_char, 10) {
            let (tok, tail) = rest.split_at(m);
            spans.push(LogSpan { text: tok.to_owned(), color: palette.timestamp });
            rest = tail;
            continue;
        }

        // ── Log level keywords ────────────────────────────────────────────────
        if let Some((tok, tail, color)) = match_level_keyword(rest, palette) {
            spans.push(LogSpan { text: tok, color });
            rest = tail;
            continue;
        }

        // ── Bracketed tokens: [INFO] {key=val} ───────────────────────────────
        if rest.starts_with('[') || rest.starts_with('{') {
            if let Some(close) = find_close(rest) {
                let (tok, tail) = rest.split_at(close + 1);
                // The content inside might itself contain a level word
                let inner_color = if tok.to_uppercase().contains("ERROR") || tok.to_uppercase().contains("FATAL") {
                    palette.error
                } else if tok.to_uppercase().contains("WARN") {
                    palette.warn
                } else if tok.to_uppercase().contains("INFO") {
                    palette.info
                } else if tok.to_uppercase().contains("DEBUG") || tok.to_uppercase().contains("TRACE") {
                    palette.debug
                } else {
                    palette.bracket
                };
                spans.push(LogSpan { text: tok.to_owned(), color: inner_color });
                rest = tail;
                continue;
            }
        }

        // ── Module path: foo::bar::baz ────────────────────────────────────────
        if let Some(m) = match_path(rest) {
            let (tok, tail) = rest.split_at(m);
            spans.push(LogSpan { text: tok.to_owned(), color: palette.path });
            rest = tail;
            continue;
        }

        // ── Numbers (standalone integers / floats / durations like 12ms) ──────
        if let Some(m) = match_number(rest) {
            let (tok, tail) = rest.split_at(m);
            spans.push(LogSpan { text: tok.to_owned(), color: palette.number });
            rest = tail;
            continue;
        }

        // ── Key=value pairs ───────────────────────────────────────────────────
        if let Some(m) = match_kv(rest) {
            let (tok, tail) = rest.split_at(m);
            // Split around '='
            if let Some(eq) = tok.find('=') {
                spans.push(LogSpan { text: tok[..eq + 1].to_owned(), color: palette.muted });
                spans.push(LogSpan { text: tok[eq + 1..].to_owned(), color: palette.number });
            } else {
                spans.push(LogSpan { text: tok.to_owned(), color: line_color });
            }
            rest = tail;
            continue;
        }

        // ── Default: consume up to next token boundary ────────────────────────
        let boundary = rest
            .find(|c: char| c == '[' || c == '{' || c == ' ')
            .unwrap_or(rest.len());
        let (tok, tail) = rest.split_at(boundary.max(1));
        spans.push(LogSpan { text: tok.to_owned(), color: line_color });
        rest = tail;
    }

    spans
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn line_level_color(line: &str, p: &LogPalette) -> egui::Color32 {
    let up = line.to_uppercase();
    if up.contains("ERROR") || up.contains("FATAL") || up.contains("PANIC") {
        p.error
    } else if up.contains("WARN") {
        p.warn
    } else if up.contains("INFO") {
        p.info
    } else if up.contains("DEBUG") {
        p.debug
    } else if up.contains("TRACE") {
        p.debug
    } else {
        p.fg
    }
}

fn match_level_keyword<'a>(
    s: &'a str,
    p: &LogPalette,
) -> Option<(String, &'a str, egui::Color32)> {
    const LEVELS: &[(&str, bool)] = &[
        ("ERROR", true), ("FATAL", true), ("PANIC", true),
        ("WARN",  true), ("WARNING", true),
        ("INFO",  true),
        ("DEBUG", true), ("TRACE", true),
        // lowercase variants
        ("error", true), ("fatal", true), ("panic", true),
        ("warn",  true), ("warning", true),
        ("info",  true),
        ("debug", true), ("trace", true),
    ];
    for (kw, _) in LEVELS {
        if s.starts_with(kw) {
            let after = &s[kw.len()..];
            // only match as a keyword if followed by whitespace, ':', ']', or end
            if after.is_empty() || matches!(after.chars().next(), Some(' ' | ':' | ']' | ')')) {
                let color = match kw.to_uppercase().as_str() {
                    "ERROR" | "FATAL" | "PANIC" => p.error,
                    "WARN"  | "WARNING"          => p.warn,
                    "INFO"                       => p.info,
                    _                            => p.debug,
                };
                return Some((kw.to_string(), after, color));
            }
        }
    }
    None
}

fn is_timestamp_char(c: char) -> bool {
    c.is_ascii_digit() || c == '-' || c == ':' || c == 'T' || c == '.' || c == 'Z' || c == '+'
}

fn match_prefix(s: &str, pred: impl Fn(char) -> bool, min_len: usize) -> Option<usize> {
    let len = s.chars().take_while(|&c| pred(c)).map(|c| c.len_utf8()).sum::<usize>();
    if len >= min_len { Some(len) } else { None }
}

fn find_close(s: &str) -> Option<usize> {
    let close = if s.starts_with('[') { ']' } else { '}' };
    s.find(close)
}

fn match_path(s: &str) -> Option<usize> {
    // word::word (at least one "::")
    if !s.chars().next().map(|c| c.is_alphabetic() || c == '_').unwrap_or(false) {
        return None;
    }
    let candidate: String = s.chars()
        .take_while(|&c| c.is_alphanumeric() || c == '_' || c == ':')
        .collect();
    if candidate.contains("::") && candidate.len() > 4 {
        Some(candidate.len())
    } else {
        None
    }
}

fn match_number(s: &str) -> Option<usize> {
    if !s.starts_with(|c: char| c.is_ascii_digit()) { return None; }
    let len = s.chars()
        .take_while(|&c| c.is_ascii_digit() || c == '.' || c == 'm' || c == 's' || c == 'µ')
        .map(|c| c.len_utf8())
        .sum::<usize>();
    if len > 0 { Some(len) } else { None }
}

fn match_kv(s: &str) -> Option<usize> {
    // key=value where key is [a-zA-Z_][a-zA-Z0-9_.-]*
    if !s.starts_with(|c: char| c.is_alphabetic() || c == '_') { return None; }
    let key_len = s.chars()
        .take_while(|&c| c.is_alphanumeric() || c == '_' || c == '-' || c == '.')
        .map(|c| c.len_utf8())
        .sum::<usize>();
    let after_key = &s[key_len..];
    if !after_key.starts_with('=') { return None; }
    let val_len = after_key[1..].chars()
        .take_while(|&c| c != ' ' && c != ',' && c != ']' && c != '}')
        .map(|c| c.len_utf8())
        .sum::<usize>();
    Some(key_len + 1 + val_len)
}