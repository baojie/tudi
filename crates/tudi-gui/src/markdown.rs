//! Markdown for the preview pane: CommonMark (plus tables, strikethrough
//! and task lists) turned into runs of text with style names, on the
//! loader thread. The pane maps the names to `GtkTextTag`s
//! ([`create_tags`]); no HTML engine is involved. Raw HTML is dropped,
//! images show their alt text.

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

/// A piece of text and the style tags to apply to it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Run {
    pub text: String,
    pub tags: Vec<&'static str>,
}

const HEADINGS: [&str; 6] = ["md-h1", "md-h2", "md-h3", "md-h4", "md-h5", "md-h6"];
const INDENTS: [&str; 6] = [
    "md-indent-1",
    "md-indent-2",
    "md-indent-3",
    "md-indent-4",
    "md-indent-5",
    "md-indent-6",
];
const BOLD: &str = "md-bold";
const ITALIC: &str = "md-italic";
const STRIKE: &str = "md-strike";
const CODE: &str = "md-code";
const PRE: &str = "md-pre";
const LINK: &str = "md-link";
const QUOTE: &str = "md-quote";
const DIM: &str = "md-dim";

#[derive(Default)]
struct Out {
    runs: Vec<Run>,
    /// Trailing newlines of the output so far.
    newlines: usize,
    bold: u32,
    italic: u32,
    strike: u32,
    link: u32,
    heading: Option<usize>,
    pre: bool,
    quote: usize,
    /// One entry per open list: the next number of an ordered list.
    lists: Vec<Option<u64>>,
    /// In a table header row.
    head: bool,
    cell: usize,
}

impl Out {
    fn tags(&self) -> Vec<&'static str> {
        let mut t = Vec::new();
        if let Some(h) = self.heading {
            t.push(HEADINGS[h]);
        }
        if self.bold > 0 || self.head {
            t.push(BOLD);
        }
        if self.italic > 0 {
            t.push(ITALIC);
        }
        if self.strike > 0 {
            t.push(STRIKE);
        }
        if self.link > 0 {
            t.push(LINK);
        }
        if self.pre {
            t.push(PRE);
        }
        if self.quote > 0 {
            t.push(QUOTE);
        }
        let depth = self.lists.len() + self.quote;
        if depth > 0 {
            t.push(INDENTS[(depth - 1).min(INDENTS.len() - 1)]);
        }
        t
    }

    fn push(&mut self, text: &str, extra: &[&'static str]) {
        if text.is_empty() {
            return;
        }
        let mut tags = self.tags();
        tags.extend_from_slice(extra);
        match self.runs.last_mut() {
            Some(last) if last.tags == tags => last.text.push_str(text),
            _ => self.runs.push(Run {
                text: text.to_string(),
                tags,
            }),
        }
        let trailing = text.bytes().rev().take_while(|&b| b == b'\n').count();
        self.newlines = if trailing == text.len() {
            self.newlines + trailing
        } else {
            trailing
        };
    }

    /// End the current line, and leave `n - 1` blank lines (none at the
    /// start of the text).
    fn lines(&mut self, n: usize) {
        if self.runs.is_empty() {
            return;
        }
        while self.newlines < n {
            // Plain newline: block tags (margins, backgrounds) end here.
            match self.runs.last_mut() {
                Some(last) if last.tags.is_empty() => last.text.push('\n'),
                _ => self.runs.push(Run {
                    text: "\n".into(),
                    tags: Vec::new(),
                }),
            }
            self.newlines += 1;
        }
    }
}

/// Render `src` into styled runs.
pub fn render(src: &str) -> Vec<Run> {
    let opts = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let mut o = Out::default();
    for ev in Parser::new_ext(src, opts) {
        match ev {
            Event::Start(tag) => match tag {
                Tag::Paragraph => {
                    // Paragraphs in a list item follow its marker.
                    if o.lists.is_empty() || o.newlines > 0 {
                        o.lines(if o.lists.is_empty() { 2 } else { 1 });
                    }
                }
                Tag::Heading { level, .. } => {
                    o.lines(2);
                    o.heading = Some(match level {
                        HeadingLevel::H1 => 0,
                        HeadingLevel::H2 => 1,
                        HeadingLevel::H3 => 2,
                        HeadingLevel::H4 => 3,
                        HeadingLevel::H5 => 4,
                        HeadingLevel::H6 => 5,
                    });
                }
                Tag::BlockQuote(_) => {
                    o.lines(2);
                    o.quote += 1;
                }
                Tag::CodeBlock(kind) => {
                    o.lines(2);
                    if let CodeBlockKind::Fenced(lang) = kind
                        && !lang.is_empty()
                    {
                        o.push(&lang, &[DIM]);
                        o.lines(1);
                    }
                    o.pre = true;
                }
                Tag::List(start) => {
                    if o.lists.is_empty() {
                        o.lines(2);
                    }
                    o.lists.push(start);
                }
                Tag::Item => {
                    o.lines(1);
                    let marker = match o.lists.last_mut() {
                        Some(Some(n)) => {
                            let m = format!("{n}. ");
                            *n += 1;
                            m
                        }
                        _ => "• ".into(),
                    };
                    o.push(&marker, &[]);
                }
                Tag::Emphasis => o.italic += 1,
                Tag::Strong => o.bold += 1,
                Tag::Strikethrough => o.strike += 1,
                Tag::Link { .. } => o.link += 1,
                Tag::Image { .. } => {
                    o.italic += 1;
                    o.push("[", &[DIM]);
                }
                Tag::Table(_) => o.lines(2),
                Tag::TableHead => {
                    o.head = true;
                    o.cell = 0;
                }
                Tag::TableRow => {
                    o.lines(1);
                    o.cell = 0;
                }
                Tag::TableCell => {
                    if o.cell > 0 {
                        o.push("  │  ", &[DIM]);
                    }
                    o.cell += 1;
                }
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Paragraph => {}
                TagEnd::Heading(_) => {
                    o.heading = None;
                    o.lines(1);
                }
                TagEnd::BlockQuote(_) => {
                    o.quote = o.quote.saturating_sub(1);
                    o.lines(1);
                }
                TagEnd::CodeBlock => {
                    // The block's own last newline ends it.
                    o.pre = false;
                    o.lines(1);
                }
                TagEnd::List(_) => {
                    o.lists.pop();
                    o.lines(1);
                }
                TagEnd::Emphasis => o.italic = o.italic.saturating_sub(1),
                TagEnd::Strong => o.bold = o.bold.saturating_sub(1),
                TagEnd::Strikethrough => o.strike = o.strike.saturating_sub(1),
                TagEnd::Link => o.link = o.link.saturating_sub(1),
                TagEnd::Image => {
                    o.push("]", &[DIM]);
                    o.italic = o.italic.saturating_sub(1);
                }
                TagEnd::TableHead => {
                    o.head = false;
                    o.lines(1);
                }
                TagEnd::Table => o.lines(1),
                _ => {}
            },
            Event::Text(t) => o.push(&t, &[]),
            Event::Code(t) => o.push(&t, &[CODE]),
            Event::SoftBreak => o.push(" ", &[]),
            Event::HardBreak => o.push("\n", &[]),
            Event::Rule => {
                o.lines(2);
                o.push("──────────", &[DIM]);
                o.lines(1);
            }
            Event::TaskListMarker(done) => o.push(if done { "☑ " } else { "☐ " }, &[]),
            Event::FootnoteReference(r) => o.push(&format!("[{r}]"), &[DIM]),
            // Raw HTML and math are not rendered.
            _ => {}
        }
    }
    // No trailing blank lines.
    while let Some(last) = o.runs.last_mut() {
        let t = last.text.trim_end_matches('\n').len();
        if t == 0 {
            o.runs.pop();
        } else {
            last.text.truncate(t);
            break;
        }
    }
    o.runs
}

/// Is this file name Markdown?
pub fn is_markdown(name: &[u8]) -> bool {
    let lower = name.to_ascii_lowercase();
    [&b".md"[..], b".markdown", b".mdown", b".mkd", b".mdx"]
        .iter()
        .any(|ext| lower.ends_with(ext) && lower.len() > ext.len())
}

/// Create the style tags in `table` (once per text buffer).
pub fn create_tags(table: &gtk::TextTagTable) {
    use gtk::{gdk, pango};
    if table.lookup(BOLD).is_some() {
        return;
    }
    let shade = gdk::RGBA::new(0.5, 0.5, 0.5, 0.14);
    let add = |tag: gtk::TextTag| {
        table.add(&tag);
    };
    for (i, name) in HEADINGS.iter().enumerate() {
        let scale = [1.6, 1.4, 1.2, 1.08, 1.0, 1.0][i];
        add(gtk::TextTag::builder()
            .name(*name)
            .weight(700)
            .scale(scale)
            .pixels_above_lines(if i < 2 { 10 } else { 6 })
            .pixels_below_lines(4)
            .build());
    }
    add(gtk::TextTag::builder().name(BOLD).weight(700).build());
    add(gtk::TextTag::builder()
        .name(ITALIC)
        .style(pango::Style::Italic)
        .build());
    add(gtk::TextTag::builder()
        .name(STRIKE)
        .strikethrough(true)
        .build());
    add(gtk::TextTag::builder()
        .name(CODE)
        .family("monospace")
        .background_rgba(&shade)
        .build());
    add(gtk::TextTag::builder()
        .name(PRE)
        .family("monospace")
        .paragraph_background_rgba(&shade)
        .wrap_mode(gtk::WrapMode::None)
        .left_margin(12)
        .build());
    add(gtk::TextTag::builder()
        .name(LINK)
        .foreground_rgba(&gdk::RGBA::new(0.21, 0.52, 0.89, 1.0))
        .underline(pango::Underline::Single)
        .build());
    add(gtk::TextTag::builder()
        .name(QUOTE)
        .foreground_rgba(&gdk::RGBA::new(0.5, 0.5, 0.5, 1.0))
        .build());
    add(gtk::TextTag::builder()
        .name(DIM)
        .foreground_rgba(&gdk::RGBA::new(0.5, 0.5, 0.5, 1.0))
        .build());
    for (i, name) in INDENTS.iter().enumerate() {
        add(gtk::TextTag::builder()
            .name(*name)
            .left_margin(8 + 18 * (i as i32 + 1))
            .build());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(runs: &[Run]) -> String {
        runs.iter().map(|r| r.text.as_str()).collect()
    }

    fn tags_of<'a>(runs: &'a [Run], needle: &str) -> &'a [&'static str] {
        &runs.iter().find(|r| r.text.contains(needle)).unwrap().tags
    }

    #[test]
    fn headings_paragraphs_and_inline_styles() {
        let r = render("# Title\n\nSome **bold**, *italic*, ~~gone~~ and `code`.\nNext line.\n");
        assert_eq!(
            text(&r),
            "Title\n\nSome bold, italic, gone and code. Next line."
        );
        assert_eq!(tags_of(&r, "Title"), ["md-h1"]);
        assert_eq!(tags_of(&r, "bold"), ["md-bold"]);
        assert_eq!(tags_of(&r, "italic"), ["md-italic"]);
        assert_eq!(tags_of(&r, "gone"), ["md-strike"]);
        assert_eq!(tags_of(&r, "code"), ["md-code"]);
    }

    #[test]
    fn lists_nested_and_ordered() {
        let r = render("- one\n- two\n  1. a\n  2. b\n- [x] done\n");
        assert_eq!(text(&r), "• one\n• two\n1. a\n2. b\n• ☑ done");
        assert_eq!(tags_of(&r, "one"), ["md-indent-1"]);
        assert_eq!(tags_of(&r, "a"), ["md-indent-2"]);
    }

    #[test]
    fn code_blocks_quotes_links_rules() {
        let r = render(
            "Intro\n\n```rust\nfn main() {}\n```\n\n> quoted\n\n[site](https://example.com)\n\n---\n\nEnd",
        );
        assert_eq!(
            text(&r),
            "Intro\n\nrust\nfn main() {}\n\nquoted\n\nsite\n\n──────────\n\nEnd"
        );
        assert_eq!(tags_of(&r, "fn main"), ["md-pre"]);
        assert_eq!(tags_of(&r, "quoted"), ["md-quote", "md-indent-1"]);
        assert_eq!(tags_of(&r, "site"), ["md-link"]);
    }

    #[test]
    fn tables_and_html() {
        let r = render("| a | b |\n|---|---|\n| 1 | 2 |\n\n<div>raw</div>\n\n![logo](x.png)\n");
        assert_eq!(text(&r), "a  │  b\n1  │  2\n\n[logo]");
        assert_eq!(tags_of(&r, "a"), ["md-bold"]);
    }

    #[test]
    fn cjk_and_truncated_input() {
        let r = render("## 标题\n\n正文**加粗");
        assert_eq!(text(&r), "标题\n\n正文**加粗");
        assert!(render("").is_empty());
    }

    #[test]
    fn markdown_names() {
        assert!(is_markdown(b"README.md"));
        assert!(is_markdown(b"notes.Markdown"));
        assert!(!is_markdown(b".md"));
        assert!(!is_markdown(b"x.txt"));
    }
}
