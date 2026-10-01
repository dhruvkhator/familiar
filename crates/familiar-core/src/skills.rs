//! Mirror `<workspace>/.claude/skills/*/SKILL.md` into the `skills` table so the UI can show what a bot has learned.

use anyhow::Result;

use crate::daemon::Ctx;
use crate::db::Bot;

pub async fn mirror(ctx: &Ctx, bot: &Bot) -> Result<()> {
    let dir = ctx.cfg.bots_dir().join(&bot.slug).join(".claude").join("skills");
    let mut found = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let file = entry.path().join("SKILL.md");
            let Ok(text) = std::fs::read_to_string(&file) else { continue };
            let folder = entry.file_name().to_string_lossy().into_owned();
            let (name, description, body) = parse(&text, &folder);
            found.push((name, description, body));
        }
    }
    ctx.db.sync_skills(bot.id, &found).await
}

/// Frontmatter `name` / `description`, then the body. Falls back to the folder name.
fn parse(text: &str, folder: &str) -> (String, String, String) {
    let (mut name, mut description) = (folder.to_owned(), String::new());
    let mut body = text;
    if let Some(rest) = text.strip_prefix("---") {
        if let Some(end) = rest.find("\n---") {
            for line in rest[..end].lines() {
                if let Some((k, v)) = line.split_once(':') {
                    let v = v.trim().trim_matches('"').to_owned();
                    match k.trim() {
                        "name" if !v.is_empty() => name = v,
                        "description" => description = v,
                        _ => {}
                    }
                }
            }
            body = rest[end + 4..].trim_start_matches(['\r', '\n']);
        }
    }
    (name, description, body.chars().take(20_000).collect())
}

#[cfg(test)]
mod tests {
    #[test]
    fn frontmatter() {
        let (n, d, b) = super::parse("---\nname: invoice-check\ndescription: \"Checks invoices\"\n---\n\n1. Open\n", "x");
        assert_eq!((n.as_str(), d.as_str(), b.as_str()), ("invoice-check", "Checks invoices", "1. Open\n"));
        assert_eq!(super::parse("just steps", "folder").0, "folder");
    }
}
