//! Markdown-light: enough of a chat reply's markdown to read well — paragraphs, `#` headings, `-`/`*`/`1.` lists,
//! fenced code blocks, `**bold**` and `` `code` `` inline. Everything else shows as written.
//!
//! Parsing is separate from drawing so it happens once per text, not once per frame: [`Doc`] is a parsed text (cache
//! it per finished message), and [`Streamed`] follows a text that only grows (a reply streaming in) by re-parsing
//! only its last block — every block before it is final once a later block has started.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

use familiar_ui::theme::{RADIUS_CHIP, Theme, text};
use gpui::{
    Div, FontWeight, HighlightStyle, IntoElement, ParentElement as _, SharedString, Styled as _, StyledText, div, px,
};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mark {
    Bold,
    Code,
}

/// A line's text with its inline marks resolved (`**`/`` ` `` taken out).
#[derive(Debug, Clone, PartialEq)]
struct Inline {
    text: SharedString,
    marks: Vec<(Range<usize>, Mark)>,
}

#[derive(Debug, Clone, PartialEq)]
enum Block {
    Para(Inline),
    Heading(Inline),
    Item(SharedString, Inline),
    Code(SharedString),
}

/// A parsed text, ready to draw any number of times.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Doc {
    blocks: Vec<Block>,
}

impl Doc {
    pub fn parse(src: &str) -> Self {
        Self { blocks: parse_from(src, 0).into_iter().map(|(_, b)| b).collect() }
    }

    pub fn render(&self, theme: &Theme) -> Div {
        render_blocks(&self.blocks, theme)
    }
}

/// Parse and draw `src` in one go (texts drawn rarely; a chat caches its [`Doc`]s).
pub fn render(src: &str, theme: &Theme) -> Div {
    Doc::parse(src).render(theme)
}

/// A text that grows at its end (a reply streaming in), parsed incrementally: [`Streamed::update`] with the whole
/// text so far re-parses from the start of its last block only. A text that is not the last one plus more (cleared,
/// replaced) is parsed again from the start.
#[derive(Debug, Default)]
pub struct Streamed {
    /// The text of the last update.
    last: String,
    /// Where the last block starts: the blocks before it are final, since appending text can't change them.
    tail_at: usize,
    done: Vec<Block>,
    /// The last block (if any), re-parsed on every update.
    tail: Option<Block>,
}

impl Streamed {
    /// Bring the parse up to `src`. Returns whether it changed.
    pub fn update(&mut self, src: &str) -> bool {
        if src == self.last {
            return false;
        }
        if !src.starts_with(self.last.as_str()) {
            self.done.clear();
            self.tail_at = 0;
        }
        let mut parsed = parse_from(src, self.tail_at);
        self.tail = parsed.pop().map(|(at, block)| {
            self.tail_at = at;
            block
        });
        self.done.extend(parsed.into_iter().map(|(_, b)| b));
        self.last.clear();
        self.last.push_str(src);
        true
    }

    pub fn render(&self, theme: &Theme) -> Div {
        render_blocks(self.done.iter().chain(self.tail.iter()), theme)
    }

    #[cfg(test)]
    fn blocks(&self) -> Vec<Block> {
        self.done.iter().chain(self.tail.iter()).cloned().collect()
    }
}

/// Parse `src[start..]` (`start` at a line start where no block is open) into blocks with the byte offset each starts
/// at. Lines are split like [`str::lines`].
fn parse_from(src: &str, start: usize) -> Vec<(usize, Block)> {
    let mut out = Vec::new();
    // The open paragraph: where it starts and its lines.
    let mut para: Option<(usize, Vec<&str>)> = None;
    let mut code: Option<(usize, Vec<&str>)> = None;
    let flush = |para: &mut Option<(usize, Vec<&str>)>, out: &mut Vec<(usize, Block)>| {
        if let Some((at, lines)) = para.take() {
            out.push((at, Block::Para(inline(&lines.join("\n")))));
        }
    };
    let mut at = start;
    for raw in src[start..].split_inclusive('\n') {
        let line_at = at;
        at += raw.len();
        let line = match raw.strip_suffix('\n') {
            Some(l) => l.strip_suffix('\r').unwrap_or(l),
            None => raw,
        };
        let trimmed = line.trim_start();
        if let Some((_, lines)) = code.as_mut() {
            if trimmed.starts_with("```") {
                let (start, lines) = code.take().unwrap();
                out.push((start, Block::Code(lines.join("\n").into())));
            } else {
                lines.push(line);
            }
            continue;
        }
        if trimmed.starts_with("```") {
            flush(&mut para, &mut out);
            code = Some((line_at, Vec::new()));
        } else if trimmed.is_empty() {
            flush(&mut para, &mut out);
        } else if let Some(h) = trimmed.strip_prefix("### ").or(trimmed.strip_prefix("## ")).or(trimmed.strip_prefix("# ")) {
            flush(&mut para, &mut out);
            out.push((line_at, Block::Heading(inline(h))));
        } else if let Some(item) = trimmed.strip_prefix("- ").or(trimmed.strip_prefix("* ")) {
            flush(&mut para, &mut out);
            out.push((line_at, Block::Item("•".into(), inline(item))));
        } else if let Some((n, rest)) = trimmed.split_once(". ").filter(|(n, _)| !n.is_empty() && n.len() <= 3 && n.chars().all(|c| c.is_ascii_digit())) {
            flush(&mut para, &mut out);
            out.push((line_at, Block::Item(format!("{n}.").into(), inline(rest))));
        } else {
            para.get_or_insert_with(|| (line_at, Vec::new())).1.push(line);
        }
    }
    if let Some((start, lines)) = code {
        out.push((start, Block::Code(lines.join("\n").into())));
    }
    flush(&mut para, &mut out);
    out
}

/// `**bold**` and `` `code` `` as marks over the plain text.
fn inline(src: &str) -> Inline {
    let mut out = String::with_capacity(src.len());
    let mut marks = Vec::new();
    let mut rest = src;
    while !rest.is_empty() {
        let bold = rest.find("**");
        let code = rest.find('`');
        let next = match (bold, code) {
            (Some(b), Some(c)) => Some(if b <= c { (b, "**") } else { (c, "`") }),
            (Some(b), None) => Some((b, "**")),
            (None, Some(c)) => Some((c, "`")),
            (None, None) => None,
        };
        let Some((at, marker)) = next else {
            out.push_str(rest);
            break;
        };
        let after = &rest[at + marker.len()..];
        let Some(end) = after.find(marker).filter(|e| *e > 0) else {
            out.push_str(&rest[..at + marker.len()]);
            rest = after;
            continue;
        };
        out.push_str(&rest[..at]);
        let start = out.len();
        out.push_str(&after[..end]);
        marks.push((start..out.len(), if marker == "**" { Mark::Bold } else { Mark::Code }));
        rest = &after[end + marker.len()..];
    }
    Inline { text: out.into(), marks }
}

fn styled(t: &Inline, theme: &Theme) -> StyledText {
    let highlights = t.marks.iter().map(|(range, mark)| {
        let style = match mark {
            Mark::Bold => HighlightStyle { font_weight: Some(FontWeight::SEMIBOLD), ..Default::default() },
            Mark::Code => HighlightStyle { background_color: Some(theme.sunken), color: Some(theme.accent), ..Default::default() },
        };
        (range.clone(), style)
    });
    StyledText::new(t.text.clone()).with_highlights(highlights.collect::<Vec<_>>())
}

fn render_blocks<'a>(blocks: impl IntoIterator<Item = &'a Block>, theme: &Theme) -> Div {
    let mut col = div().flex().flex_col().gap(px(8.0)).min_w_0();
    for b in blocks {
        col = col.child(match b {
            Block::Para(t) => div().child(styled(t, theme)).into_any_element(),
            Block::Heading(t) => div().font_weight(FontWeight::SEMIBOLD).child(styled(t, theme)).into_any_element(),
            Block::Item(mark, t) => div()
                .flex()
                .gap(px(8.0))
                .child(div().flex_none().text_color(theme.muted).child(mark.clone()))
                .child(div().flex_1().min_w_0().child(styled(t, theme)))
                .into_any_element(),
            Block::Code(t) => div()
                .px(px(12.0))
                .py(px(9.0))
                .rounded(px(RADIUS_CHIP))
                .bg(theme.sunken)
                .font_family(theme.font_mono.clone())
                .text_size(px(text::CAPTION))
                .child(t.clone())
                .into_any_element(),
        });
    }
    col
}

/// Parsed messages by id, kept in step with the shown list by [`DocCache::sync`] (when it loads, not per frame): a
/// message is parsed again only when its text changes (length and hash).
#[derive(Default)]
pub struct DocCache {
    docs: HashMap<Uuid, (u64, Arc<Doc>)>,
}

impl DocCache {
    /// Keep exactly these messages, parsing new or changed ones.
    pub fn sync<'a>(&mut self, messages: impl IntoIterator<Item = (Uuid, &'a str)>) {
        let mut next = HashMap::new();
        for (id, src) in messages {
            let key = fingerprint(src);
            let doc = match self.docs.remove(&id) {
                Some((k, doc)) if k == key => doc,
                _ => Arc::new(Doc::parse(src)),
            };
            next.insert(id, (key, doc));
        }
        self.docs = next;
    }

    pub fn get(&self, id: Uuid) -> Option<Arc<Doc>> {
        self.docs.get(&id).map(|(_, d)| d.clone())
    }
}

/// The text's hash, mixed with its length.
fn fingerprint(src: &str) -> u64 {
    use std::hash::{Hash as _, Hasher as _};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    src.hash(&mut h);
    h.finish() ^ (src.len() as u64).rotate_left(32)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every construct, CRLF lines, a 4-digit "number", non-ASCII text and a fence left open at the end.
    const SAMPLE: &str = "# Hi\n\nOne **bold** and `code`\ntwo\n- a\n* b\n1. c\n123. d\n1234. not an item\n\n```rust\nlet x = 1;\n\n  indented\n```\nAfter the code\r\nCRLF line\r\n\n## Sub — café ✓\n- `unterminated and **half\n   ### indented heading\n```\nleft open\n";

    fn texts(doc: &[Block]) -> Vec<String> {
        doc.iter()
            .map(|b| match b {
                Block::Para(t) => format!("P {}", t.text),
                Block::Heading(t) => format!("H {}", t.text),
                Block::Item(m, t) => format!("I {m} {}", t.text),
                Block::Code(c) => format!("C {c}"),
            })
            .collect()
    }

    #[test]
    fn splits_blocks() {
        let b = Doc::parse("# Hi\n\nOne\ntwo\n- a\n1. b\n```\ncode\n```").blocks;
        assert_eq!(texts(&b), ["H Hi", "P One\ntwo", "I • a", "I 1. b", "C code"]);
    }

    #[test]
    fn inline_marks() {
        let t = inline("a **b** `c` **open");
        assert_eq!(t.text.as_ref(), "a b c **open");
        assert_eq!(t.marks, vec![(2..3, Mark::Bold), (4..5, Mark::Code)]);
    }

    /// Feed `text` growing by `steps` (cycled) and check the memoised parse against a full parse at every step.
    fn check_growing(text: &str, steps: &[usize]) {
        let mut s = Streamed::default();
        let mut end = 0;
        let mut i = 0;
        while end < text.len() {
            end = (end + steps[i % steps.len()]).min(text.len());
            while !text.is_char_boundary(end) {
                end += 1;
            }
            i += 1;
            s.update(&text[..end]);
            assert_eq!(s.blocks(), Doc::parse(&text[..end]).blocks, "after {end} bytes of {text:?}");
        }
    }

    #[test]
    fn memoised_parse_equals_full_parse_for_growing_text() {
        // Every prefix, one byte at a time.
        check_growing(SAMPLE, &[1]);
        // Uneven chunks over a longer reply.
        let long = SAMPLE.repeat(12);
        check_growing(&long, &[1, 7, 3, 40, 2, 17, 64, 5]);
        check_growing(&long, &[40]);
    }

    #[test]
    fn memoised_parse_only_reparses_the_last_block() {
        let mut s = Streamed::default();
        s.update("# A\n\npara\n- item");
        assert_eq!(texts(&s.done), ["H A", "P para"]);
        assert_eq!(s.tail_at, "# A\n\npara\n".len());
        assert!(!s.update("# A\n\npara\n- item"), "same text: nothing to do");
    }

    #[test]
    fn memoised_parse_starts_over_when_the_text_is_replaced() {
        let mut s = Streamed::default();
        for text in ["para\n- item one", "para\n-x", "", "# New\n\nbody", "# Newer heading\n\nbody"] {
            s.update(text);
            assert_eq!(s.blocks(), Doc::parse(text).blocks, "{text:?}");
        }
    }

    #[test]
    fn doc_cache_parses_a_message_once() {
        let (a, b) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let mut cache = DocCache::default();
        cache.sync([(a, "# A"), (b, "b")]);
        let first = cache.get(a).unwrap();
        cache.sync([(a, "# A"), (b, "b changed")]);
        assert!(Arc::ptr_eq(&first, &cache.get(a).unwrap()), "unchanged text keeps its parse");
        assert_eq!(texts(&cache.get(b).unwrap().blocks), ["P b changed"]);
        cache.sync([(b, "b changed")]);
        assert!(cache.get(a).is_none(), "messages no longer shown are dropped");
    }
}
