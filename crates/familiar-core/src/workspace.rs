//! A bot's "computer": its folder on this machine, plus the instructions it runs with.

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::db::Bot;

const HOUSE_RULES: &str = "\
## How you work
- This folder is your computer. Keep your files, notes and downloads here.
- You are always on: you may be woken by a schedule with no human watching. Finish the job end to end,
  and only stop when you need a decision from your owner.
- Anything irreversible or outward-facing (sending messages, purchases, deleting data, posting) goes through
  an approval. If an approval is denied, do not retry the same action another way.
- When you figure out a repeatable procedure, save it as a skill in `.claude/skills/<name>/SKILL.md`
  (frontmatter `name` and `description`, then the steps) so you can run it again later.
- Be brief in your final answer: what you did, what changed, what needs your owner.
";

/// Returns (workspace dir, system prompt file). The prompt lives outside the workspace: restricted mode confines
/// the bot's file tools to its workspace, so it cannot rewrite its own instructions.
pub fn prepare(bots_dir: &Path, bot: &Bot, memories: &[String]) -> Result<(PathBuf, PathBuf)> {
    // The slug becomes a path segment: never let it escape bots_dir.
    let valid = !bot.slug.is_empty()
        && !bot.slug.starts_with('-')
        && bot.slug.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    anyhow::ensure!(valid, "invalid bot slug `{}`", bot.slug);
    let dir = bots_dir.join(&bot.slug);
    std::fs::create_dir_all(dir.join(".claude").join("skills"))?;

    let mut md = format!("# {}\n\nYou are {}, an always-on AI teammate running through Familiar.\n\n", bot.name, bot.name);
    if let Some(persona) = bot.persona.as_deref().filter(|p| !p.trim().is_empty()) {
        md.push_str(persona.trim());
        md.push_str("\n\n");
    }
    md.push_str(HOUSE_RULES);
    if !memories.is_empty() {
        md.push_str("\n## What your owner taught you\n");
        for m in memories {
            md.push_str(&format!("- {}\n", m.trim().replace('\n', " ")));
        }
    }
    let prompts = bots_dir.join(".prompts");
    std::fs::create_dir_all(&prompts)?;
    let prompt = prompts.join(format!("{}.md", bot.slug));
    std::fs::write(&prompt, md)?;
    Ok((dir, prompt))
}
