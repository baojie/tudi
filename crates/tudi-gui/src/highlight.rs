//! Syntax highlighting for the preview pane (ADR-031): code files and
//! fenced code blocks in Markdown, on the loader thread, with syntect's
//! bundled grammars and themes. The result is runs of text with an
//! optional colour (and bold / italic), applied as text tags by the pane.
//!
//! Colours equal to the theme's default foreground are left unset, so plain
//! code text follows the window's own text colour.

use std::sync::OnceLock;

use syntect::easy::HighlightLines;
use syntect::highlighting::{Color, FontStyle, Theme, ThemeSet};
use syntect::parsing::{SyntaxReference, SyntaxSet};
use syntect::util::LinesWithEndings;

use crate::markdown::Run;

/// Lines highlighted at most; the rest is shown plain (fancy-regex grammars
/// can be slow on huge generated files).
const MAX_LINES: usize = 4000;
/// Longer lines (minified code) are not highlighted.
const MAX_LINE: usize = 2000;

fn syntaxes() -> &'static SyntaxSet {
    static S: OnceLock<SyntaxSet> = OnceLock::new();
    S.get_or_init(SyntaxSet::load_defaults_newlines)
}

fn theme(dark: bool) -> &'static Theme {
    static T: OnceLock<ThemeSet> = OnceLock::new();
    let set = T.get_or_init(ThemeSet::load_defaults);
    &set.themes[if dark {
        "base16-ocean.dark"
    } else {
        "InspiredGitHub"
    }]
}

/// The grammar for a code block's info string (`rust`, `py`, `sh`, …).
pub fn syntax_for_lang(lang: &str) -> Option<&'static SyntaxReference> {
    let token = lang.split([' ', ',', '{']).next()?.trim();
    if token.is_empty() {
        return None;
    }
    let ss = syntaxes();
    ss.find_syntax_by_token(token)
        .or_else(|| ss.find_syntax_by_token(&token.to_ascii_lowercase()))
        .filter(|s| s.name != "Plain Text")
}

/// The grammar for a file: by extension (or whole name, e.g. `Makefile`),
/// else by its first line (`#!/bin/sh`, `<?xml`).
pub fn syntax_for_file(name: &str, text: &str) -> Option<&'static SyntaxReference> {
    let ss = syntaxes();
    let ext = name.rsplit_once('.').map_or(name, |(_, e)| e);
    ss.find_syntax_by_extension(ext)
        .or_else(|| ss.find_syntax_by_extension(name))
        .or_else(|| ss.find_syntax_by_first_line(text.lines().next().unwrap_or("")))
        .filter(|s| s.name != "Plain Text")
}

fn rgb(c: Color) -> u32 {
    u32::from(c.r) << 16 | u32::from(c.g) << 8 | u32::from(c.b)
}

/// Highlight `code` with `syntax`; each run also gets `extra` tags.
pub fn highlight(
    code: &str,
    syntax: &SyntaxReference,
    dark: bool,
    extra: &[&'static str],
) -> Vec<Run> {
    let ss = syntaxes();
    let theme = theme(dark);
    let default_fg = theme.settings.foreground.map(rgb);
    let mut h = HighlightLines::new(syntax, theme);
    let mut out: Vec<Run> = Vec::new();
    let mut push = |text: &str, color: Option<u32>, tags: Vec<&'static str>| match out.last_mut() {
        Some(last) if last.color == color && last.tags == tags => last.text.push_str(text),
        _ => out.push(Run {
            text: text.to_string(),
            tags,
            color,
        }),
    };
    for (n, line) in LinesWithEndings::from(code).enumerate() {
        let plain = n >= MAX_LINES || line.len() > MAX_LINE;
        let ranges = if plain {
            None
        } else {
            h.highlight_line(line, ss).ok()
        };
        let Some(ranges) = ranges else {
            push(line, None, extra.to_vec());
            continue;
        };
        for (style, text) in ranges {
            let mut tags = extra.to_vec();
            if style.font_style.contains(FontStyle::BOLD) {
                tags.push("md-bold");
            }
            if style.font_style.contains(FontStyle::ITALIC) {
                tags.push("md-italic");
            }
            let fg = rgb(style.foreground);
            let color = (Some(fg) != default_fg).then_some(fg);
            // Newlines carry no colour, so block tags end cleanly.
            match text.strip_suffix('\n') {
                Some(body) => {
                    push(body, color, tags);
                    push("\n", None, extra.to_vec());
                }
                None => push(text, color, tags),
            }
        }
    }
    out.retain(|r| !r.text.is_empty());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_grammars() {
        assert_eq!(syntax_for_lang("rust").unwrap().name, "Rust");
        assert_eq!(syntax_for_lang("Python").unwrap().name, "Python");
        assert_eq!(
            syntax_for_lang("sh").unwrap().name,
            "Bourne Again Shell (bash)"
        );
        assert!(syntax_for_lang("").is_none());
        assert!(syntax_for_lang("no-such-language").is_none());
        assert_eq!(syntax_for_file("main.rs", "").unwrap().name, "Rust");
        assert_eq!(syntax_for_file("Makefile", "").unwrap().name, "Makefile");
        assert_eq!(
            syntax_for_file("run", "#!/usr/bin/env python3\n")
                .unwrap()
                .name,
            "Python"
        );
        assert!(syntax_for_file("notes.txt", "hello").is_none());
    }

    #[test]
    fn colours_keywords_and_keeps_text() {
        let code = "fn main() {\n    let s = \"hi\"; // note\n}\n";
        let rs = syntax_for_lang("rust").unwrap();
        for dark in [false, true] {
            let runs = highlight(code, rs, dark, &["md-pre"]);
            let text: String = runs.iter().map(|r| r.text.as_str()).collect();
            assert_eq!(text, code);
            let color_of =
                |needle: &str| runs.iter().find(|r| r.text.contains(needle)).unwrap().color;
            assert!(color_of("fn").is_some());
            // Some themes colour the quotes too, some only the contents.
            assert!(color_of("hi").is_some());
            assert_ne!(color_of("fn"), color_of("hi"));
            assert!(color_of("// note").is_some());
            assert!(runs.iter().all(|r| r.tags.contains(&"md-pre")));
            // Newlines are separate, uncoloured runs.
            assert!(
                runs.iter()
                    .filter(|r| r.text.contains('\n'))
                    .all(|r| r.color.is_none())
            );
        }
    }

    #[test]
    fn long_lines_stay_plain() {
        let line = format!("let x = \"{}\";\n", "a".repeat(MAX_LINE));
        let runs = highlight(&line, syntax_for_lang("rust").unwrap(), false, &[]);
        assert_eq!(runs.len(), 1);
        assert!(runs[0].color.is_none());
    }
}
