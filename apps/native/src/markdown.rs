//! Markdown-light: enough of a chat reply's markdown to read well — paragraphs, `#` headings, `-`/`*`/`1.` lists,
//! fenced code blocks, `**bold**` and `` `code` `` inline. Everything else shows as written.

use familiar_ui::theme::{RADIUS_CHIP, Theme, text};
use gpui::{
    Div, FontWeight, HighlightStyle, IntoElement, ParentElement as _, SharedString, Styled as _, StyledText, div, px,
};

enum Block {
    Para(String),
    Heading(String),
    Item(String, String),
    Code(String),
}

fn blocks(src: &str) -> Vec<Block> {
    let mut out = Vec::new();
    let mut para: Vec<&str> = Vec::new();
    let mut code: Option<Vec<&str>> = None;
    let flush = |para: &mut Vec<&str>, out: &mut Vec<Block>| {
        if !para.is_empty() {
            out.push(Block::Para(para.join("\n")));
            para.clear();
        }
    };
    for line in src.lines() {
        let trimmed = line.trim_start();
        if let Some(lines) = code.as_mut() {
            if trimmed.starts_with("```") {
                out.push(Block::Code(lines.join("\n")));
                code = None;
            } else {
                lines.push(line);
            }
            continue;
        }
        if trimmed.starts_with("```") {
            flush(&mut para, &mut out);
            code = Some(Vec::new());
        } else if trimmed.is_empty() {
            flush(&mut para, &mut out);
        } else if let Some(h) = trimmed.strip_prefix("### ").or(trimmed.strip_prefix("## ")).or(trimmed.strip_prefix("# ")) {
            flush(&mut para, &mut out);
            out.push(Block::Heading(h.to_owned()));
        } else if let Some(item) = trimmed.strip_prefix("- ").or(trimmed.strip_prefix("* ")) {
            flush(&mut para, &mut out);
            out.push(Block::Item("•".into(), item.to_owned()));
        } else if let Some((n, rest)) = trimmed.split_once(". ").filter(|(n, _)| !n.is_empty() && n.len() <= 3 && n.chars().all(|c| c.is_ascii_digit())) {
            flush(&mut para, &mut out);
            out.push(Block::Item(format!("{n}."), rest.to_owned()));
        } else {
            para.push(line);
        }
    }
    if let Some(lines) = code {
        out.push(Block::Code(lines.join("\n")));
    }
    flush(&mut para, &mut out);
    out
}

/// `**bold**` and `` `code` `` as highlights over the plain text.
fn inline(src: &str, theme: &Theme) -> StyledText {
    let mut out = String::with_capacity(src.len());
    let mut highlights = Vec::new();
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
        let style = if marker == "**" {
            HighlightStyle { font_weight: Some(FontWeight::SEMIBOLD), ..Default::default() }
        } else {
            HighlightStyle { background_color: Some(theme.sunken), color: Some(theme.accent), ..Default::default() }
        };
        highlights.push((start..out.len(), style));
        rest = &after[end + marker.len()..];
    }
    StyledText::new(SharedString::from(out)).with_highlights(highlights)
}

pub fn render(src: &str, theme: &Theme) -> Div {
    let mut col = div().flex().flex_col().gap(px(8.0)).min_w_0();
    for b in blocks(src) {
        col = col.child(match b {
            Block::Para(t) => div().child(inline(&t, theme)).into_any_element(),
            Block::Heading(t) => div().font_weight(FontWeight::SEMIBOLD).child(inline(&t, theme)).into_any_element(),
            Block::Item(mark, t) => div()
                .flex()
                .gap(px(8.0))
                .child(div().flex_none().text_color(theme.muted).child(mark))
                .child(div().flex_1().min_w_0().child(inline(&t, theme)))
                .into_any_element(),
            Block::Code(t) => div()
                .px(px(12.0))
                .py(px(9.0))
                .rounded(px(RADIUS_CHIP))
                .bg(theme.sunken)
                .font_family(theme.font_mono.clone())
                .text_size(px(text::CAPTION))
                .child(t)
                .into_any_element(),
        });
    }
    col
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_blocks() {
        let b = blocks("# Hi\n\nOne\ntwo\n- a\n1. b\n```\ncode\n```");
        assert!(matches!(&b[0], Block::Heading(h) if h == "Hi"));
        assert!(matches!(&b[1], Block::Para(p) if p == "One\ntwo"));
        assert!(matches!(&b[2], Block::Item(m, t) if m == "•" && t == "a"));
        assert!(matches!(&b[3], Block::Item(m, t) if m == "1." && t == "b"));
        assert!(matches!(&b[4], Block::Code(c) if c == "code"));
    }
}
