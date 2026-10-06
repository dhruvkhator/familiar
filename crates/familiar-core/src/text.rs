//! Text the owner approves must show whole: characters that don't show, or change how the rest reads, are written
//! out (Telegram approvals) or refused (drafts approved through the API). The native app's approval cards use the same
//! set.

/// Characters that don't show, or change how the rest reads (the app's approval cards use the same set): Cc, Cf
/// (bidi overrides, zero-width, tags), Zl/Zp, Co, Cn, plus blank-looking fillers and variation selectors. `\n` and
/// `\t` count only on single-line fields.
pub fn hidden_char(c: char, multiline: bool) -> bool {
    use unicode_properties::{GeneralCategory as G, UnicodeGeneralCategory as _};
    if multiline && (c == '\n' || c == '\t') {
        return false;
    }
    matches!(
        c.general_category(),
        G::Control | G::Format | G::LineSeparator | G::ParagraphSeparator | G::PrivateUse | G::Unassigned | G::Surrogate
    ) || matches!(
        c,
        '\u{034F}'
            | '\u{115F}'
            | '\u{1160}'
            | '\u{180B}'..='\u{180D}'
            | '\u{3164}'
            | '\u{FFA0}'
            | '\u{FE00}'..='\u{FE0F}'
            | '\u{E0100}'..='\u{E01EF}'
    )
}

/// `s` with every hidden character written out as `⟨U+202E⟩`, and whether there were any.
pub fn reveal(s: &str, multiline: bool) -> (String, bool) {
    let mut out = String::with_capacity(s.len());
    let mut hidden = false;
    for c in s.chars() {
        if hidden_char(c, multiline) {
            hidden = true;
            out.push_str(&format!("⟨U+{:04X}⟩", c as u32));
        } else {
            out.push(c);
        }
    }
    (out, hidden)
}

/// Whether `s` has any hidden character.
pub fn has_hidden(s: &str, multiline: bool) -> bool {
    s.chars().any(|c| hidden_char(c, multiline))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_by_category() {
        for s in ["a\u{202E}b", "a\u{200B}b", "tag\u{E0041}", "x\u{FE0F}", "\u{3164}", "cr\rlf"] {
            assert!(has_hidden(s, true), "{s:?}");
        }
        assert!(has_hidden("a\nb", false));
        for s in ["a\nb\tc", "café 日本 😀", "quotes “ok” — dash"] {
            assert!(!has_hidden(s, true), "{s:?}");
        }
        assert_eq!(reveal("x\u{202E}y", true), ("x⟨U+202E⟩y".to_owned(), true));
    }
}
