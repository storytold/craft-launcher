//! A third-party app's README, drawn from its Markdown without a Markdown engine.
//!
//! The text comes from someone ArtCraft doesn't vouch for, so it is only ever drawn, never run:
//! no scripts, no raw HTML beyond a few harmless tags, links that open in the browser only as
//! web or mail addresses (with the real address in the tooltip), and pictures that are only
//! fetched from GitHub's own hosts (`artcraft_launcher_core::readme`).

use std::collections::HashMap;
use std::sync::Arc;

use artcraft_launcher_core::readme as safe;
use egui::{Color32, RichText, Sense, Ui, vec2};

use crate::details::Picture;
use crate::theme::Tokens;

/// Where a README is: asked for, here, or not to be had.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Readme {
    Loading,
    /// Parsed once, when it arrives, not on every frame.
    Ready(Arc<Vec<Block>>),
    Failed(String),
}

impl Readme {
    pub(crate) fn ready(text: &str) -> Self {
        Self::Ready(Arc::new(blocks(text)))
    }
}

/// A run of text, or a picture, inside a block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Inline {
    Text { text: String, bold: bool, italic: bool, strike: bool, code: bool, link: Option<String> },
    Image { alt: String, src: String, width: Option<u32>, link: Option<String> },
    Break,
}

/// How an item of a list is marked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Marker {
    Bullet,
    /// `1.` as written.
    Number(String),
    Task(bool),
}

/// One block of a README, classified.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Block {
    Heading(u8, Vec<Inline>),
    Paragraph(Vec<Inline>),
    Item { depth: u8, marker: Marker, content: Vec<Inline> },
    Quote(Vec<Inline>),
    Code(String),
    Rule,
    Table { header: Vec<Vec<Inline>>, rows: Vec<Vec<Vec<Inline>>> },
}

/// The most blocks kept: a huge README must not make the dialog crawl.
const MAX_BLOCKS: usize = 600;
/// The most pictures one README gets to show; later ones appear as their description.
const MAX_IMAGES: usize = 40;
/// The most rows of one table.
const MAX_ROWS: usize = 200;
/// The deepest nesting of links and emphasis followed.
const MAX_DEPTH: usize = 6;

// ---------------------------------------------------------------------------------------------
// Inline Markdown
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Default)]
struct Style {
    bold: bool,
    italic: bool,
    strike: bool,
    code: bool,
    link: Option<String>,
}

struct Parser<'a> {
    /// `[label]: address` definitions, lower-cased by label.
    refs: &'a HashMap<String, String>,
    images_left: usize,
}

fn at(c: &[char], i: usize) -> char {
    c.get(i).copied().unwrap_or('\0')
}

fn starts_with(c: &[char], i: usize, s: &str) -> bool {
    s.chars().enumerate().all(|(k, ch)| at(c, i + k) == ch)
}

/// The index of the next `seq` at or after `from`.
fn find_seq(c: &[char], from: usize, seq: &[char]) -> Option<usize> {
    if seq.is_empty() || c.len() < seq.len() {
        return None;
    }
    (from..=c.len() - seq.len()).find(|&i| c.get(i..i + seq.len()) == Some(seq))
}

fn push_text(out: &mut Vec<Inline>, buf: &mut String, st: &Style) {
    if buf.is_empty() {
        return;
    }
    let text = std::mem::take(buf);
    out.push(Inline::Text { text, bold: st.bold, italic: st.italic, strike: st.strike, code: st.code, link: st.link.clone() });
}

/// Appends a character, folding runs of white space into one space.
fn push_char(buf: &mut String, ch: char) {
    if ch.is_whitespace() {
        if !buf.ends_with(' ') {
            buf.push(' ');
        }
    } else {
        buf.push(ch);
    }
}

/// The value of attribute `name` in an HTML tag's text (`img src="a.png" width=60`).
fn attr(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let mut from = 0;
    while let Some(found) = lower.get(from..).and_then(|rest| rest.find(name)) {
        let start = from + found;
        from = start + name.len();
        let before_ok = start == 0 || lower.as_bytes().get(start - 1).is_some_and(|b| b.is_ascii_whitespace());
        let rest = tag.get(start + name.len()..)?.trim_start();
        let Some(value) = rest.strip_prefix('=') else { continue };
        if !before_ok {
            continue;
        }
        let value = value.trim_start();
        let quote = value.chars().next().filter(|q| matches!(q, '"' | '\''));
        return Some(match quote {
            Some(q) => value.get(1..)?.split(q).next()?.to_string(),
            None => value.split(|c: char| c.is_whitespace() || c == '>').next()?.to_string(),
        });
    }
    None
}

fn entity(name: &str) -> Option<char> {
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" | "#39" => '\'',
        "nbsp" => ' ',
        "copy" => '©',
        "reg" => '®',
        "hellip" => '…',
        "mdash" => '—',
        "ndash" => '–',
        _ => {
            let digits = name.strip_prefix('#')?;
            let code = match digits.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => digits.parse().ok()?,
            };
            char::from_u32(code).filter(|c| !c.is_control())?
        }
    })
}

impl Parser<'_> {
    fn inline(&mut self, s: &str) -> Vec<Inline> {
        let chars: Vec<char> = s.chars().collect();
        let mut out = Vec::new();
        self.run(&chars, &Style::default(), &mut out, 0);
        // A break at either end says nothing.
        while matches!(out.last(), Some(Inline::Break)) {
            out.pop();
        }
        while matches!(out.first(), Some(Inline::Break)) {
            out.remove(0);
        }
        out
    }

    /// `[text](address "title")`, `[text][label]` or `[label]` starting at `start` (the `[`):
    /// the text's characters, the address and the index after the whole thing.
    fn link_parts<'c>(&self, c: &'c [char], start: usize) -> Option<(&'c [char], String, usize)> {
        // The `[` at `start` always opens the link, so count it up front.
        let mut depth = 1usize;
        let mut close = None;
        for i in start + 1..c.len().min(start + 2000) {
            match at(c, i) {
                '\\' => {}
                '[' if at(c, i.wrapping_sub(1)) != '\\' => depth += 1,
                ']' if at(c, i.wrapping_sub(1)) != '\\' => {
                    depth -= 1;
                    if depth == 0 {
                        close = Some(i);
                        break;
                    }
                }
                _ => {}
            }
        }
        let close = close?;
        let inner = c.get(start + 1..close)?;
        match at(c, close + 1) {
            '(' => {
                let mut depth = 0usize;
                let mut end = None;
                for i in close + 1..c.len().min(close + 2200) {
                    match at(c, i) {
                        '(' => depth += 1,
                        ')' => {
                            depth -= 1;
                            if depth == 0 {
                                end = Some(i);
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                let end = end?;
                let raw: String = c.get(close + 2..end)?.iter().collect();
                let raw = raw.trim();
                let dest = match raw.strip_prefix('<') {
                    Some(rest) => rest.split('>').next().unwrap_or("").to_string(),
                    None => raw.split_whitespace().next().unwrap_or("").to_string(),
                };
                Some((inner, dest, end + 1))
            }
            '[' => {
                let label_end = close + 2 + c.get(close + 2..)?.iter().take(200).position(|&ch| ch == ']')?;
                let label: String = c.get(close + 2..label_end)?.iter().collect();
                let key = if label.is_empty() { inner.iter().collect::<String>() } else { label };
                let dest = self.refs.get(&key.trim().to_lowercase())?.clone();
                Some((inner, dest, label_end + 1))
            }
            _ => {
                let key: String = inner.iter().collect();
                let dest = self.refs.get(&key.trim().to_lowercase())?.clone();
                Some((inner, dest, close + 1))
            }
        }
    }

    /// An image, or its description when the README has shown too many.
    fn image(&mut self, alt: String, src: String, width: Option<u32>, st: &Style, out: &mut Vec<Inline>) {
        if self.images_left == 0 || src.trim().is_empty() {
            if !alt.is_empty() {
                out.push(Inline::Text { text: alt, bold: st.bold, italic: st.italic, strike: st.strike, code: false, link: st.link.clone() });
            }
            return;
        }
        self.images_left -= 1;
        out.push(Inline::Image { alt, src, width, link: st.link.clone() });
    }

    fn run(&mut self, c: &[char], base: &Style, out: &mut Vec<Inline>, depth: usize) {
        let mut st = base.clone();
        let mut buf = String::new();
        let mut i = 0;
        while i < c.len() {
            let ch = at(c, i);
            match ch {
                '\\' if at(c, i + 1).is_ascii_punctuation() => {
                    buf.push(at(c, i + 1));
                    i += 2;
                }
                '\n' => {
                    push_text(out, &mut buf, &st);
                    out.push(Inline::Break);
                    i += 1;
                }
                '`' => {
                    let n = (i..c.len()).take_while(|&k| at(c, k) == '`').count();
                    let ticks = vec!['`'; n];
                    match find_seq(c, i + n, &ticks) {
                        Some(end) => {
                            push_text(out, &mut buf, &st);
                            let text: String = c.get(i + n..end).unwrap_or_default().iter().collect();
                            let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
                            if !text.is_empty() {
                                out.push(Inline::Text {
                                    text,
                                    bold: st.bold,
                                    italic: st.italic,
                                    strike: st.strike,
                                    code: true,
                                    link: st.link.clone(),
                                });
                            }
                            i = end + n;
                        }
                        None => {
                            buf.extend(std::iter::repeat_n('`', n));
                            i += n;
                        }
                    }
                }
                '!' if at(c, i + 1) == '[' && depth < MAX_DEPTH => match self.link_parts(c, i + 1) {
                    Some((inner, dest, end)) => {
                        push_text(out, &mut buf, &st);
                        let alt = plain(inner);
                        self.image(alt, dest, None, &st, out);
                        i = end;
                    }
                    None => {
                        buf.push('!');
                        i += 1;
                    }
                },
                '[' if depth < MAX_DEPTH => match self.link_parts(c, i) {
                    Some((inner, dest, end)) => {
                        push_text(out, &mut buf, &st);
                        let linked = Style { link: if dest.is_empty() { st.link.clone() } else { Some(dest) }, ..st.clone() };
                        self.run(inner, &linked, out, depth + 1);
                        i = end;
                    }
                    None => {
                        buf.push('[');
                        i += 1;
                    }
                },
                '<' => i = self.html(c, i, &mut st, &mut buf, out),
                '&' => {
                    let end = (i + 1..c.len().min(i + 10)).find(|&k| at(c, k) == ';');
                    let decoded = end.and_then(|e| {
                        let name: String = c.get(i + 1..e)?.iter().collect();
                        entity(&name).map(|d| (d, e))
                    });
                    match decoded {
                        Some((d, e)) => {
                            push_char(&mut buf, d);
                            i = e + 1;
                        }
                        None => {
                            buf.push('&');
                            i += 1;
                        }
                    }
                }
                '*' | '_' | '~' => i = emphasis(c, i, &mut st, &mut buf, out),
                'h' if st.link.is_none()
                    && (i == 0 || !at(c, i - 1).is_alphanumeric())
                    && (starts_with(c, i, "https://") || starts_with(c, i, "http://")) =>
                {
                    let mut end = i;
                    while end < c.len() && !at(c, end).is_whitespace() && at(c, end) != '<' {
                        end += 1;
                    }
                    while end > i && matches!(at(c, end - 1), '.' | ',' | ';' | ':' | '!' | '?' | ')' | ']' | '\'' | '"') {
                        end -= 1;
                    }
                    let url: String = c.get(i..end).unwrap_or_default().iter().collect();
                    push_text(out, &mut buf, &st);
                    let linked = Style { link: Some(url.clone()), ..st.clone() };
                    buf.push_str(&url);
                    push_text(out, &mut buf, &linked);
                    i = end;
                }
                _ => {
                    push_char(&mut buf, ch);
                    i += 1;
                }
            }
        }
        push_text(out, &mut buf, &st);
    }

    /// A `<…>` at `i`: an autolink, a tag this reads (`img`, `br`, `a`, bold, italic, code,
    /// strike) or a comment or tag it drops. Returns where to go on from.
    fn html(&mut self, c: &[char], i: usize, st: &mut Style, buf: &mut String, out: &mut Vec<Inline>) -> usize {
        if starts_with(c, i, "<!--") {
            let end = find_seq(c, i + 4, &['-', '-', '>']).map_or(c.len(), |e| e + 3);
            return end;
        }
        let next = at(c, i + 1);
        let close = (i + 1..c.len().min(i + 600)).find(|&k| at(c, k) == '>');
        let Some(close) = close.filter(|_| next.is_alphabetic() || next == '/') else {
            buf.push('<');
            return i + 1;
        };
        let tag: String = c.get(i + 1..close).unwrap_or_default().iter().collect();
        let lower = tag.to_ascii_lowercase();
        if lower.starts_with("https://") || lower.starts_with("http://") || lower.starts_with("mailto:") {
            push_text(out, buf, st);
            let linked = Style { link: Some(tag.clone()), ..st.clone() };
            buf.push_str(&tag);
            push_text(out, buf, &linked);
            return close + 1;
        }
        let closing = lower.starts_with('/');
        let name: String = lower.trim_start_matches('/').chars().take_while(|ch| ch.is_ascii_alphanumeric()).collect();
        match name.as_str() {
            "img" if !closing => {
                push_text(out, buf, st);
                let width = attr(&tag, "width").and_then(|w| w.trim_end_matches("px").trim().parse::<u32>().ok());
                self.image(attr(&tag, "alt").unwrap_or_default(), attr(&tag, "src").unwrap_or_default(), width, st, out);
            }
            "br" => {
                push_text(out, buf, st);
                out.push(Inline::Break);
            }
            "a" => {
                push_text(out, buf, st);
                st.link = if closing { None } else { attr(&tag, "href") };
            }
            "b" | "strong" => {
                push_text(out, buf, st);
                st.bold = !closing;
            }
            "i" | "em" => {
                push_text(out, buf, st);
                st.italic = !closing;
            }
            "s" | "del" | "strike" => {
                push_text(out, buf, st);
                st.strike = !closing;
            }
            "code" | "kbd" | "tt" => {
                push_text(out, buf, st);
                st.code = !closing;
            }
            _ => {}
        }
        close + 1
    }
}

/// Bold, italic and strikethrough marks (`**`, `__`, `*`, `_`, `~~`) at `i`. A mark with nothing
/// to close it, a lone `~`, or a `_` in the middle of a word (`snake_case`) is just text.
/// Returns where to go on from.
fn emphasis(c: &[char], i: usize, st: &mut Style, buf: &mut String, out: &mut Vec<Inline>) -> usize {
    let ch = at(c, i);
    let double = at(c, i + 1) == ch;
    let width = if double { 2 } else { 1 };
    let prev = if i == 0 { ' ' } else { at(c, i - 1) };
    let open = is_on(st, ch, double);
    let literal = (ch == '~' && !double) || (ch == '_' && prev.is_alphanumeric() && !open);
    let after = at(c, i + width);
    let mark: Vec<char> = vec![ch; width];
    let closes_here = open && !prev.is_whitespace();
    let opens_here = !open && !after.is_whitespace() && after != '\0' && find_seq(c, i + width, &mark).is_some();
    if !literal && (closes_here || opens_here) {
        push_text(out, buf, st);
        match (ch, double) {
            ('~', _) => st.strike = !st.strike,
            (_, true) => st.bold = !st.bold,
            (_, false) => st.italic = !st.italic,
        }
    } else {
        buf.extend(std::iter::repeat_n(ch, width));
    }
    i + width
}

/// Whether the mark's style is on now.
fn is_on(st: &Style, ch: char, double: bool) -> bool {
    match (ch, double) {
        ('~', _) => st.strike,
        (_, true) => st.bold,
        (_, false) => st.italic,
    }
}

/// Text without Markdown marks, for a picture's description.
fn plain(c: &[char]) -> String {
    let none = HashMap::new();
    let mut p = Parser { refs: &none, images_left: 0 };
    let mut out = Vec::new();
    p.run(c, &Style::default(), &mut out, MAX_DEPTH);
    let mut text = String::new();
    for item in out {
        if let Inline::Text { text: t, .. } = item {
            text.push_str(&t);
        }
    }
    text.trim().to_string()
}

// ---------------------------------------------------------------------------------------------
// Blocks
// ---------------------------------------------------------------------------------------------

/// `[label]: address "title"` as (lower-cased label, address).
fn reference_definition(line: &str) -> Option<(String, String)> {
    let rest = line.trim_start().strip_prefix('[')?;
    let (label, rest) = rest.split_once("]:")?;
    if label.is_empty() || label.contains(['[', ']']) || line.len() - line.trim_start().len() > 3 {
        return None;
    }
    let rest = rest.trim();
    let dest = match rest.strip_prefix('<') {
        Some(inner) => inner.split('>').next()?,
        None => rest.split_whitespace().next()?,
    };
    (!dest.is_empty()).then(|| (label.trim().to_lowercase(), dest.to_string()))
}

fn fence(trimmed: &str) -> Option<char> {
    let ch = trimmed.chars().next().filter(|c| matches!(c, '`' | '~'))?;
    (trimmed.chars().take_while(|&c| c == ch).count() >= 3).then_some(ch)
}

/// All the reference definitions outside code, which may sit after their use.
fn reference_definitions(lines: &[&str]) -> HashMap<String, String> {
    let mut refs = HashMap::new();
    let mut open: Option<char> = None;
    for line in lines {
        let trimmed = line.trim_start();
        match (open, fence(trimmed)) {
            (None, Some(f)) => open = Some(f),
            (Some(o), Some(f)) if o == f => open = None,
            (None, None) => {
                if let Some((label, dest)) = reference_definition(line) {
                    refs.entry(label).or_insert(dest);
                }
            }
            _ => {}
        }
    }
    refs
}

/// `---`, `***`, `___` (spaces allowed) of at least three.
fn is_rule(trimmed: &str) -> bool {
    let marks: Vec<char> = trimmed.chars().filter(|c| !c.is_whitespace()).collect();
    marks.len() >= 3 && marks.first().is_some_and(|m| matches!(m, '-' | '*' | '_')) && marks.iter().all(|c| Some(c) == marks.first())
}

/// The cells of a table row.
fn cells(line: &str) -> Vec<String> {
    let line = line.trim();
    let line = line.strip_prefix('|').unwrap_or(line);
    let line = line.strip_suffix('|').filter(|_| !line.ends_with("\\|")).unwrap_or(line);
    let mut out = Vec::new();
    let mut cell = String::new();
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\\' if chars.peek() == Some(&'|') => {
                cell.push('|');
                chars.next();
            }
            '|' => out.push(std::mem::take(&mut cell).trim().to_string()),
            _ => cell.push(ch),
        }
    }
    out.push(cell.trim().to_string());
    out
}

/// The `|---|:-:|` line under a table's head.
fn is_table_rule(line: &str) -> bool {
    let t = line.trim();
    t.contains('-') && t.contains('|') && t.chars().all(|c| matches!(c, '-' | ':' | '|' | ' '))
}

/// `- item`, `* item`, `+ item`, `1. item`, `2) item`: the marker and the item's text.
fn list_marker(trimmed: &str) -> Option<(Marker, &str)> {
    if let Some(rest) = ["- ", "* ", "+ "].iter().find_map(|p| trimmed.strip_prefix(p)) {
        for (tag, done) in [("[ ] ", false), ("[x] ", true), ("[X] ", true)] {
            if let Some(item) = rest.strip_prefix(tag) {
                return Some((Marker::Task(done), item));
            }
        }
        return Some((Marker::Bullet, rest));
    }
    let digits = trimmed.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 || digits > 3 {
        return None;
    }
    let (number, rest) = trimmed.split_at(digits);
    let punct = rest.chars().next().filter(|c| matches!(c, '.' | ')'))?;
    let item = rest.get(1..)?.strip_prefix(' ')?;
    Some((Marker::Number(format!("{number}{punct}")), item))
}

/// Tags that start a new block of HTML (the rest of the line is still read).
fn block_tag(trimmed: &str) -> Option<String> {
    let inner = trimmed.strip_prefix('<')?;
    let name: String = inner.trim_start_matches('/').chars().take_while(char::is_ascii_alphanumeric).collect::<String>().to_ascii_lowercase();
    matches!(
        name.as_str(),
        "p" | "div"
            | "details"
            | "summary"
            | "table"
            | "thead"
            | "tbody"
            | "tr"
            | "td"
            | "th"
            | "center"
            | "section"
            | "ul"
            | "ol"
            | "li"
            | "hr"
            | "blockquote"
            | "picture"
            | "figure"
            | "figcaption"
            | "header"
            | "footer"
            | "nav"
            | "main"
            | "article"
    )
    .then_some(name)
}

struct Builder<'a> {
    p: Parser<'a>,
    out: Vec<Block>,
    para: String,
    quote: String,
    item: Option<(u8, Marker, String)>,
    /// The paragraph's last line ended with a hard line break.
    hard: bool,
}

impl Builder<'_> {
    fn flush(&mut self) {
        if let Some((depth, marker, text)) = self.item.take() {
            let content = self.p.inline(text.trim());
            if !content.is_empty() {
                self.out.push(Block::Item { depth, marker, content });
            }
        }
        let quote = std::mem::take(&mut self.quote);
        if !quote.trim().is_empty() {
            let content = self.p.inline(quote.trim());
            if !content.is_empty() {
                self.out.push(Block::Quote(content));
            }
        }
        let para = std::mem::take(&mut self.para);
        if !para.trim().is_empty() {
            let content = self.p.inline(para.trim());
            if !content.is_empty() {
                self.out.push(Block::Paragraph(content));
            }
        }
        self.hard = false;
    }

    fn push(&mut self, block: Block) {
        self.flush();
        self.out.push(block);
    }

    fn heading(&mut self, level: u8, text: &str) {
        self.flush();
        let content = self.p.inline(text.trim());
        if !content.is_empty() {
            self.out.push(Block::Heading(level.clamp(1, 6), content));
        }
    }
}

/// The README's blocks: headings, paragraphs, lists, quotes, code, rules and tables, with
/// emphasis, links and pictures inside them.
pub(crate) fn blocks(text: &str) -> Vec<Block> {
    let lines: Vec<&str> = text.lines().collect();
    let refs = reference_definitions(&lines);
    let mut b = Builder {
        p: Parser { refs: &refs, images_left: MAX_IMAGES },
        out: Vec::new(),
        para: String::new(),
        quote: String::new(),
        item: None,
        hard: false,
    };
    let mut code: Option<(char, String)> = None;
    let mut in_comment = false;
    let mut i = 0;
    while i < lines.len() && b.out.len() < MAX_BLOCKS {
        let raw = lines.get(i).copied().unwrap_or("").trim_end_matches('\r');
        i += 1;
        let line = raw.trim_end();
        let trimmed = line.trim_start();
        if in_comment {
            in_comment = !trimmed.contains("-->");
            continue;
        }
        if let Some((marker, body)) = code.as_mut() {
            if fence(trimmed) == Some(*marker) {
                let done = std::mem::take(body);
                code = None;
                b.push(Block::Code(done.trim_end().to_string()));
            } else if body.len() < 20_000 {
                body.push_str(line);
                body.push('\n');
            }
            continue;
        }
        if let Some(marker) = fence(trimmed) {
            b.flush();
            code = Some((marker, String::new()));
            continue;
        }
        if trimmed.starts_with("<!--") {
            in_comment = !trimmed.contains("-->");
            continue;
        }
        if reference_definition(line).is_some() {
            continue;
        }
        if trimmed.is_empty() {
            b.flush();
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix('>') {
            if b.quote.is_empty() {
                b.flush();
            }
            let rest = rest.strip_prefix(' ').unwrap_or(rest);
            // GitHub's alerts: `> [!NOTE]` is a bold label.
            let rest = match rest.trim().strip_prefix("[!").and_then(|r| r.strip_suffix(']')) {
                Some(kind) if !kind.is_empty() && kind.chars().all(|c| c.is_ascii_alphabetic()) => {
                    let lower = kind.to_ascii_lowercase();
                    let label = lower.get(..1).unwrap_or("").to_ascii_uppercase() + lower.get(1..).unwrap_or("");
                    format!("**{label}**\n")
                }
                _ => rest.to_string(),
            };
            if !b.quote.is_empty() && !b.quote.ends_with('\n') {
                b.quote.push(' ');
            }
            b.quote.push_str(&rest);
            continue;
        }
        if !b.quote.is_empty() {
            b.flush();
        }
        // `#` headings (they need the space, so `#hashtag` stays text).
        let hashes = trimmed.bytes().take_while(|&c| c == b'#').count();
        if (1..=6).contains(&hashes) && trimmed.get(hashes..).is_some_and(|r| r.is_empty() || r.starts_with(' ')) {
            let title = trimmed.get(hashes..).unwrap_or("").trim().trim_end_matches('#').trim_end();
            b.heading(hashes as u8, title);
            continue;
        }
        // `<h1 align=center>Title</h1>`
        let lower = trimmed.to_ascii_lowercase();
        if let Some(level) = lower.strip_prefix("<h").and_then(|r| r.chars().next()).and_then(|d| d.to_digit(10)).filter(|d| (1..=6).contains(d))
            && lower.chars().nth(3).is_some_and(|c| c == '>' || c == ' ')
        {
            b.heading(level as u8, trimmed);
            continue;
        }
        // Setext headings, and rules.
        if !b.para.is_empty() && b.item.is_none() && !trimmed.is_empty() && trimmed.chars().all(|c| c == '=') {
            let text = std::mem::take(&mut b.para);
            b.heading(1, &text);
            continue;
        }
        if !b.para.is_empty() && b.item.is_none() && trimmed.len() >= 2 && trimmed.chars().all(|c| c == '-') {
            let text = std::mem::take(&mut b.para);
            b.heading(2, &text);
            continue;
        }
        if is_rule(trimmed) {
            b.push(Block::Rule);
            continue;
        }
        // Tables.
        if line.contains('|') && lines.get(i).is_some_and(|next| is_table_rule(next)) {
            let head = cells(line);
            let mut rows = Vec::new();
            i += 1;
            while let Some(row) = lines.get(i).filter(|r| !r.trim().is_empty() && r.contains('|')) {
                if rows.len() < MAX_ROWS {
                    rows.push(cells(row));
                }
                i += 1;
            }
            b.flush();
            let width = head.len();
            let header: Vec<Vec<Inline>> = head.iter().map(|c| b.p.inline(c)).collect();
            let rows: Vec<Vec<Vec<Inline>>> =
                rows.iter().map(|row| (0..width).map(|k| b.p.inline(row.get(k).map_or("", String::as_str))).collect()).collect();
            b.out.push(Block::Table { header, rows });
            continue;
        }
        // HTML blocks: `<hr>`, and tags that start a new paragraph.
        let mut text = trimmed.to_string();
        if let Some(tag) = block_tag(trimmed) {
            b.flush();
            if tag == "hr" {
                b.out.push(Block::Rule);
                continue;
            }
            if tag == "summary" && !trimmed.starts_with("</") {
                text = format!("<b>{trimmed}</b>");
            }
        }
        // List items; a line after one continues it.
        let indent = line.len() - trimmed.len() + line.chars().filter(|&c| c == '\t').count() * 3;
        if let Some((marker, body)) = list_marker(trimmed) {
            b.flush();
            b.item = Some(((indent / 2).min(5) as u8, marker, body.to_string()));
            continue;
        }
        if let Some((_, _, item)) = b.item.as_mut()
            && b.para.is_empty()
            && block_tag(trimmed).is_none()
        {
            item.push(' ');
            item.push_str(trimmed);
            continue;
        }
        // Paragraph text; two spaces or a backslash at the end of a line is a hard break.
        if !b.para.is_empty() {
            b.para.push(if b.hard { '\n' } else { ' ' });
        }
        b.hard = raw.ends_with("  ") || line.ends_with('\\');
        b.para.push_str(text.trim_end_matches('\\'));
    }
    if let Some((_, body)) = code {
        b.push(Block::Code(body.trim_end().to_string()));
    }
    b.flush();
    b.out.truncate(MAX_BLOCKS);
    b.out
}

// ---------------------------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------------------------

/// What drawing needs besides the blocks.
struct Env<'a> {
    repo: &'a str,
    images: &'a HashMap<String, Picture>,
    /// Pictures that should be asked for.
    wanted: Vec<String>,
    scale: f32,
}

#[derive(Clone, Copy)]
struct Look {
    size: f32,
    color: Color32,
    bold: bool,
}

/// Draws a README at `scale` times the usual text size. Returns the addresses of pictures it
/// shows that haven't been asked for yet; the caller asks the backend for them.
pub(crate) fn show(ui: &mut Ui, blocks: &[Block], scale: f32, repo: &str, images: &HashMap<String, Picture>) -> Vec<String> {
    let t = Tokens::get(ui.ctx());
    let size = 14.5 * scale;
    let mut env = Env { repo, images, wanted: Vec::new(), scale };
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = 8.0 * scale;
        if blocks.is_empty() {
            ui.label(RichText::new(tl!("This README is empty.")).color(t.muted));
            return;
        }
        let body = Look { size, color: t.ink_strong, bold: false };
        for (n, block) in blocks.iter().enumerate() {
            match block {
                Block::Heading(level, content) => {
                    let h = match level {
                        1 => 24.0,
                        2 => 20.0,
                        3 => 17.0,
                        _ => 15.5,
                    } * scale;
                    ui.add_space(6.0 * scale);
                    inlines(ui, content, Look { size: h, color: t.ink_strong, bold: true }, &mut env);
                }
                Block::Paragraph(content) => inlines(ui, content, body, &mut env),
                Block::Item { depth, marker, content } => {
                    ui.horizontal_top(|ui| {
                        ui.add_space((8.0 + 18.0 * f32::from(*depth)) * scale);
                        let mark = match marker {
                            Marker::Bullet if *depth == 0 => "•".to_string(),
                            Marker::Bullet => "◦".to_string(),
                            Marker::Number(n) => n.clone(),
                            Marker::Task(true) => "☑".to_string(),
                            Marker::Task(false) => "☐".to_string(),
                        };
                        ui.label(RichText::new(mark).size(size).color(t.muted));
                        ui.vertical(|ui| inlines(ui, content, body, &mut env));
                    });
                }
                Block::Quote(content) => {
                    let framed = egui::Frame::new().inner_margin(egui::Margin { left: 14, right: 0, top: 2, bottom: 2 }).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        inlines(ui, content, Look { color: t.muted, ..body }, &mut env);
                    });
                    let rect = framed.response.rect;
                    ui.painter().vline(rect.left() + 1.0, rect.y_range(), egui::Stroke::new(2.0, t.line_strong));
                }
                Block::Code(code) => {
                    egui::Frame::new().stroke(t.hairline()).inner_margin(egui::Margin::same(10)).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.add(egui::Label::new(RichText::new(code).monospace().size(size - 1.0).color(t.muted)).wrap());
                    });
                }
                Block::Rule => {
                    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
                    ui.painter().hline(rect.x_range(), rect.center().y, t.hairline());
                }
                Block::Table { header, rows } => {
                    egui::ScrollArea::horizontal().id_salt(("readme-table", n)).auto_shrink([false, true]).show(ui, |ui| {
                        egui::Frame::new().stroke(t.hairline()).inner_margin(egui::Margin::same(8)).show(ui, |ui| {
                            egui::Grid::new(("readme-grid", n)).spacing(vec2(18.0 * scale, 6.0 * scale)).show(ui, |ui| {
                                for cell in header {
                                    inlines(ui, cell, Look { bold: true, ..body }, &mut env);
                                }
                                ui.end_row();
                                for row in rows {
                                    for cell in row {
                                        inlines(ui, cell, body, &mut env);
                                    }
                                    ui.end_row();
                                }
                            });
                        });
                    });
                }
            }
        }
    });
    env.wanted
}

/// One paragraph's worth of text and pictures, wrapping like a paragraph.
fn inlines(ui: &mut Ui, items: &[Inline], look: Look, env: &mut Env<'_>) {
    let t = Tokens::get(ui.ctx());
    let max_width = ui.available_width();
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(0.0, 4.0 * env.scale);
        for item in items {
            match item {
                Inline::Break => ui.end_row(),
                Inline::Text { text, bold, italic, strike, code, link } => {
                    let mut rich = RichText::new(text).size(if *code { look.size - 1.0 } else { look.size });
                    if *code {
                        rich = rich.monospace().background_color(t.sunken);
                    }
                    if *bold || look.bold {
                        rich = rich.strong();
                    }
                    if *italic {
                        rich = rich.italics();
                    }
                    if *strike {
                        rich = rich.strikethrough();
                    }
                    match link.as_deref().and_then(|href| safe::link_url(env.repo, href)) {
                        Some(url) => {
                            ui.add(egui::Hyperlink::from_label_and_url(rich, url).open_in_new_tab(true));
                        }
                        None => {
                            ui.add(egui::Label::new(rich.color(look.color)));
                        }
                    }
                }
                Inline::Image { alt, src, width, link } => image(ui, alt, src, *width, link.as_deref(), max_width, env),
            }
        }
    });
}

/// A picture of the README: drawn once it has arrived, a spinner until then, and its
/// description in muted text when it can't or mustn't be had.
fn image(ui: &mut Ui, alt: &str, src: &str, width: Option<u32>, link: Option<&str>, max_width: f32, env: &mut Env<'_>) {
    let t = Tokens::get(ui.ctx());
    let description = |ui: &mut Ui, hover: Option<&str>| {
        if !alt.is_empty() {
            let resp = ui.add(egui::Label::new(RichText::new(alt).size(13.0 * env.scale).color(t.muted)));
            if let Some(why) = hover {
                let _ = resp.on_hover_text(why);
            }
        }
    };
    let Some(url) = safe::image_url(env.repo, src) else {
        description(ui, None);
        return;
    };
    match env.images.get(&url) {
        Some(Picture::Ready(bytes)) => {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            std::hash::Hash::hash(&url, &mut hasher);
            let uri = format!("bytes://readme/{:016x}", std::hash::Hasher::finish(&hasher));
            let source = egui::ImageSource::Bytes { uri: uri.into(), bytes: egui::load::Bytes::Shared(bytes.clone()) };
            let limit = width.map_or(max_width, |w| (w as f32).min(max_width)).max(16.0);
            let picture = egui::Image::new(source).fit_to_original_size(1.0).max_width(limit).sense(Sense::click());
            let resp = ui.add(picture);
            if let Some(href) = link.and_then(|h| safe::link_url(env.repo, h)) {
                let resp = resp.on_hover_text(href.as_str()).on_hover_cursor(egui::CursorIcon::PointingHand);
                if resp.clicked() {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(href));
                }
            } else if !alt.is_empty() {
                let _ = resp.on_hover_text(alt);
            }
            ui.add_space(4.0 * env.scale);
        }
        Some(Picture::Failed(why)) => description(ui, Some(why.as_str())),
        Some(Picture::Loading) => {
            ui.add(egui::Spinner::new().size(14.0 * env.scale));
        }
        None => {
            if !env.wanted.contains(&url) {
                env.wanted.push(url);
            }
            ui.add(egui::Spinner::new().size(14.0 * env.scale));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(s: &str) -> Inline {
        Inline::Text { text: s.into(), bold: false, italic: false, strike: false, code: false, link: None }
    }

    fn inline(s: &str) -> Vec<Inline> {
        let none = HashMap::new();
        Parser { refs: &none, images_left: MAX_IMAGES }.inline(s)
    }

    #[test]
    fn escaped_backslash_before_bracket_does_not_panic() {
        // The text is: \\[x] y
        let _ = inline("\\\\[x] y");
        let _ = inline("\\\\[x]");
        let _ = inline("\\\\]");
    }

    #[test]
    fn emphasis_code_and_links_become_styled_text() {
        let got = inline("**Fast** and `small`, see [the docs](https://x.y/z).");
        assert_eq!(
            got,
            vec![
                Inline::Text { text: "Fast".into(), bold: true, italic: false, strike: false, code: false, link: None },
                text(" and "),
                Inline::Text { text: "small".into(), bold: false, italic: false, strike: false, code: true, link: None },
                text(", see "),
                Inline::Text { text: "the docs".into(), bold: false, italic: false, strike: false, code: false, link: Some("https://x.y/z".into()) },
                text("."),
            ]
        );
    }

    #[test]
    fn snake_case_and_stray_marks_stay_text() {
        assert_eq!(inline("use my_var_name and 2 * 3"), vec![text("use my_var_name and 2 * 3")]);
        assert_eq!(inline("1 < 2 & 3"), vec![text("1 < 2 & 3")]);
        assert_eq!(inline("[broken](no close"), vec![text("[broken](no close")]);
    }

    #[test]
    fn pictures_and_html_are_read() {
        let got = inline("a ![logo](docs/l.png) <img src=\"https://raw.githubusercontent.com/o/r/main/b.png\" width=\"60px\" alt=\"B\">");
        assert_eq!(
            got,
            vec![
                text("a "),
                Inline::Image { alt: "logo".into(), src: "docs/l.png".into(), width: None, link: None },
                text(" "),
                Inline::Image { alt: "B".into(), src: "https://raw.githubusercontent.com/o/r/main/b.png".into(), width: Some(60), link: None },
            ]
        );
    }

    #[test]
    fn a_linked_badge_keeps_its_link() {
        let got = inline("[![build](b.png)](https://ci.example/run)");
        assert_eq!(got, vec![Inline::Image { alt: "build".into(), src: "b.png".into(), width: None, link: Some("https://ci.example/run".into()) }]);
        let got = inline("<a href=\"https://x.y\"><img src=\"a.png\"></a> after");
        assert_eq!(
            got,
            vec![Inline::Image { alt: String::new(), src: "a.png".into(), width: None, link: Some("https://x.y".into()) }, text(" after")]
        );
    }

    #[test]
    fn reference_links_and_bare_addresses_work() {
        let mut refs = HashMap::new();
        refs.insert("docs".to_string(), "https://x.y/docs".to_string());
        let mut p = Parser { refs: &refs, images_left: MAX_IMAGES };
        let got = p.inline("[read][docs] or [docs] or https://a.b/c.");
        let links: Vec<Option<&str>> = got
            .iter()
            .filter_map(|i| match i {
                Inline::Text { link, .. } => Some(link.as_deref()),
                _ => None,
            })
            .collect();
        assert_eq!(links, vec![Some("https://x.y/docs"), None, Some("https://x.y/docs"), None, Some("https://a.b/c"), None]);
    }

    #[test]
    fn too_many_pictures_fall_back_to_their_description() {
        let many = "![x](a.png) ".repeat(MAX_IMAGES + 5);
        let got = inline(&many);
        assert_eq!(got.iter().filter(|i| matches!(i, Inline::Image { .. })).count(), MAX_IMAGES);
    }

    #[test]
    fn a_readme_becomes_blocks() {
        let md = "# Tool\n\n[![ci](b.svg)](x)\n<p align=center><img src=a.png></p>\n\nA small\ntool.\n\n- one\n  more\n  - nested\n* two\n1. three\n- [x] done\n\n> quoted\n> text\n\n```sh\ncargo run\n```\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n---\n\nSetext\n======\n";
        let got = blocks(md);
        let item = |depth, marker, s: &str| Block::Item { depth, marker, content: vec![text(s)] };
        assert_eq!(got.first(), Some(&Block::Heading(1, vec![text("Tool")])));
        assert!(matches!(got.get(1), Some(Block::Paragraph(p)) if matches!(p.as_slice(), [Inline::Image { link: Some(_), .. }])));
        assert!(matches!(got.get(2), Some(Block::Paragraph(p)) if matches!(p.as_slice(), [Inline::Image { src, link: None, .. }] if src == "a.png")));
        assert_eq!(got.get(3), Some(&Block::Paragraph(vec![text("A small tool.")])));
        assert_eq!(got.get(4), Some(&item(0, Marker::Bullet, "one more")));
        assert_eq!(got.get(5), Some(&item(1, Marker::Bullet, "nested")));
        assert_eq!(got.get(6), Some(&item(0, Marker::Bullet, "two")));
        assert_eq!(got.get(7), Some(&item(0, Marker::Number("1.".into()), "three")));
        assert_eq!(got.get(8), Some(&item(0, Marker::Task(true), "done")));
        assert_eq!(got.get(9), Some(&Block::Quote(vec![text("quoted text")])));
        assert_eq!(got.get(10), Some(&Block::Code("cargo run".into())));
        assert_eq!(
            got.get(11),
            Some(&Block::Table { header: vec![vec![text("a")], vec![text("b")]], rows: vec![vec![vec![text("1")], vec![text("2")]]] })
        );
        assert_eq!(got.get(12), Some(&Block::Rule));
        assert_eq!(got.get(13), Some(&Block::Heading(1, vec![text("Setext")])));
        assert_eq!(got.len(), 14);
    }

    #[test]
    fn comments_definitions_and_html_headings_are_handled() {
        let md = "<!-- hidden\nstill hidden -->\n<h1 align=\"center\">Title</h1>\n\n[a]: https://x.y\n\nSee [a].\n";
        let got = blocks(md);
        assert_eq!(got.first(), Some(&Block::Heading(1, vec![text("Title")])));
        assert!(
            matches!(got.get(1), Some(Block::Paragraph(p)) if p.iter().any(|i| matches!(i, Inline::Text { link: Some(l), .. } if l == "https://x.y")))
        );
        assert_eq!(got.len(), 2);
    }

    #[test]
    fn an_unclosed_code_fence_and_huge_input_are_handled() {
        assert_eq!(blocks("```\nlet x = 1;"), vec![Block::Code("let x = 1;".into())]);
        let many = "- x\n".repeat(5000);
        assert_eq!(blocks(&many).len(), MAX_BLOCKS);
        assert!(blocks("").is_empty());
        assert!(blocks("\u{0}<<<[[[***___~~~```").len() <= 2);
    }
}
