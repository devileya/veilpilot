//! A small markdown renderer for answers. Deliberately not CommonMark:
//! answers are short, and an unclosed fence mid-stream must still render as a
//! code block rather than fail.

use std::ops::Range;
use std::sync::LazyLock;

use gpui::{
    AnyElement, FontWeight, InteractiveText, IntoElement, ParentElement as _, SharedString, StrikethroughStyle,
    Styled as _, StyledText, TextRun, UnderlineStyle, div, px,
};
use regex::Regex;

use super::theme;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Span {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    pub strike: bool,
    pub link: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Block {
    Paragraph(Vec<Span>),
    Heading(Vec<Span>),
    List { ordered: bool, items: Vec<Vec<Span>> },
    Quote(Vec<Span>),
    Code { lang: String, text: String },
    Rule,
}

static FENCE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*```(\w*)\s*$").unwrap());
static FENCE_END: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*```\s*$").unwrap());
static HEADING: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(#{1,6})\s+(.*)$").unwrap());
static RULE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*(-{3,}|\*{3,}|_{3,})\s*$").unwrap());
static BULLET: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*[-*+]\s+(.*)$").unwrap());
static NUMBER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*\d+[.)]\s+(.*)$").unwrap());
static QUOTE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*>\s?(.*)$").unwrap());

fn blank(line: &str) -> bool {
    line.trim().is_empty()
}

fn starts_block(line: &str) -> bool {
    blank(line)
        || FENCE.is_match(line)
        || HEADING.is_match(line)
        || BULLET.is_match(line)
        || NUMBER.is_match(line)
        || QUOTE.is_match(line)
        || RULE.is_match(line)
}

pub fn parse(source: &str) -> Vec<Block> {
    let lines: Vec<&str> = source.split('\n').collect();
    let mut out = Vec::new();
    let mut list: Option<(bool, Vec<Vec<Span>>)> = None;
    let mut i = 0;

    fn flush(list: &mut Option<(bool, Vec<Vec<Span>>)>, out: &mut Vec<Block>) {
        if let Some((ordered, items)) = list.take() {
            out.push(Block::List { ordered, items });
        }
    }

    while i < lines.len() {
        let line = lines[i];

        if let Some(caps) = FENCE.captures(line) {
            flush(&mut list, &mut out);
            let lang = caps[1].to_string();
            let mut body = Vec::new();
            i += 1;
            while i < lines.len() && !FENCE_END.is_match(lines[i]) {
                body.push(lines[i]);
                i += 1;
            }
            i += 1; // closing fence, or past the end while still streaming
            out.push(Block::Code { lang, text: body.join("\n") });
            continue;
        }
        if blank(line) {
            flush(&mut list, &mut out);
            i += 1;
            continue;
        }
        if RULE.is_match(line) {
            flush(&mut list, &mut out);
            out.push(Block::Rule);
            i += 1;
            continue;
        }
        if let Some(caps) = HEADING.captures(line) {
            flush(&mut list, &mut out);
            out.push(Block::Heading(inline(&caps[2])));
            i += 1;
            continue;
        }
        let bullet = BULLET.captures(line).map(|c| (false, c[1].to_string()));
        let numbered = NUMBER.captures(line).map(|c| (true, c[1].to_string()));
        if let Some((ordered, content)) = bullet.or(numbered) {
            match &mut list {
                Some((kind, items)) if *kind == ordered => items.push(inline(&content)),
                _ => {
                    flush(&mut list, &mut out);
                    list = Some((ordered, vec![inline(&content)]));
                }
            }
            i += 1;
            continue;
        }
        if QUOTE.is_match(line) {
            flush(&mut list, &mut out);
            let mut body = Vec::new();
            while i < lines.len() {
                let Some(caps) = QUOTE.captures(lines[i]) else { break };
                body.push(caps[1].to_string());
                i += 1;
            }
            out.push(Block::Quote(inline(&body.join(" "))));
            continue;
        }

        flush(&mut list, &mut out);
        let mut paragraph = vec![line];
        i += 1;
        while i < lines.len() && !starts_block(lines[i]) {
            paragraph.push(lines[i]);
            i += 1;
        }
        out.push(Block::Paragraph(inline(&paragraph.join("\n"))));
    }
    flush(&mut list, &mut out);
    out
}

/* ---------- inline ---------- */

static LINK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[([^\]\n]+)\]\((https?://[^\s)]+)\)").unwrap());

/// Inline spans. Code spans are pulled out first so emphasis never rewrites
/// code; links next; then bold, italic and strikethrough over what is left.
pub fn inline(text: &str) -> Vec<Span> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find('`') {
        let after = &rest[start + 1..];
        match after.find(['`', '\n']) {
            Some(end) if after.as_bytes()[end] == b'`' && end > 0 => {
                links(&rest[..start], &mut out);
                out.push(Span { text: after[..end].to_string(), code: true, ..Default::default() });
                rest = &after[end + 1..];
            }
            _ => {
                links(&rest[..start + 1], &mut out);
                rest = after;
            }
        }
    }
    links(rest, &mut out);
    merge(out)
}

fn links(text: &str, out: &mut Vec<Span>) {
    let mut last = 0;
    for caps in LINK.captures_iter(text) {
        let whole = caps.get(0).unwrap();
        emphasis(&text[last..whole.start()], Style::default(), out);
        let mut label = Vec::new();
        emphasis(&caps[1], Style::default(), &mut label);
        for mut span in label {
            span.link = Some(caps[2].to_string());
            out.push(span);
        }
        last = whole.end();
    }
    emphasis(&text[last..], Style::default(), out);
}

#[derive(Clone, Copy, Default)]
struct Style {
    bold: bool,
    italic: bool,
    strike: bool,
}

/// Recursive emphasis: `**bold**`, `*italic*` / `_italic_` (only after
/// whitespace or line start, so snake_case survives), `~~strike~~`.
fn emphasis(text: &str, style: Style, out: &mut Vec<Span>) {
    if text.is_empty() {
        return;
    }
    let bytes = text.as_bytes();
    let mut plain_start = 0;
    let mut i = 0;
    while i < bytes.len() {
        let prev_ok = i == 0 || matches!(bytes[i - 1], b' ' | b'\t' | b'\n' | b'(');
        let try_delim = |delim: &str, needs_boundary: bool| -> Option<usize> {
            if !text[i..].starts_with(delim) || (needs_boundary && !prev_ok) {
                return None;
            }
            let inner_start = i + delim.len();
            let close = text[inner_start..].find(delim)? + inner_start;
            let inner = &text[inner_start..close];
            if inner.is_empty() || (delim.len() == 1 && inner.contains('\n')) {
                return None;
            }
            if delim == "**" && inner.contains('*') {
                return None;
            }
            Some(close)
        };

        let found = [("**", false), ("~~", false), ("*", true), ("_", true)]
            .iter()
            .find_map(|(d, b)| try_delim(d, *b).map(|close| (*d, close)));

        if let Some((delim, close)) = found {
            push_plain(&text[plain_start..i], style, out);
            let mut inner = style;
            match delim {
                "**" => inner.bold = true,
                "~~" => inner.strike = true,
                _ => inner.italic = true,
            }
            emphasis(&text[i + delim.len()..close], inner, out);
            i = close + delim.len();
            plain_start = i;
        } else {
            i += text[i..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
        }
    }
    push_plain(&text[plain_start..], style, out);
}

fn push_plain(text: &str, style: Style, out: &mut Vec<Span>) {
    if !text.is_empty() {
        out.push(Span {
            text: text.to_string(),
            bold: style.bold,
            italic: style.italic,
            strike: style.strike,
            ..Default::default()
        });
    }
}

fn merge(spans: Vec<Span>) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::new();
    for span in spans {
        if let Some(last) = out.last_mut() {
            let same = last.bold == span.bold
                && last.italic == span.italic
                && last.code == span.code
                && last.strike == span.strike
                && last.link == span.link;
            if same {
                last.text.push_str(&span.text);
                continue;
            }
        }
        out.push(span);
    }
    out
}

/// Plain text of a whole answer, for the copy button.
pub fn plain_text(source: &str) -> String {
    let mut out = Vec::new();
    for block in parse(source) {
        let spans = |s: &[Span]| s.iter().map(|s| s.text.as_str()).collect::<String>();
        match block {
            Block::Paragraph(s) | Block::Heading(s) | Block::Quote(s) => out.push(spans(&s)),
            Block::List { ordered, items } => out.push(
                items
                    .iter()
                    .enumerate()
                    .map(|(n, s)| if ordered { format!("{}. {}", n + 1, spans(s)) } else { format!("• {}", spans(s)) })
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            Block::Code { text, .. } => out.push(text),
            Block::Rule => {}
        }
    }
    out.join("\n\n")
}

/* ---------- rendering ---------- */

pub struct RenderOptions {
    pub id: SharedString,
    /// Append the streaming caret to the last block.
    pub caret: bool,
    pub caret_visible: bool,
}

fn runs_for(
    spans: &[Span],
    base_color: gpui::Hsla,
    heading: bool,
) -> (String, Vec<TextRun>, Vec<(Range<usize>, String)>) {
    let mut text = String::new();
    let mut runs = Vec::new();
    let mut links = Vec::new();
    for span in spans {
        let start = text.len();
        let content = if heading { span.text.to_uppercase() } else { span.text.clone() };
        text.push_str(&content);
        let mut font = if span.code { theme::mono_font() } else { theme::ui_font() };
        if span.bold || heading {
            font = theme::with_weight(font, if heading { FontWeight::SEMIBOLD } else { FontWeight::BOLD });
        }
        if span.italic {
            font = theme::italic(font);
        }
        let color = if heading || span.link.is_some() {
            theme::hsla(theme::lamp())
        } else if span.bold {
            theme::hsla(theme::white())
        } else {
            base_color
        };
        runs.push(TextRun {
            len: content.len(),
            font,
            color,
            background_color: span.code.then(|| theme::hsla(theme::glass())),
            underline: span.link.as_ref().map(|_| UnderlineStyle {
                color: Some(color),
                thickness: px(1.),
                wavy: false,
            }),
            strikethrough: span.strike.then(|| StrikethroughStyle { color: Some(color), thickness: px(1.) }),
        });
        if let Some(href) = &span.link {
            links.push((start..text.len(), href.clone()));
        }
    }
    (text, runs, links)
}

fn caret_run(text: &mut String, runs: &mut Vec<TextRun>, visible: bool) {
    let glyph = "▍";
    text.push_str(glyph);
    runs.push(TextRun {
        len: glyph.len(),
        font: theme::ui_font(),
        color: if visible { theme::hsla(theme::lamp()) } else { gpui::transparent_black() },
        background_color: None,
        underline: None,
        strikethrough: None,
    });
}

fn rich_text(
    id: SharedString,
    spans: &[Span],
    base_color: gpui::Hsla,
    heading: bool,
    caret: Option<bool>,
) -> AnyElement {
    let (mut text, mut runs, links) = runs_for(spans, base_color, heading);
    if let Some(visible) = caret {
        caret_run(&mut text, &mut runs, visible);
    }
    if text.is_empty() {
        return div().into_any_element();
    }
    let styled = StyledText::new(text).with_runs(runs);
    if links.is_empty() {
        return styled.into_any_element();
    }
    let ranges: Vec<Range<usize>> = links.iter().map(|(r, _)| r.clone()).collect();
    let hrefs: Vec<String> = links.into_iter().map(|(_, h)| h).collect();
    InteractiveText::new(id, styled)
        .on_click(ranges, move |ix, _, _| {
            if let Some(href) = hrefs.get(ix) {
                crate::platform::open_external(href);
            }
        })
        .into_any_element()
}

pub fn render(source: &str, opts: RenderOptions) -> Vec<AnyElement> {
    let blocks = parse(source);
    let count = blocks.len();
    let read = theme::hsla(theme::read());
    let mut out = Vec::new();
    for (n, block) in blocks.into_iter().enumerate() {
        let id: SharedString = format!("{}-{n}", opts.id).into();
        let caret = (opts.caret && n + 1 == count).then_some(opts.caret_visible);
        let element = match block {
            Block::Paragraph(spans) => div().child(rich_text(id, &spans, read, false, caret)).into_any_element(),
            Block::Heading(spans) => {
                div().text_size(px(11.5)).child(rich_text(id, &spans, read, true, caret)).into_any_element()
            }
            Block::Quote(spans) => div()
                .pl(px(10.))
                .border_l_1()
                .border_color(theme::edge())
                .child(rich_text(id, &spans, theme::hsla(theme::haze()), false, caret))
                .into_any_element(),
            Block::List { ordered, items } => {
                let n_items = items.len();
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .children(items.into_iter().enumerate().map(|(k, spans)| {
                        let marker = if ordered { format!("{}.", k + 1) } else { "•".into() };
                        let item_caret = caret.filter(|_| k + 1 == n_items);
                        div()
                            .flex()
                            .gap(px(7.))
                            .child(div().flex_none().min_w(px(12.)).text_color(theme::haze()).child(marker))
                            .child(div().flex_1().min_w_0().child(rich_text(
                                format!("{id}-{k}").into(),
                                &spans,
                                read,
                                false,
                                item_caret,
                            )))
                    }))
                    .into_any_element()
            }
            Block::Code { text, .. } => {
                let mut content = text;
                let mut runs = vec![TextRun {
                    len: content.len(),
                    font: theme::mono_font(),
                    color: read,
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                }];
                if let Some(visible) = caret {
                    caret_run(&mut content, &mut runs, visible);
                }
                runs.retain(|r| r.len > 0);
                div()
                    .bg(theme::glass())
                    .border_1()
                    .border_color(theme::edge())
                    .rounded(px(5.))
                    .px(px(11.))
                    .py(px(10.))
                    .text_size(px(11.5))
                    .line_height(px(17.8))
                    .child(if content.is_empty() {
                        div().into_any_element()
                    } else {
                        StyledText::new(content).with_runs(runs).into_any_element()
                    })
                    .into_any_element()
            }
            Block::Rule => div().h(px(1.)).bg(theme::edge()).into_any_element(),
        };
        out.push(element);
    }
    if out.is_empty() && opts.caret {
        out.push(rich_text(format!("{}-caret", opts.id).into(), &[], read, false, Some(opts.caret_visible)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(text: &str) -> Span {
        Span { text: text.into(), ..Default::default() }
    }

    #[test]
    fn blocks() {
        let md = "# Title\n\nPara one\ncontinues\n\n- a\n- b\n1. c\n\n> quoted\n> more\n\n---\n```rust\nfn x() {}\n```";
        let b = parse(md);
        assert_eq!(b[0], Block::Heading(vec![span("Title")]));
        assert_eq!(b[1], Block::Paragraph(vec![span("Para one\ncontinues")]));
        assert_eq!(b[2], Block::List { ordered: false, items: vec![vec![span("a")], vec![span("b")]] });
        assert_eq!(b[3], Block::List { ordered: true, items: vec![vec![span("c")]] });
        assert_eq!(b[4], Block::Quote(vec![span("quoted more")]));
        assert_eq!(b[5], Block::Rule);
        assert_eq!(b[6], Block::Code { lang: "rust".into(), text: "fn x() {}".into() });
    }

    #[test]
    fn unclosed_fence_mid_stream_is_still_code() {
        let b = parse("Fix:\n```js\nconst a = 1\nconst b");
        assert_eq!(b[1], Block::Code { lang: "js".into(), text: "const a = 1\nconst b".into() });
    }

    #[test]
    fn inline_styles() {
        let s = inline(
            "The **root cause** is `cfg.db` in *parseConfig* — see [docs](https://x.dev/a) ~~old~~ snake_case_name",
        );
        assert_eq!(s[0], span("The "));
        assert_eq!(s[1], Span { text: "root cause".into(), bold: true, ..Default::default() });
        assert_eq!(s[3], Span { text: "cfg.db".into(), code: true, ..Default::default() });
        assert_eq!(s[5], Span { text: "parseConfig".into(), italic: true, ..Default::default() });
        assert_eq!(s[7], Span { text: "docs".into(), link: Some("https://x.dev/a".into()), ..Default::default() });
        assert_eq!(s[9], Span { text: "old".into(), strike: true, ..Default::default() });
        assert_eq!(s[10], span(" snake_case_name"));
    }

    #[test]
    fn emphasis_never_rewrites_code() {
        let s = inline("`**not bold**` and **bold**");
        assert_eq!(s[0], Span { text: "**not bold**".into(), code: true, ..Default::default() });
        assert!(s[2].bold);
    }

    #[test]
    fn stray_markers_stay_literal() {
        assert_eq!(inline("2 * 3 = 6 and a ` tick"), vec![span("2 * 3 = 6 and a ` tick")]);
        assert_eq!(inline("**unclosed bold"), vec![span("**unclosed bold")]);
    }

    #[test]
    fn plain_text_for_copy() {
        assert_eq!(plain_text("**A** b\n\n- x\n- y\n\n```\ncode\n```"), "A b\n\n• x\n• y\n\ncode");
    }
}
