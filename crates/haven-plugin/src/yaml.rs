//! A deliberately small YAML, for files Haven has to trust.
//!
//! A repository index is the one file in Haven that an attacker gets to
//! write. It is signed, so a *modified* index is caught — but the parser
//! still runs on whatever arrives, before anything has been verified, and
//! it runs on every refresh. So it is a subset, not a YAML:
//!
//! * mappings, `key: value`, one per line;
//! * nested blocks under a `key:` with nothing after the colon;
//! * sequences of scalars or of mappings, `- item` / `- key: value`;
//! * flow sequences on one line, `[a, b, c]`, because `events: [a, b]` is
//!   how anyone would write it;
//! * plain, `'single'` and `"double"` quoted scalars;
//! * `#` comments and blank lines.
//!
//! Everything else — anchors, aliases, tags, multiple documents, block
//! literals, flow mappings, tabs for indentation — is an error that names
//! its line. Refusing a document is always safe here: the file is fetched
//! again on the next refresh, and a repository that cannot write this much
//! YAML has made a mistake worth telling it about.
//!
//! Every node carries the line it came from, so a complaint about the
//! *contents* of an index — a hash that is not a hash, a missing version —
//! can point at the line too. That is most of what makes a hand-written
//! index pleasant to fix.

use std::fmt;

/// A parsed node, and where it was written.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Node {
    pub line: usize,
    pub value: Value,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Value {
    Scalar(String),
    List(Vec<Node>),
    /// In file order, and a list rather than a map: a duplicate key is an
    /// error worth reporting rather than something to silently resolve, and
    /// an index has a handful of keys, so lookup cost does not come into it.
    Map(Vec<(String, Node)>),
}

/// What is wrong, and the line it is wrong on.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Error {
    pub line: usize,
    pub message: String,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}

impl std::error::Error for Error {}

fn err<T>(line: usize, message: impl Into<String>) -> Result<T, Error> {
    Err(Error { line, message: message.into() })
}

impl Node {
    pub fn as_str(&self) -> Option<&str> {
        match &self.value {
            Value::Scalar(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_list(&self) -> Option<&[Node]> {
        match &self.value {
            Value::List(items) => Some(items),
            _ => None,
        }
    }

    pub fn as_map(&self) -> Option<&[(String, Node)]> {
        match &self.value {
            Value::Map(entries) => Some(entries),
            _ => None,
        }
    }

    /// One key of a mapping, if this is a mapping and it has that key.
    pub fn get(&self, key: &str) -> Option<&Node> {
        self.as_map()?.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    /// What kind of thing this is, for an error message.
    pub fn kind(&self) -> &'static str {
        match &self.value {
            Value::Scalar(_) => "a value",
            Value::List(_) => "a list",
            Value::Map(_) => "a block",
        }
    }
}

/// Parse a whole document. The top level must be a mapping: every file this
/// parser exists for is one, and allowing a bare list or scalar would only
/// move the error further from its cause.
pub fn parse(text: &str) -> Result<Node, Error> {
    let lines = significant(text)?;
    if lines.is_empty() {
        return err(1, "the file is empty");
    }
    let mut at = 0;
    let indent = lines[0].indent;
    let node = block(&lines, &mut at, indent, 0)?;
    if at < lines.len() {
        return err(lines[at].number, "this line is indented less than the block it is in");
    }
    match node.value {
        Value::Map(_) => Ok(node),
        _ => err(node.line, "the file has to start with `key: value`"),
    }
}

/// One line that carries content, with its indentation measured.
#[derive(Clone, Debug)]
struct Line<'a> {
    number: usize,
    indent: usize,
    text: &'a str,
}

/// Strip blanks and comments, measure indentation, and refuse the syntax
/// this subset does not have. Doing it in one pass up front means the
/// parser below only ever sees lines it can act on.
fn significant(text: &str) -> Result<Vec<Line<'_>>, Error> {
    let mut out = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let number = i + 1;
        if raw.starts_with('\t') || raw.starts_with(' ') && raw.trim_start().starts_with('\t') {
            return err(number, "indent with spaces, not tabs");
        }
        let indent = raw.len() - raw.trim_start().len();
        let body = raw.trim_start();
        // A comment is a `#` that starts a line. One after a value is not
        // stripped: `name: a#b` is a name with a hash in it, and guessing
        // otherwise silently changes data.
        if body.is_empty() || body.starts_with('#') {
            continue;
        }
        // Only the markers that are line-shaped are caught here. A sigil
        // in the *value* position — `alias: *other` — is not at the start
        // of a line, so it is caught where plain scalars are read instead.
        for (bad, what) in [
            ("---", "several documents in one file"),
            ("...", "a document end marker"),
            ("%", "a directive"),
        ] {
            if body.starts_with(bad) {
                return err(number, format!("{what} is more YAML than Haven reads"));
            }
        }
        out.push(Line { number, indent, text: body.trim_end() });
    }
    Ok(out)
}

/// How deep a document may nest.
///
/// This parser recurses once per level, and the levels are not bounded by
/// anything in the file itself: nesting a thousand deep costs a thousand
/// lines, each one space further in, which is nothing to generate. Without
/// a limit, that is a stack overflow — in the daemon, on bytes that have
/// not been verified yet, which is to say a one-line denial of service by
/// anyone who can answer an HTTP request. Found by the test below, which
/// was written expecting the indentation to bound this. It does not.
///
/// An index uses four levels. Thirty-two leaves room to be wrong about
/// that and is nowhere near the stack.
const MAX_DEPTH: usize = 32;

/// Parse every line indented at least `indent` as one block, starting at
/// `*at` and leaving it on the first line that belongs to an outer block.
fn block(lines: &[Line<'_>], at: &mut usize, indent: usize, depth: usize) -> Result<Node, Error> {
    let first = &lines[*at];
    if depth > MAX_DEPTH {
        return err(first.number, format!("nested more than {MAX_DEPTH} deep"));
    }
    if first.text.starts_with("- ") || first.text == "-" {
        return sequence(lines, at, indent, depth);
    }
    mapping(lines, at, indent, depth)
}

fn mapping(lines: &[Line<'_>], at: &mut usize, indent: usize, depth: usize) -> Result<Node, Error> {
    let line = lines[*at].number;
    let mut entries: Vec<(String, Node)> = Vec::new();
    while *at < lines.len() {
        let here = &lines[*at];
        if here.indent < indent {
            break;
        }
        if here.indent > indent {
            return err(here.number, "this line is indented further than anything expects");
        }
        if here.text.starts_with("- ") || here.text == "-" {
            return err(here.number, "a list item where this block's keys are");
        }
        let (key, rest) = split_key(here)?;
        if entries.iter().any(|(k, _)| *k == key) {
            return err(here.number, format!("`{key}` is set twice in the same block"));
        }
        let number = here.number;
        *at += 1;
        let node = if rest.is_empty() {
            // `key:` with nothing after it opens a block, which has to be
            // indented further than the key. Nothing at all after it is an
            // error rather than an empty value: in an index every key that
            // is written is meant to carry something, and a silently empty
            // one shows up later as a missing field with no line to blame.
            let deeper = lines.get(*at).filter(|l| l.indent > indent);
            match deeper {
                Some(l) => {
                    let inner = l.indent;
                    block(lines, at, inner, depth + 1)?
                }
                None => return err(number, format!("`{key}:` has nothing under it")),
            }
        } else {
            Node { line: number, value: scalar_or_flow(number, rest)? }
        };
        entries.push((key, node));
    }
    Ok(Node { line, value: Value::Map(entries) })
}

fn sequence(
    lines: &[Line<'_>],
    at: &mut usize,
    indent: usize,
    depth: usize,
) -> Result<Node, Error> {
    let line = lines[*at].number;
    let mut items = Vec::new();
    while *at < lines.len() {
        let here = &lines[*at];
        if here.indent < indent {
            break;
        }
        if here.indent > indent {
            return err(here.number, "this line is indented further than anything expects");
        }
        let Some(rest) = here.text.strip_prefix("- ").map(str::trim_start) else {
            if here.text == "-" {
                return err(here.number, "a list item with nothing in it");
            }
            return err(here.number, "a key where this list's items are");
        };
        let number = here.number;
        // Where the item's own content starts, which is what its
        // continuation lines must line up with: `- name: x` puts the
        // mapping at the column of `name`.
        let column = here.indent + (here.text.len() - rest.len());
        *at += 1;
        if rest.contains(": ") || rest.ends_with(':') {
            // `- key: value`, the first entry of a mapping whose remaining
            // entries are the following lines at the same column.
            let mut inner = vec![Line { number, indent: column, text: rest }];
            while let Some(l) = lines.get(*at).filter(|l| l.indent >= column) {
                inner.push(l.clone());
                *at += 1;
            }
            let mut inner_at = 0;
            let node = mapping(&inner, &mut inner_at, column, depth + 1)?;
            if inner_at < inner.len() {
                return err(inner[inner_at].number, "this line is indented oddly for its block");
            }
            items.push(node);
        } else {
            items.push(Node { line: number, value: scalar_or_flow(number, rest)? });
        }
    }
    Ok(Node { line, value: Value::List(items) })
}

/// Split `key: value` into its two halves. The key is plain text — quoted
/// keys are not in this subset, because no file Haven reads has one.
fn split_key<'a>(line: &Line<'a>) -> Result<(String, &'a str), Error> {
    let text = line.text;
    let end = match text.find(": ") {
        Some(at) => at,
        None if text.ends_with(':') => text.len() - 1,
        None => return err(line.number, "expected `key: value`"),
    };
    let key = text[..end].trim();
    if key.is_empty() {
        return err(line.number, "a value with no key");
    }
    if key.contains(['"', '\'', '[', ']', ':']) {
        return err(line.number, format!("`{key}` is not a plain key"));
    }
    Ok((key.to_string(), text[end + 1..].trim_start()))
}

/// A scalar, or a one-line `[a, b, c]` list.
fn scalar_or_flow(line: usize, text: &str) -> Result<Value, Error> {
    let text = text.trim();
    if let Some(inner) = text.strip_prefix('[') {
        let Some(inner) = inner.strip_suffix(']') else {
            return err(line, "a `[` list that does not close on the same line");
        };
        let inner = inner.trim();
        if inner.is_empty() {
            return Ok(Value::List(Vec::new()));
        }
        let mut items = Vec::new();
        for part in split_flow(line, inner)? {
            items.push(Node { line, value: Value::Scalar(unquote(line, part.trim())?) });
        }
        return Ok(Value::List(items));
    }
    Ok(Value::Scalar(unquote(line, text)?))
}

/// Split a flow list on commas that are not inside quotes.
fn split_flow(line: usize, text: &str) -> Result<Vec<&str>, Error> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut quote: Option<char> = None;
    for (i, c) in text.char_indices() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(c),
            (None, ',') => {
                parts.push(&text[start..i]);
                start = i + 1;
            }
            (None, _) => {}
        }
    }
    if quote.is_some() {
        return err(line, "a quote that is never closed");
    }
    parts.push(&text[start..]);
    if parts.iter().any(|p| p.trim().is_empty()) {
        return err(line, "an empty item in a `[` list");
    }
    Ok(parts)
}

/// Take the quotes off a scalar, if it has any.
///
/// Single quotes are literal, as in YAML. Double quotes understand `\\`,
/// `\"` and `\n` and nothing else — the full escape vocabulary is a parser
/// of its own, and no index needs `\u`.
fn unquote(line: usize, text: &str) -> Result<String, Error> {
    let mut chars = text.chars();
    match (chars.next(), text.len()) {
        (Some('\''), n) if n >= 2 && text.ends_with('\'') => Ok(text[1..n - 1].to_string()),
        (Some('"'), n) if n >= 2 && text.ends_with('"') => {
            let mut out = String::with_capacity(n);
            let mut inner = text[1..n - 1].chars();
            while let Some(c) = inner.next() {
                if c != '\\' {
                    out.push(c);
                    continue;
                }
                match inner.next() {
                    Some('\\') => out.push('\\'),
                    Some('"') => out.push('"'),
                    Some('n') => out.push('\n'),
                    Some(other) => return err(line, format!("`\\{other}` is not an escape here")),
                    None => return err(line, "a `\\` at the end of a quoted value"),
                }
            }
            Ok(out)
        }
        (Some('\'' | '"'), _) => err(line, "a quote that is never closed"),
        // A plain scalar, which is where the rest of YAML hides: a leading
        // sigil makes one of these an alias, an anchor, a tag, a flow
        // mapping or a block scalar, and reading it as text would mean
        // Haven and a real YAML parser disagreeing about what an index
        // says. Quoting it is always available to a repository that means
        // the character literally.
        _ => match plain_sigil(text) {
            Some(what) => err(line, format!("{what} is more YAML than Haven reads; quote it")),
            None => Ok(text.to_string()),
        },
    }
}

/// What a plain scalar's first character makes it, if it makes it anything
/// other than text.
fn plain_sigil(text: &str) -> Option<&'static str> {
    Some(match text.chars().next()? {
        '*' => "an alias",
        '&' => "an anchor",
        '!' => "a tag",
        '{' | '}' => "a flow mapping",
        ']' => "a stray `]`",
        '|' | '>' => "a block scalar",
        '`' | '@' => "a reserved character",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(text: &str) -> Node {
        parse(text).unwrap_or_else(|e| panic!("should parse: {e}"))
    }

    fn fails(text: &str) -> Error {
        parse(text).expect_err("should not parse")
    }

    #[test]
    fn a_flat_mapping_is_keys_and_values() {
        let doc = ok("repo: haven-test\ndescription: some plugins\n");
        assert_eq!(doc.get("repo").and_then(Node::as_str), Some("haven-test"));
        assert_eq!(doc.get("description").and_then(Node::as_str), Some("some plugins"));
        assert_eq!(doc.get("missing"), None);
    }

    #[test]
    fn a_list_of_mappings_is_the_shape_an_index_is() {
        let doc = ok("\
plugins:
  - name: gitline
    version: 0.2.1
  - name: urls
    version: 1.0.0
");
        let list = doc.get("plugins").and_then(Node::as_list).expect("a list");
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].get("name").and_then(Node::as_str), Some("gitline"));
        assert_eq!(list[1].get("version").and_then(Node::as_str), Some("1.0.0"));
    }

    #[test]
    fn a_flow_list_is_read_and_an_empty_one_is_allowed() {
        let doc = ok("events: [pane-focus, cwd-change]\nnet: []\n");
        let events = doc.get("events").and_then(Node::as_list).expect("a list");
        assert_eq!(
            events.iter().filter_map(Node::as_str).collect::<Vec<_>>(),
            ["pane-focus", "cwd-change"]
        );
        assert!(doc.get("net").and_then(Node::as_list).expect("a list").is_empty());
    }

    #[test]
    fn nested_blocks_nest() {
        let doc = ok("\
capabilities:
  exec:
    - git
  read-pane-content: false
");
        let caps = doc.get("capabilities").expect("a block");
        assert_eq!(caps.get("read-pane-content").and_then(Node::as_str), Some("false"));
        let exec = caps.get("exec").and_then(Node::as_list).expect("a list");
        assert_eq!(exec[0].as_str(), Some("git"));
    }

    #[test]
    fn comments_and_blank_lines_are_not_content() {
        let doc = ok("# a repository\n\nrepo: x\n\n   # indented comment\nkey: y\n");
        assert_eq!(doc.get("repo").and_then(Node::as_str), Some("x"));
        assert_eq!(doc.get("key").and_then(Node::as_str), Some("y"));
    }

    #[test]
    fn a_hash_inside_a_value_stays_in_the_value() {
        // Stripping it would silently change data, and a URL fragment or a
        // colour is exactly the sort of value that has one.
        let doc = ok("note: a#b\n");
        assert_eq!(doc.get("note").and_then(Node::as_str), Some("a#b"));
    }

    #[test]
    fn quotes_come_off_and_escapes_are_small() {
        let doc = ok("a: 'it: literal'\nb: \"say \\\"hi\\\"\"\nc: \"one\\ntwo\"\n");
        assert_eq!(doc.get("a").and_then(Node::as_str), Some("it: literal"));
        assert_eq!(doc.get("b").and_then(Node::as_str), Some("say \"hi\""));
        assert_eq!(doc.get("c").and_then(Node::as_str), Some("one\ntwo"));
        assert!(parse("a: \"\\u0041\"\n").is_err(), "`\\u` is not in the subset");
    }

    #[test]
    fn every_node_knows_its_line() {
        let doc = ok("repo: x\nplugins:\n  - name: a\n    version: 1\n");
        let first = &doc.get("plugins").and_then(Node::as_list).unwrap()[0];
        assert_eq!(first.get("version").unwrap().line, 4, "a complaint has to point somewhere");
    }

    #[test]
    fn the_yaml_this_does_not_read_is_refused_by_line() {
        // Each of these is a real YAML document and none of them is one
        // Haven will read. The point of the test is the line number: a
        // repository fixing its index needs to be told where.
        for (text, line, needle) in [
            ("repo: x\nalias: *other\n", 2, "alias"),
            ("repo: x\nanchor: &a b\n", 2, "anchor"),
            ("---\nrepo: x\n", 1, "documents"),
            ("repo: x\nflow: {a: b}\n", 2, "flow mapping"),
            ("repo: x\n\tnested: y\n", 2, "tabs"),
            ("repo: x\nrepo: y\n", 2, "set twice"),
            ("repo: x\nbad\n", 2, "key: value"),
            ("repo: x\nlist: [a, b\n", 2, "does not close"),
            ("repo: x\nopen: 'a\n", 2, "never closed"),
            ("repo: x\nempty:\n", 2, "nothing under it"),
            ("repo: x\nlist: [a, , b]\n", 2, "empty item"),
            ("- a\n- b\n", 1, "`key: value`"),
            ("", 1, "empty"),
        ] {
            let e = fails(text);
            assert_eq!(e.line, line, "wrong line for {text:?}: {e}");
            assert!(e.message.contains(needle), "wrong complaint for {text:?}: {e}");
        }
    }

    #[test]
    fn a_list_and_a_mapping_cannot_share_a_block() {
        assert!(parse("plugins:\n  - a\n  b: c\n").is_err());
        assert!(parse("plugins:\n  b: c\n  - a\n").is_err());
    }

    #[test]
    fn nothing_here_recurses_without_bound() {
        // A hostile index is the normal case: this parser runs on bytes
        // from the network before a signature has been checked, and deep
        // nesting is the cheapest way to turn a parser into a crash. This
        // test was written believing the indentation bounded the depth. It
        // does not — a thousand levels is a thousand short lines — and the
        // first run of it overflowed the stack. Hence `MAX_DEPTH`.
        let deep: String = (0..2000).map(|i| format!("{}k:\n", " ".repeat(i))).collect();
        let e = parse(&deep).expect_err("deep nesting must be refused, not followed");
        assert!(e.message.contains("nested more than"), "{e}");
        // And the degenerate ones do not panic.
        for text in ["[", "-", "- ", ":", ": x", "a:\n a:\n  a:\n", "\u{feff}a: b\n"] {
            let _ = parse(text);
        }
    }
}
