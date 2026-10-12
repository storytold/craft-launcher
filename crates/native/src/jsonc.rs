//! Editing other programs' JSON settings files without disturbing them.
//!
//! AI tools keep their MCP servers in JSON files the user also edits by hand, and some of them
//! (VS Code, Zed, opencode) allow comments and trailing commas. Reading such a file into a value
//! and writing it back would drop the comments and reorder or reformat everything. Instead this
//! parses the text into spans and changes only the bytes of the one member being added, replaced
//! or removed; the rest of the file stays byte for byte as it was.
//!
//! Every edit works on `root[container][name]`, which is all the AI tools' files need.

use serde_json::Value;

/// Deeper nesting than this is refused rather than risking the stack.
const MAX_DEPTH: usize = 128;

#[derive(Debug)]
struct Node {
    start: usize,
    end: usize,
    kind: Kind,
}

#[derive(Debug)]
enum Kind {
    Object { members: Vec<Member> },
    Array(Vec<Node>),
    Scalar,
}

#[derive(Debug)]
struct Member {
    key: String,
    key_start: usize,
    value: Node,
    /// Where the comma after the value is, if there is one (a trailing comma counts).
    comma: Option<usize>,
}

struct Parser<'a> {
    text: &'a str,
    bytes: &'a [u8],
    pos: usize,
    /// The member of the root whose contents are kept. Everything else in the file is checked
    /// and skipped, so a settings file that is mostly history (Claude Code's) costs no memory.
    container: &'a str,
}

impl<'a> Parser<'a> {
    fn error(&self, at: usize, what: &str) -> String {
        let before = self.text.get(..at).unwrap_or(self.text);
        let line = before.matches('\n').count() + 1;
        let column = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
        format!("{what} at line {line}, column {column}")
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    /// Skips whitespace, `// line` and `/* block */` comments.
    fn skip_trivia(&mut self) -> Result<(), String> {
        loop {
            match self.peek() {
                Some(b' ' | b'\t' | b'\n' | b'\r') => self.pos += 1,
                Some(b'/') => match self.bytes.get(self.pos + 1) {
                    Some(b'/') => {
                        let rest = self.bytes.get(self.pos..).unwrap_or_default();
                        self.pos += rest.iter().position(|&b| b == b'\n').unwrap_or(rest.len());
                    }
                    Some(b'*') => {
                        let start = self.pos;
                        let rest = self.text.get(self.pos + 2..).unwrap_or_default();
                        let len = rest.find("*/").ok_or_else(|| self.error(start, "unclosed comment"))?;
                        self.pos += 2 + len + 2;
                    }
                    _ => return Err(self.error(self.pos, "unexpected '/'")),
                },
                _ => return Ok(()),
            }
        }
    }

    fn expect(&mut self, byte: u8, what: &str) -> Result<(), String> {
        if self.peek() == Some(byte) {
            self.pos += 1;
            Ok(())
        } else {
            Err(self.error(self.pos, &format!("expected {what}")))
        }
    }

    /// The value at the current position. With `keep`, its members and items are recorded;
    /// without, it is only checked and its span returned.
    fn value(&mut self, depth: usize, keep: bool) -> Result<Node, String> {
        if depth > MAX_DEPTH {
            return Err(self.error(self.pos, "nested too deeply"));
        }
        let start = self.pos;
        match self.peek() {
            Some(b'{') => {
                self.pos += 1;
                let mut members = Vec::new();
                loop {
                    self.skip_trivia()?;
                    if self.peek() == Some(b'}') {
                        self.pos += 1;
                        break;
                    }
                    let key_start = self.pos;
                    if self.peek().is_none() {
                        return Err(self.error(self.pos, "unexpected end of file"));
                    }
                    if self.peek() != Some(b'"') {
                        return Err(self.error(self.pos, "expected a name in quotes"));
                    }
                    let key = if keep { self.string()? } else { self.skip_string().map(|()| String::new())? };
                    self.skip_trivia()?;
                    self.expect(b':', "':'")?;
                    self.skip_trivia()?;
                    // Of the root's members, only the container's contents are needed.
                    let value = self.value(depth + 1, keep && (depth > 0 || key == self.container))?;
                    self.skip_trivia()?;
                    let comma = (self.peek() == Some(b',')).then_some(self.pos);
                    if keep {
                        members.push(Member { key, key_start, value, comma });
                    }
                    if comma.is_some() {
                        self.pos += 1;
                    } else {
                        self.skip_trivia()?;
                        self.expect(b'}', "',' or '}'")?;
                        break;
                    }
                }
                Ok(Node { start, end: self.pos, kind: Kind::Object { members } })
            }
            Some(b'[') => {
                self.pos += 1;
                let mut items = Vec::new();
                loop {
                    self.skip_trivia()?;
                    if self.peek() == Some(b']') {
                        self.pos += 1;
                        break;
                    }
                    let item = self.value(depth + 1, keep)?;
                    if keep {
                        items.push(item);
                    }
                    self.skip_trivia()?;
                    if self.peek() == Some(b',') {
                        self.pos += 1;
                    } else {
                        self.expect(b']', "',' or ']'")?;
                        break;
                    }
                }
                Ok(Node { start, end: self.pos, kind: Kind::Array(items) })
            }
            Some(b'"') => {
                self.skip_string()?;
                Ok(Node { start, end: self.pos, kind: Kind::Scalar })
            }
            Some(_) => {
                let rest = self.bytes.get(self.pos..).unwrap_or_default();
                let len =
                    rest.iter().position(|b| matches!(b, b' ' | b'\t' | b'\n' | b'\r' | b',' | b']' | b'}' | b'/' | b':')).unwrap_or(rest.len());
                let token = self.text.get(self.pos..self.pos + len).unwrap_or_default();
                if len == 0 || serde_json::from_str::<Value>(token).is_err() {
                    return Err(self.error(start, "unexpected text"));
                }
                self.pos += len;
                Ok(Node { start, end: self.pos, kind: Kind::Scalar })
            }
            None => Err(self.error(self.pos, "unexpected end of file")),
        }
    }

    /// Moves past the string at the current position.
    fn skip_string(&mut self) -> Result<(), String> {
        let start = self.pos;
        let mut i = self.pos + 1;
        loop {
            match self.bytes.get(i) {
                Some(b'\\') => i += 2,
                Some(b'"') => break,
                Some(_) => i += 1,
                None => return Err(self.error(start, "unclosed string")),
            }
        }
        self.pos = i + 1;
        Ok(())
    }

    /// The string at the current position, decoded.
    fn string(&mut self) -> Result<String, String> {
        let start = self.pos;
        self.skip_string()?;
        let raw = self.text.get(start..self.pos).unwrap_or_default();
        serde_json::from_str::<String>(raw).map_err(|_| self.error(start, "invalid string"))
    }
}

/// A parsed settings file: its text, the member of the root that was asked for and, unless the
/// file is blank, the root value.
#[derive(Debug)]
pub struct Doc<'a> {
    text: &'a str,
    container: &'a str,
    root: Option<Node>,
}

/// The byte offset where the JSON starts (after a UTF-8 byte order mark).
fn body_start(text: &str) -> usize {
    if text.starts_with('\u{feff}') { '\u{feff}'.len_utf8() } else { 0 }
}

/// Parses `text` for work on `root[container]`.
pub fn parse<'a>(text: &'a str, container: &'a str) -> Result<Doc<'a>, String> {
    let mut p = Parser { text, bytes: text.as_bytes(), pos: body_start(text), container };
    p.skip_trivia()?;
    if p.peek().is_none() {
        return Ok(Doc { text, container, root: None });
    }
    let root = p.value(0, true)?;
    p.skip_trivia()?;
    if p.peek().is_some() {
        return Err(p.error(p.pos, "unexpected text after the end"));
    }
    if !matches!(root.kind, Kind::Object { .. }) {
        return Err("the file isn't a JSON object".into());
    }
    Ok(Doc { text, container, root: Some(root) })
}

fn members(node: &Node) -> Option<&[Member]> {
    match &node.kind {
        Kind::Object { members } => Some(members),
        _ => None,
    }
}

/// The last member called `key` (JSON readers take the last of duplicate names).
fn find<'n>(members: &'n [Member], key: &str) -> Option<(usize, &'n Member)> {
    members.iter().enumerate().rev().find(|(_, m)| m.key == key)
}

impl Doc<'_> {
    /// The object at `root[container]`: `Ok(None)` when it isn't there.
    fn container(&self) -> Result<Option<&Node>, String> {
        let Some(root) = &self.root else { return Ok(None) };
        let Some((_, m)) = find(members(root).unwrap_or_default(), self.container) else { return Ok(None) };
        match m.value.kind {
            Kind::Object { .. } => Ok(Some(&m.value)),
            _ => Err(format!("\"{}\" isn't an object", self.container)),
        }
    }

    /// Every member of `root[container]`, as values.
    pub fn entries(&self) -> Result<Vec<(String, Value)>, String> {
        let Some(node) = self.container()? else { return Ok(Vec::new()) };
        Ok(members(node).unwrap_or_default().iter().map(|m| (m.key.clone(), self.to_value(&m.value))).collect())
    }

    fn to_value(&self, node: &Node) -> Value {
        match &node.kind {
            Kind::Object { members } => Value::Object(members.iter().map(|m| (m.key.clone(), self.to_value(&m.value))).collect()),
            Kind::Array(items) => Value::Array(items.iter().map(|n| self.to_value(n)).collect()),
            Kind::Scalar => self.text.get(node.start..node.end).and_then(|t| serde_json::from_str(t).ok()).unwrap_or(Value::Null),
        }
    }
}

/// How the file is laid out, so added text looks like the rest.
struct Style {
    indent: String,
    newline: &'static str,
}

impl Style {
    fn of(text: &str) -> Self {
        let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
        // The first indented line is a member of the root: its indent is one level.
        let indent = text
            .lines()
            .skip(1)
            .filter(|l| !l.trim().is_empty())
            .map(|l| l.get(..l.len() - l.trim_start().len()).unwrap_or_default())
            .find(|lead| !lead.is_empty())
            .map_or_else(|| "  ".to_string(), |lead| if lead.starts_with('\t') { "\t".to_string() } else { " ".repeat(lead.len().min(8)) });
        Self { indent, newline }
    }

    fn at(&self, depth: usize) -> String {
        self.indent.repeat(depth)
    }
}

fn quote(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

/// `value` as JSON text whose first line continues a line at `depth` and whose closing bracket
/// sits at `depth`. Lists stay on one line (`["mcp"]`), as people write them.
fn render(value: &Value, depth: usize, style: &Style, inline: bool) -> String {
    match value {
        Value::Object(map) => render_fields(map.iter().map(|(k, v)| (k.as_str(), v)), depth, style, inline),
        Value::Array(items) => format!("[{}]", items.iter().map(|v| render(v, depth, style, true)).collect::<Vec<_>>().join(", ")),
        other => serde_json::to_string(other).unwrap_or_else(|_| "null".into()),
    }
}

/// An object from `(name, value)` pairs, in their order.
fn render_fields<'v>(fields: impl Iterator<Item = (&'v str, &'v Value)>, depth: usize, style: &Style, inline: bool) -> String {
    let fields: Vec<(&str, &Value)> = fields.collect();
    if fields.is_empty() {
        return "{}".into();
    }
    if inline {
        let body: Vec<String> = fields.iter().map(|(k, v)| format!("{}: {}", quote(k), render(v, depth, style, true))).collect();
        return format!("{{{}}}", body.join(", "));
    }
    let inner = style.at(depth + 1);
    let body: Vec<String> = fields.iter().map(|(k, v)| format!("{inner}{}: {}", quote(k), render(v, depth + 1, style, false))).collect();
    format!("{{{nl}{}{nl}{}}}", body.join(&format!(",{}", style.newline)), style.at(depth), nl = style.newline)
}

/// Replaces `range` of `text` with `with`.
fn splice(text: &str, start: usize, end: usize, with: &str) -> String {
    let mut out = String::with_capacity(text.len() + with.len());
    out.push_str(text.get(..start).unwrap_or(text));
    out.push_str(with);
    out.push_str(text.get(end..).unwrap_or_default());
    out
}

/// Whether the object spans one line only (`{"a": 1}`): new members then stay on that line too.
fn one_line(text: &str, node: &Node) -> bool {
    text.get(node.start..node.end).is_some_and(|s| !s.contains('\n')) && !matches!(&node.kind, Kind::Object { members } if members.is_empty())
}

/// Adds the member `"key": value_text` to the object `node`, whose members sit at `depth`.
fn insert(text: &str, node: &Node, depth: usize, key: &str, value_text: &str, style: &Style) -> String {
    let members = members(node).unwrap_or_default();
    let member = format!("{}: {value_text}", quote(key));
    match members.last() {
        Some(last) if one_line(text, node) => match last.comma {
            Some(c) => splice(text, c + 1, c + 1, &format!(" {member},")),
            None => splice(text, last.value.end, last.value.end, &format!(", {member}")),
        },
        Some(last) => match last.comma {
            // Trailing commas: keep writing them.
            Some(c) => splice(text, c + 1, c + 1, &format!("{}{}{member},", style.newline, style.at(depth))),
            None => splice(text, last.value.end, last.value.end, &format!(",{}{}{member}", style.newline, style.at(depth))),
        },
        None => {
            let (open, close) = (node.start + 1, node.end - 1);
            let inside = text.get(open..close).unwrap_or_default();
            let line = format!("{}{}{member}", style.newline, style.at(depth));
            if inside.trim().is_empty() {
                splice(text, open, close, &format!("{line}{}{}", style.newline, style.at(depth.saturating_sub(1))))
            } else {
                // Only comments inside: put the member first and keep them.
                splice(text, open, open, &line)
            }
        }
    }
}

/// Sets `root[container][name]` to `fields` (an object, in that order), adding the container or
/// the root object when they're missing. Everything else in `text` is kept as it is.
pub fn set(text: &str, container: &str, name: &str, fields: &[(&str, Value)]) -> Result<String, String> {
    let doc = parse(text, container)?;
    let style = Style::of(text);
    let entry = |depth: usize, inline: bool| render_fields(fields.iter().map(|(k, v)| (*k, v)), depth, &style, inline);
    let Some(root) = &doc.root else {
        // A blank file (or only comments): an empty object after whatever is there, then as usual.
        let lead = text.trim_end();
        let gap = if lead.trim_start_matches('\u{feff}').is_empty() { "" } else { style.newline };
        return set(&format!("{lead}{gap}{{}}{}", style.newline), container, name, fields);
    };
    match doc.container()? {
        Some(node) => {
            let inline = one_line(text, node);
            match find(members(node).unwrap_or_default(), name) {
                Some((_, m)) => Ok(splice(text, m.value.start, m.value.end, &entry(2, inline))),
                None => Ok(insert(text, node, 2, name, &entry(2, inline), &style)),
            }
        }
        None => {
            let inline = one_line(text, root);
            let object = if inline {
                format!("{{{}: {}}}", quote(name), entry(2, true))
            } else {
                format!("{{{nl}{}{}: {}{nl}{}}}", style.at(2), quote(name), entry(2, false), style.at(1), nl = style.newline)
            };
            Ok(insert(text, root, 1, container, &object, &style))
        }
    }
}

/// Removes `root[container][name]` (every copy of it). `text` comes back unchanged when it isn't
/// there.
pub fn remove(text: &str, container: &str, name: &str) -> Result<String, String> {
    let mut text = text.to_string();
    // One pass for each copy of the name, the last first. A file with more copies than this is
    // not worth saving.
    for _ in 0..64 {
        let doc = parse(&text, container)?;
        let Some(node) = doc.container()? else { return Ok(text) };
        let list = members(node).unwrap_or_default();
        let Some((i, m)) = find(list, name) else { return Ok(text) };
        let copies = list.iter().filter(|m| m.key == name).count();
        let blank = |s: Option<&str>| s.is_some_and(|s| s.trim().is_empty());
        let after = m.comma.map_or(m.value.end, |c| c + 1);
        if list.len() == 1 && blank(text.get(node.start + 1..m.key_start)) && blank(text.get(after..node.end - 1)) {
            // The only member, with nothing else inside: back to `{}`.
            return Ok(splice(&text, node.start + 1, node.end - 1, ""));
        }
        let (mut start, mut end) = match (m.comma, i.checked_sub(1).and_then(|p| list.get(p))) {
            (Some(_), _) => (m.key_start, after),
            // The last member: take the comma before it instead.
            (None, Some(prev)) => (prev.value.end, m.value.end),
            (None, None) => (m.key_start, m.value.end),
        };
        // A member on lines of its own takes those lines with it.
        let line_start = text.get(..start).and_then(|s| s.rfind('\n')).map_or(0, |n| n + 1);
        let line_end = text.get(end..).map_or(text.len(), |s| s.find('\n').map_or(text.len(), |n| end + n + 1));
        if start == m.key_start && blank(text.get(line_start..start)) && blank(text.get(end..line_end)) {
            (start, end) = (line_start, line_end);
        } else if m.comma.is_some() {
            // `{"a": 1, "b": 2}` without "a" is `{"b": 2}`.
            end += text.get(end..).map_or(0, |s| s.len() - s.trim_start_matches([' ', '\t']).len());
        }
        text = splice(&text, start, end, "");
        if copies == 1 {
            return Ok(text);
        }
    }
    Err(format!("\"{name}\" appears too many times"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entry() -> Vec<(&'static str, Value)> {
        vec![("command", json!("/usr/bin/photocraft-cli")), ("args", json!(["mcp"]))]
    }

    fn reads(text: &str) -> Value {
        let doc = parse(text, "mcpServers").unwrap();
        Value::Object(doc.entries().unwrap().into_iter().collect())
    }

    #[test]
    fn adds_to_an_existing_list_and_keeps_everything_else() {
        let before = "{\n  \"mcpServers\": {\n    \"blender\": {\n      \"command\": \"uvx\",\n      \"args\": [\"blender-mcp\"]\n    }\n  },\n  \"zeta\": 1,\n  \"alpha\": 2\n}\n";
        let after = set(before, "mcpServers", "photocraft", &entry()).unwrap();
        assert_eq!(
            after,
            "{\n  \"mcpServers\": {\n    \"blender\": {\n      \"command\": \"uvx\",\n      \"args\": [\"blender-mcp\"]\n    },\n    \"photocraft\": {\n      \"command\": \"/usr/bin/photocraft-cli\",\n      \"args\": [\"mcp\"]\n    }\n  },\n  \"zeta\": 1,\n  \"alpha\": 2\n}\n"
        );
        assert_eq!(reads(&after)["photocraft"]["args"], json!(["mcp"]));
        // And removing it gives back the file exactly.
        assert_eq!(remove(&after, "mcpServers", "photocraft").unwrap(), before);
    }

    #[test]
    fn creates_the_file_the_list_and_the_object() {
        let made = set("", "mcpServers", "photocraft", &entry()).unwrap();
        assert_eq!(
            made,
            "{\n  \"mcpServers\": {\n    \"photocraft\": {\n      \"command\": \"/usr/bin/photocraft-cli\",\n      \"args\": [\"mcp\"]\n    }\n  }\n}\n"
        );
        let added = set("{\n  \"theme\": \"dark\"\n}\n", "servers", "photocraft", &entry()).unwrap();
        assert_eq!(
            added,
            "{\n  \"theme\": \"dark\",\n  \"servers\": {\n    \"photocraft\": {\n      \"command\": \"/usr/bin/photocraft-cli\",\n      \"args\": [\"mcp\"]\n    }\n  }\n}\n"
        );
        let empty = set("{}", "mcpServers", "a", &[("command", json!("x"))]).unwrap();
        assert_eq!(empty, "{\n  \"mcpServers\": {\n    \"a\": {\n      \"command\": \"x\"\n    }\n  }\n}");
        let empty_list = set("{\n  \"mcpServers\": {}\n}", "mcpServers", "a", &[("command", json!("x"))]).unwrap();
        assert_eq!(empty_list, "{\n  \"mcpServers\": {\n    \"a\": {\n      \"command\": \"x\"\n    }\n  }\n}");
        serde_json::from_str::<Value>(&empty_list).unwrap();
        // Emptied again, it reads as it did.
        assert_eq!(remove(&empty_list, "mcpServers", "a").unwrap(), "{\n  \"mcpServers\": {}\n}");
        assert_eq!(remove("{\"m\": { /* keep */ \"a\": 1 }}", "m", "a").unwrap(), "{\"m\": { /* keep */  }}");
    }

    #[test]
    fn keeps_comments_trailing_commas_tabs_and_crlf() {
        // Zed's and VS Code's settings: comments, trailing commas.
        let zed = "// Zed settings\n//\n// Docs: https://zed.dev/docs\n{\n\t\"theme\": \"One Dark\", // mine\n\t/* servers */\n\t\"context_servers\": {\n\t\t\"other\": {\"command\": \"x\"},\n\t},\n}\n";
        let after = set(zed, "context_servers", "photocraft", &entry()).unwrap();
        assert!(after.starts_with("// Zed settings\n//\n// Docs: https://zed.dev/docs\n{\n\t\"theme\": \"One Dark\", // mine\n\t/* servers */\n"));
        assert!(after.contains("\t\t\"other\": {\"command\": \"x\"},\n\t\t\"photocraft\": {\n\t\t\t\"command\": \"/usr/bin/photocraft-cli\",\n\t\t\t\"args\": [\"mcp\"]\n\t\t},\n\t},\n}\n"), "{after}");
        assert_eq!(remove(&after, "context_servers", "photocraft").unwrap(), zed);
        let crlf = "{\r\n    \"mcpServers\": {\r\n        \"a\": {}\r\n    }\r\n}\r\n";
        let after = set(crlf, "mcpServers", "b", &[("command", json!("x"))]).unwrap();
        assert_eq!(
            after,
            "{\r\n    \"mcpServers\": {\r\n        \"a\": {},\r\n        \"b\": {\r\n            \"command\": \"x\"\r\n        }\r\n    }\r\n}\r\n"
        );
        assert_eq!(remove(&after, "mcpServers", "b").unwrap(), crlf);
    }

    #[test]
    fn replaces_an_entry_in_place() {
        let before = "{\"mcpServers\": {\"photocraft\": {\"command\": \"old\", \"args\": []}, \"z\": {}}}";
        let after = set(before, "mcpServers", "photocraft", &entry()).unwrap();
        assert_eq!(after, "{\"mcpServers\": {\"photocraft\": {\"command\": \"/usr/bin/photocraft-cli\", \"args\": [\"mcp\"]}, \"z\": {}}}");
        // One-line objects stay on one line.
        let added = set("{\"mcpServers\": {\"z\": {}}}", "mcpServers", "a", &[("command", json!("x"))]).unwrap();
        assert_eq!(added, "{\"mcpServers\": {\"z\": {}, \"a\": {\"command\": \"x\"}}}");
        let root = set("{\"x\": 1}", "mcpServers", "a", &[("command", json!("x"))]).unwrap();
        assert_eq!(root, "{\"x\": 1, \"mcpServers\": {\"a\": {\"command\": \"x\"}}}");
    }

    #[test]
    fn removes_first_middle_last_and_only_members() {
        let three = "{\n  \"mcpServers\": {\n    \"a\": 1,\n    \"b\": 2,\n    \"c\": 3\n  }\n}";
        assert_eq!(remove(three, "mcpServers", "a").unwrap(), "{\n  \"mcpServers\": {\n    \"b\": 2,\n    \"c\": 3\n  }\n}");
        assert_eq!(remove(three, "mcpServers", "b").unwrap(), "{\n  \"mcpServers\": {\n    \"a\": 1,\n    \"c\": 3\n  }\n}");
        assert_eq!(remove(three, "mcpServers", "c").unwrap(), "{\n  \"mcpServers\": {\n    \"a\": 1,\n    \"b\": 2\n  }\n}");
        let one = "{\n  \"mcpServers\": {\n    \"a\": {\n      \"x\": [1, 2]\n    }\n  }\n}";
        let gone = remove(one, "mcpServers", "a").unwrap();
        assert_eq!(gone, "{\n  \"mcpServers\": {}\n}");
        serde_json::from_str::<Value>(&gone).unwrap();
        assert_eq!(remove("{\"mcpServers\": {\"a\": 1, \"b\": 2}}", "mcpServers", "b").unwrap(), "{\"mcpServers\": {\"a\": 1}}");
        assert_eq!(remove("{\"mcpServers\": {\"a\": 1, \"b\": 2}}", "mcpServers", "a").unwrap(), "{\"mcpServers\": {\"b\": 2}}");
        // Not there: nothing changes.
        assert_eq!(remove(three, "mcpServers", "zz").unwrap(), three);
        assert_eq!(remove("{}", "mcpServers", "a").unwrap(), "{}");
        assert_eq!(remove("", "mcpServers", "a").unwrap(), "");
        // Duplicates all go.
        assert_eq!(remove("{\"m\": {\"a\": 1, \"b\": 0, \"a\": 2}}", "m", "a").unwrap(), "{\"m\": {\"b\": 0}}");
    }

    #[test]
    fn reads_values_with_escapes_and_comments() {
        let text = "\u{feff}{\n  // servers\n  \"mcpServers\": {\n    \"w\": {\"command\": \"C:\\\\Program Files\\\\W\\\\w-cli.exe\", \"args\": [\"mcp\", \"--x=\\\"y\\\"\"], \"n\": -1.5e3, \"t\": true, \"z\": null,},\n  },\n}";
        let v = reads(text);
        assert_eq!(v["w"]["command"], r"C:\Program Files\W\w-cli.exe");
        assert_eq!(v["w"]["args"], json!(["mcp", "--x=\"y\""]));
        assert_eq!(v["w"]["n"], json!(-1500.0));
        let after = set(text, "mcpServers", "v", &entry()).unwrap();
        assert!(after.starts_with('\u{feff}'));
        assert_eq!(reads(&after)["v"]["command"], "/usr/bin/photocraft-cli");
    }

    #[test]
    fn refuses_what_it_cannot_edit_safely() {
        for (bad, why) in [
            ("{\"a\": }", "unexpected text at line 1, column 7"),
            ("{\"a\": 1", "expected ',' or '}' at line 1, column 8"),
            ("{\n  \"a\": tru\n}", "unexpected text at line 2, column 8"),
            ("{\"a\": \"x}", "unclosed string at line 1, column 7"),
            ("{/* x", "unclosed comment at line 1, column 2"),
            ("[1]", "the file isn't a JSON object"),
            ("{} {}", "unexpected text after the end at line 1, column 4"),
            ("{a: 1}", "expected a name in quotes at line 1, column 2"),
        ] {
            assert_eq!(parse(bad, "m").err().as_deref(), Some(why), "{bad}");
            assert!(set(bad, "m", "a", &entry()).is_err());
        }
        assert_eq!(set("{\"m\": []}", "m", "a", &entry()).err().as_deref(), Some("\"m\" isn't an object"));
        let deep = format!("{{\"a\": {}{}}}", "[".repeat(1000), "]".repeat(1000));
        assert!(parse(&deep, "m").unwrap_err().starts_with("nested too deeply"));
        // Trouble outside the member being changed is found too: the whole file has to be sound.
        assert_eq!(set("{\"m\": {}, \"other\": [1, }", "m", "a", &entry()).err().as_deref(), Some("unexpected text at line 1, column 24"));
        assert_eq!(set("{\"other\": {\"k\" 1}, \"m\": {}}", "m", "a", &entry()).err().as_deref(), Some("expected ':' at line 1, column 16"));
    }

    #[test]
    fn every_edit_gives_valid_json() {
        let samples = [
            "{}",
            "{\n}",
            "{\"mcpServers\": {}}",
            "{\n  \"mcpServers\": {\n    \"a\": {\"command\": \"x\"}\n  }\n}",
            "{\n    \"x\": [1, {\"y\": 2}],\n    \"mcpServers\": {\"a\": 1}\n}",
        ];
        for s in samples {
            let added = set(s, "mcpServers", "photocraft", &entry()).unwrap();
            let v: Value = serde_json::from_str(&added).unwrap_or_else(|e| panic!("{e}: {added}"));
            assert_eq!(v["mcpServers"]["photocraft"]["command"], "/usr/bin/photocraft-cli");
            let removed = remove(&added, "mcpServers", "photocraft").unwrap();
            let v: Value = serde_json::from_str(&removed).unwrap_or_else(|e| panic!("{e}: {removed}"));
            assert!(v["mcpServers"].get("photocraft").is_none());
        }
    }
}
