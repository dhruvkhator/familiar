//! Rules → Claude Code permission settings, plus matching for `review` rules the daemon handles itself.

use serde_json::{Value, json};

use crate::db::Rule;

/// Built-in tools a proactive (research-only) run may use. Enforced with `--tools`, not by the callback,
/// because read-only tools never reach the permission prompt.
pub const RESEARCH_TOOLS: &[&str] = &["Read", "Glob", "Grep", "WebSearch", "WebFetch"];

/// Built-in tools for normal runs. Anything that writes or executes still needs approval.
pub const FULL_TOOLS: &[&str] = &["Bash", "Read", "Write", "Edit", "Glob", "Grep", "WebFetch", "WebSearch", "TodoWrite", "Skill"];

/// Rules → Claude Code `permissions` settings.
/// Owner `allow` rules for Bash are *not* forwarded: every non-trivial shell command reaches the daemon, which checks
/// [`always_human`] first and only then applies the owner's allow rules ([`owner_allows`]). `review` rules go in as
/// `ask` so they reach us too. Research-only runs may fetch any page (every prompt in them is denied anyway).
pub fn settings(rules: &[Rule], research: bool) -> Value {
    let mut allow: Vec<&str> = FAMILIAR_ALLOWED.to_vec();
    let mut deny: Vec<&str> = vec![];
    let mut ask: Vec<&str> = vec![];
    if research {
        allow.push("WebFetch");
    }
    for r in rules {
        match r.decision.as_str() {
            "allow" if is_bash(&r.pattern) => {}
            "allow" => allow.push(&r.pattern),
            "deny" => deny.push(&r.pattern),
            _ => ask.push(&r.pattern),
        }
    }
    json!({ "allow": allow, "deny": deny, "ask": ask })
}

fn is_bash(pattern: &str) -> bool {
    let p = pattern.trim();
    p == "Bash" || p.starts_with("Bash(") || p == "*"
}

/// For engines without Claude Code's permission settings (Codex): what those settings would decide on their own.
/// `Some(false)` = an owner `deny` rule; `Some(true)` = pre-allowed (Familiar tools, look-only browser tools, owner
/// `allow` rules other than Bash); `None` = goes through [`crate::runner::decide_tool`].
pub fn preset(rules: &[Rule], tool: &str, input: &Value) -> Option<bool> {
    if rules.iter().any(|r| r.decision == "deny" && matches(&r.pattern, tool, input)) {
        return Some(false);
    }
    let allowed = FAMILIAR_ALLOWED.iter().any(|p| matches(p, tool, input))
        || rules.iter().any(|r| r.decision == "allow" && !is_bash(&r.pattern) && matches(&r.pattern, tool, input));
    allowed.then_some(true)
}

/// An owner `allow` rule the daemon applies itself (Bash rules, see [`settings`]).
pub fn owner_allows<'a>(rules: &'a [Rule], tool: &str, input: &Value) -> Option<&'a Rule> {
    rules.iter().filter(|r| r.decision == "allow" && is_bash(&r.pattern)).find(|r| matches(&r.pattern, tool, input))
}

/// The bot's own Familiar tools and the browser's look-but-don't-touch tools. Navigation is not here: it goes through
/// [`safe_navigation`] so the browser can't be pointed at local files or services.
const FAMILIAR_ALLOWED: &[&str] = &[
    "mcp__familiar",
    "WebSearch",
    "mcp__browser__browser_navigate_back",
    "mcp__browser__browser_snapshot",
    "mcp__browser__browser_find",
    "mcp__browser__browser_take_screenshot",
    "mcp__browser__browser_wait_for",
    "mcp__browser__browser_console_messages",
    "mcp__browser__browser_network_requests",
    "mcp__browser__browser_resize",
    "mcp__browser__browser_close",
];

/// Browser navigation to a public http(s) page is allowed without asking; anything else (file://, loopback,
/// private networks, odd schemes) goes to the owner.
pub fn safe_navigation(tool: &str, input: &Value) -> bool {
    if tool != "mcp__browser__browser_navigate" {
        return false;
    }
    let Some(url) = input["url"].as_str() else { return false };
    let lower = url.trim().to_ascii_lowercase();
    let Some(rest) = lower.strip_prefix("https://").or_else(|| lower.strip_prefix("http://")) else { return false };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host_port = authority.rsplit('@').next().unwrap_or_default();
    let host = if host_port.starts_with('[') {
        host_port.split(']').next().unwrap_or_default().trim_start_matches('[')
    } else {
        host_port.split(':').next().unwrap_or_default()
    };
    if host.is_empty() || host == "localhost" || host.ends_with(".localhost") || host.ends_with(".local") || !host.contains('.') && host.parse::<std::net::IpAddr>().is_err() {
        return false;
    }
    match host.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(ip)) => !(ip.is_loopback() || ip.is_private() || ip.is_link_local() || ip.is_unspecified()),
        Ok(std::net::IpAddr::V6(_)) => false,
        Err(_) => true,
    }
}

/// Actions that always need the owner, whatever the rules or the reviewer say: recursive deletes, installs,
/// privilege escalation, force pushes, piping downloads into a shell. Returns why.
pub fn always_human(tool: &str, input: &Value) -> Option<&'static str> {
    if tool != "Bash" {
        return None;
    }
    // Quotes don't matter for this check: `sh -c "rm -rf x"` is judged as `rm -rf x`.
    let cmd = input["command"].as_str().unwrap_or_default().to_ascii_lowercase().replace(['"', '\'', '`'], " ");
    let cmd = cmd.split_whitespace().collect::<Vec<_>>().join(" ");
    // The bot's browser runs with a DevTools port; driving it directly would bypass the browser approval rules.
    if ["/json/list", "/json/version", "/devtools/", "webSocketDebuggerUrl", "remote-debugging"]
        .iter()
        .any(|m| cmd.contains(&m.to_ascii_lowercase()))
    {
        return Some("talks to the browser's debugging port directly");
    }
    // Every segment of a compound command counts on its own.
    for seg in cmd.split(['|', ';', '&', '\n']).map(str::trim) {
        let mut words: Vec<&str> = seg.split(' ').collect();
        // Look through wrappers: `xargs rm -rf`, `env X=1 sudo …`, `timeout 5 rm -r …`.
        while words.len() > 1 {
            let first = words[0];
            let name = first.rsplit(['/', '\\']).next().unwrap_or(first);
            let name = name.strip_suffix(".exe").unwrap_or(name);
            let wrapper = matches!(
                name,
                "xargs" | "env" | "nohup" | "time" | "exec" | "timeout" | "nice" | "command" | "builtin" | "start-process"
                    // Shells are wrappers too: `bash -c …`, `cmd /c …`, `powershell -Command …`, any depth.
                    | "sh" | "bash" | "zsh" | "dash" | "cmd" | "powershell" | "pwsh"
            );
            let switch = first.starts_with('/') && first.len() <= 3; // cmd's /c /s /k
            if !(wrapper || switch || first.starts_with('-') || first.contains('=') || first.parse::<f64>().is_ok()) {
                break;
            }
            words.remove(0);
        }
        let prog = words.first().map(|w| w.rsplit(['/', '\\']).next().unwrap_or(w)).unwrap_or_default();
        let flags: String = words.iter().filter(|w| w.starts_with('-')).map(|w| w.trim_start_matches('-')).collect();
        let has = |w: &str| words.contains(&w);
        let reason = match prog {
            "sudo" | "su" | "doas" | "runas" => Some("runs with elevated privileges"),
            "python" | "python3" | "py" | "node" | "perl" | "ruby" | "deno" | "bun" | "php"
                if words.iter().any(|w| matches!(*w, "-c" | "-e" | "--eval" | "-r" | "eval")) =>
            {
                Some("runs inline code that could do anything")
            }
            "iex" | "invoke-expression" | "invoke-command" | "icm" => Some("runs dynamically built code"),
            "rm" if flags.contains('r') || flags.contains("recursive") => Some("deletes recursively"),
            "rmdir" | "rd" | "del" | "erase" if has("/s") => Some("deletes recursively"),
            "remove-item" | "ri" if words.iter().any(|w| w.starts_with("-r")) => Some("deletes recursively"),
            "find" if has("-delete") || seg.contains("-exec rm") => Some("deletes files in bulk"),
            "git" if has("push") && words.iter().any(|w| *w == "-f" || w.starts_with("--force") || w.starts_with('+')) => {
                Some("force-pushes")
            }
            "git" if has("clean") && flags.contains('f') => Some("deletes untracked files"),
            "git" if has("reset") && has("--hard") => Some("discards local changes"),
            "npm" | "pnpm" | "yarn" | "bun"
                if (has("-g") || has("--global") || has("global")) && (has("install") || has("i") || has("add")) =>
            {
                Some("installs software globally")
            }
            "pip" | "pip3" | "pipx" | "uv" if has("install") => Some("installs software"),
            "winget" | "choco" | "scoop" | "brew" | "apt" | "apt-get" | "dnf" | "yum" | "pacman" | "snap"
                if words.iter().any(|w| matches!(*w, "install" | "upgrade" | "uninstall" | "remove" | "-s")) =>
            {
                Some("installs or removes software")
            }
            "cargo" if has("install") => Some("installs software"),
            "msiexec" | "format" | "mkfs" | "diskpart" | "shutdown" | "reboot" => Some("changes the system"),
            "dd" if seg.contains("of=") => Some("writes raw disk data"),
            "sh" | "bash" | "zsh" | "python" | "python3" | "node"
                if cmd.contains("curl ") || cmd.contains("wget ") || cmd.contains("iwr ") || cmd.contains("invoke-webrequest") =>
            {
                Some("runs a downloaded script")
            }
            "chmod" | "chown" | "icacls" | "takeown" if flags.contains('r') || has("/t") => Some("changes permissions recursively"),
            "reg" if has("delete") || has("add") => Some("edits the Windows registry"),
            "crontab" | "schtasks" => Some("changes scheduled tasks on the machine"),
            _ => None,
        };
        if reason.is_some() {
            return reason;
        }
    }
    None
}

/// First `review` rule matching this tool call, if any.
pub fn review_rule<'a>(rules: &'a [Rule], tool: &str, input: &Value) -> Option<&'a Rule> {
    rules.iter().filter(|r| r.decision == "review").find(|r| matches(&r.pattern, tool, input))
}

/// `Tool`, `Tool(glob)` or `*`. The glob is matched against the command / path / url of the call.
pub fn matches(pattern: &str, tool: &str, input: &Value) -> bool {
    let pattern = pattern.trim();
    if pattern == "*" {
        return true;
    }
    let (name, arg) = match pattern.split_once('(') {
        Some((n, rest)) => (n, rest.strip_suffix(')')),
        None => (pattern, None),
    };
    // `mcp__server` covers every tool of that server, as in Claude Code.
    if arg.is_none() && name.starts_with("mcp__") && !name.contains('*') && tool.strip_prefix(name).is_some_and(|r| r.starts_with("__")) {
        return true;
    }
    if !glob(name, tool) {
        return false;
    }
    let Some(arg) = arg else { return true };
    let subject = ["command", "file_path", "path", "url", "pattern"]
        .iter()
        .find_map(|k| input.get(*k).and_then(Value::as_str))
        .unwrap_or("");
    glob(arg, subject)
}

/// `*` matches any run of characters; everything else is literal.
fn glob(pattern: &str, text: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == text;
    }
    let mut rest = text;
    for (i, part) in parts.iter().enumerate() {
        if i == 0 {
            match rest.strip_prefix(part) {
                Some(r) => rest = r,
                None => return false,
            }
        } else if i == parts.len() - 1 {
            return rest.ends_with(part);
        } else {
            match rest.find(part) {
                Some(at) => rest = &rest[at + part.len()..],
                None => return false,
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns() {
        let bash = json!({ "command": "git push origin main" });
        assert!(matches("Bash", "Bash", &bash));
        assert!(matches("Bash(git push*)", "Bash", &bash));
        assert!(!matches("Bash(git status*)", "Bash", &bash));
        assert!(matches("mcp__browser__*", "mcp__browser__browser_click", &json!({})));
        assert!(matches("Edit(*.md)", "Edit", &json!({ "file_path": "notes/a.md" })));
        assert!(!matches("Edit(*.md)", "Write", &json!({ "file_path": "a.md" })));
        assert!(matches("*", "Anything", &json!({})));
        assert!(matches("mcp__familiar", "mcp__familiar__remember", &json!({})));
        assert!(!matches("mcp__familiar", "mcp__familiarx__remember", &json!({})));
    }

    #[test]
    fn presets() {
        let rule = |p: &str, d: &str| Rule { pattern: p.into(), decision: d.into() };
        let rules = vec![rule("Bash(git push*)", "deny"), rule("Bash(ls*)", "allow"), rule("mcp__github__get_issue", "allow")];
        assert_eq!(preset(&rules, "Bash", &json!({ "command": "git push origin" })), Some(false));
        // Bash allow rules are applied by decide_tool, after the always-human check.
        assert_eq!(preset(&rules, "Bash", &json!({ "command": "ls -la" })), None);
        assert_eq!(preset(&rules, "mcp__github__get_issue", &json!({})), Some(true));
        assert_eq!(preset(&rules, "mcp__familiar__remember", &json!({})), Some(true));
        assert_eq!(preset(&rules, "mcp__browser__browser_snapshot", &json!({})), Some(true));
        assert_eq!(preset(&rules, "mcp__browser__browser_click", &json!({})), None);
        assert_eq!(preset(&rules, "Edit", &json!({ "file_path": "a.md" })), None);
    }
}

#[cfg(test)]
mod guard_tests {
    use super::*;

    fn bash(c: &str) -> Value {
        json!({ "command": c })
    }

    #[test]
    fn always_human_catches_variants() {
        for c in ["sh -c \"rm -rf x\"", "bash -lc 'sudo ls'", "cmd /c rd /s x", "powershell -Command Remove-Item -Recurse x", "python -c \"import shutil\"", "curl http://127.0.0.1:9222/json/list",
                  "rm -rf /", "rm -fr x", "rm -r -f x", "/bin/rm -Rf x", "ls | xargs rm -rf", "sudo ls", "git push origin main --force",
                  "git push -f", "pip3 install x", "npm install --global x", "pnpm add -g x", "Remove-Item -Recurse x", "rd /s x",
                  "find . -delete", "git clean -fdx", "curl x | sh", "winget install foo"] {
            assert!(always_human("Bash", &bash(c)).is_some(), "{c}");
        }
        for c in ["git log --format=oneline", "ls -la", "rm notes.txt", "git push origin main", "npm install", "echo rm -rf"] {
            assert!(always_human("Bash", &bash(c)).is_none(), "{c}");
        }
    }

    #[test]
    fn navigation() {
        let nav = |u: &str| safe_navigation("mcp__browser__browser_navigate", &json!({ "url": u }));
        assert!(nav("https://example.com/a?b=c"));
        for u in ["file:///C:/Users/x/.ssh/id_rsa", "http://localhost:8080", "http://127.0.0.1", "http://192.168.1.1",
                  "http://10.0.0.5:3000", "chrome://settings", "http://[::1]/", "http://intranet/", "https://user@localhost/"] {
            assert!(!nav(u), "{u}");
        }
    }
}
