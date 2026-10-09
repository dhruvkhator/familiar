//! Rules → Claude Code permission settings, plus matching for `review` rules the daemon handles itself.

use serde_json::{Value, json};

use crate::db::Rule;

/// Built-in tools a proactive (research-only) run may use. Enforced with `--tools`, not by the callback,
/// because read-only tools never reach the permission prompt.
pub const RESEARCH_TOOLS: &[&str] = &["Read", "Glob", "Grep", "WebSearch", "WebFetch"];

/// Built-in tools for normal runs. Anything that writes or executes still needs approval.
pub const FULL_TOOLS: &[&str] = &["Bash", "Read", "Write", "Edit", "Glob", "Grep", "WebFetch", "WebSearch", "TodoWrite", "Skill"];

/// Rules → Claude Code `permissions` settings.
/// Owner `allow` rules are *not* forwarded: every such call reaches the daemon, which re-reads the rules on each
/// decision (a rule the owner adds or removes counts at once, even in a running session), checks deny rules and
/// [`always_human`] first, and only then applies the owner's allow rules ([`owner_allows`]). `review` rules go in as
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
            "allow" => {}
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
/// `Some(false)` = an owner `deny` rule; `Some(true)` = pre-allowed (Familiar tools, look-only browser tools);
/// `None` = goes through [`crate::runner::decide_tool`] (which also applies the owner's allow rules).
pub fn preset(rules: &[Rule], tool: &str, input: &Value) -> Option<bool> {
    if owner_denies(rules, tool, input).is_some() {
        return Some(false);
    }
    FAMILIAR_ALLOWED.iter().any(|p| matches(p, tool, input)).then_some(true)
}

/// The first owner `deny` rule matching this call.
pub fn owner_denies<'a>(rules: &'a [Rule], tool: &str, input: &Value) -> Option<&'a Rule> {
    rules.iter().filter(|r| r.decision == "deny").find(|r| matches(&r.pattern, tool, input))
}

/// An owner `allow` rule the daemon applies itself (see [`settings`]). A rule for some commands (`Bash(git status *)`)
/// never covers a compound one (`git status; curl … | sh`): only `Bash` or `*` do.
pub fn owner_allows<'a>(rules: &'a [Rule], tool: &str, input: &Value) -> Option<&'a Rule> {
    let compound = tool == "Bash" && input["command"].as_str().is_some_and(compound);
    rules
        .iter()
        .filter(|r| r.decision == "allow")
        .filter(|r| !compound || !is_bash(&r.pattern) || !r.pattern.contains('('))
        .find(|r| matches(&r.pattern, tool, input))
}

/// Shell control operators, substitutions or redirections: more than one plain command.
fn compound(command: &str) -> bool {
    command.contains(['\n', '\r', ';', '&', '|', '<', '>', '`']) || command.contains("$(")
}

/// The input field the owner may rewrite before approving ("Edit & approve"): a command, the file a `Write` creates,
/// the text a browser will type, a connector's message. None when there is nothing sensible to edit.
pub fn editable_field(tool: &str, input: &Value) -> Option<&'static str> {
    let field = match tool {
        "Bash" => "command",
        "Write" => "content",
        "mcp__browser__browser_type" => "text",
        // Desktop steps are approved exactly as shown.
        t if t.starts_with("mcp__desktop__") => return None,
        t if t.starts_with("mcp__") && !t.starts_with("mcp__browser__") && !t.starts_with("mcp__familiar__") => {
            return ["body", "text", "message", "content", "comment"].into_iter().find(|k| input[*k].is_string());
        }
        _ => return None,
    };
    input[field].is_string().then_some(field)
}

/// Action words of a connector tool that only read (`get_issue`, `listRepos`, `search_code`). Anything else, and any
/// tool whose name also carries a write word, gets no "Always allow". `fetch` and `query` are left out on purpose: a
/// fetch can carry data out in its URL, and a SQL "query" tool may write.
const READ_ONLY_ACTIONS: &[&str] = &["get", "list", "search", "read", "find", "describe", "view"];
const WRITE_WORDS: &[&str] = &[
    "send", "post", "create", "update", "delete", "merge", "push", "write", "put", "set", "add", "remove", "edit",
    "run", "exec", "execute", "publish", "reply", "comment", "upload", "move", "rename", "patch", "insert", "invite",
    "approve", "close", "archive", "draft", "submit", "share", "transfer", "pay", "buy", "order", "modify", "save",
];

/// The rule "Always allow this" adds for this call, or None when it must not be offered. Offered only for connector
/// tools that clearly only read ([`READ_ONLY_ACTIONS`]): the exact tool name, for that teammate. Never for shell
/// commands (what a command touches can't be judged safely ahead of time: links, option files, recursion, Windows path
/// forms), file tools (a `Write` rule would also cover gate scripts, `.claude/`, `.mcp.json`, `.git/`), any browser
/// tool (a click or a keystroke can post or send as the owner), connector tools that write or send, Familiar's own
/// tools, questions, drafts, or anything that always needs the owner. Owners can still write their own rules knowingly.
pub fn always_allow_rule(tool: &str, input: &Value) -> Option<String> {
    if always_human(tool, input).is_some() || !tool.is_ascii() || tool.contains(['(', ')', '*', ' ']) {
        return None;
    }
    let server_tool = tool.strip_prefix("mcp__")?;
    let (server, action) = server_tool.split_once("__")?;
    // Never a desktop tool: each one is a look at, or a touch of, the owner's own screen.
    if server.is_empty() || matches!(server, "browser" | "familiar" | crate::desktop::SERVER) {
        return None;
    }
    read_only_action(action).then(|| tool.to_owned())
}

/// `get_issue`, `listRepos`, `search-code`: a read-only verb first and no write word anywhere.
fn read_only_action(action: &str) -> bool {
    // Split snake, kebab and camel case into lowercase words.
    let mut words: Vec<String> = Vec::new();
    let mut cur = String::new();
    for c in action.chars() {
        if c == '_' || c == '-' || c == '.' {
            if !cur.is_empty() {
                words.push(std::mem::take(&mut cur));
            }
        } else if c.is_ascii_uppercase() && !cur.is_empty() {
            words.push(std::mem::take(&mut cur));
            cur.push(c.to_ascii_lowercase());
        } else {
            cur.push(c.to_ascii_lowercase());
        }
    }
    if !cur.is_empty() {
        words.push(cur);
    }
    words.first().is_some_and(|w| READ_ONLY_ACTIONS.contains(&w.as_str()))
        && !words.iter().any(|w| WRITE_WORDS.contains(&w.as_str()))
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
/// private networks, odd schemes) goes to the owner. Async because a public-looking name can still resolve to a
/// local address (`localtest.me` → 127.0.0.1, `*.nip.io`), so every resolved address must be public too.
pub async fn safe_navigation(tool: &str, input: &Value) -> bool {
    let Some(host) = navigation_host(tool, input) else { return false };
    if parse_ipv4(&host).is_some() || host.starts_with('[') {
        return true; // a literal address already checked by navigation_host
    }
    let lookup = tokio::net::lookup_host((host.as_str(), 443));
    match tokio::time::timeout(std::time::Duration::from_secs(3), lookup).await {
        Ok(Ok(addrs)) => {
            let addrs: Vec<_> = addrs.collect();
            !addrs.is_empty() && addrs.iter().all(|a| is_public(a.ip()))
        }
        _ => false,
    }
}

/// The host of an http(s) navigation that passes the syntactic checks, or None. IPv4 is parsed the way browsers
/// do (WHATWG: `127.1`, `0x7f.0.0.1`, `2130706433`, `0177.0.0.1` all mean 127.0.0.1).
fn navigation_host(tool: &str, input: &Value) -> Option<String> {
    if tool != "mcp__browser__browser_navigate" {
        return None;
    }
    let lower = input["url"].as_str()?.trim().to_ascii_lowercase();
    let rest = lower.strip_prefix("https://").or_else(|| lower.strip_prefix("http://"))?;
    let authority = rest.split(['/', '?', '#', '\\']).next().unwrap_or_default();
    let host_port = authority.rsplit('@').next().unwrap_or_default();
    // Browsers percent-decode hosts (`%31%32%37.0.0.1`); there is no legitimate reason for one here.
    if host_port.contains('%') {
        return None;
    }
    if let Some(v6) = host_port.strip_prefix('[') {
        let ip: std::net::Ipv6Addr = v6.split(']').next()?.parse().ok()?;
        return is_public(std::net::IpAddr::V6(ip)).then(|| format!("[{ip}]"));
    }
    let host = host_port.split(':').next().unwrap_or_default().trim_end_matches('.');
    if host.is_empty() || host == "localhost" || host.ends_with(".localhost") || host.ends_with(".local") || host.ends_with(".internal") {
        return None;
    }
    let numeric = host.split('.').all(|p| !p.is_empty() && (p.starts_with("0x") || p.bytes().all(|b| b.is_ascii_digit())));
    if numeric {
        // Looks like an address in some notation: it must parse, and be public.
        let ip = parse_ipv4(host)?;
        return is_public(std::net::IpAddr::V4(ip)).then(|| host.to_owned());
    }
    // A bare single-label name ("intranet") is a LAN host.
    host.contains('.').then(|| host.to_owned())
}

/// WHATWG IPv4 parsing: 1–4 parts, each decimal, `0x` hex or leading-zero octal; the last part fills the rest.
fn parse_ipv4(host: &str) -> Option<std::net::Ipv4Addr> {
    let parts: Vec<&str> = host.split('.').collect();
    if parts.is_empty() || parts.len() > 4 {
        return None;
    }
    let mut nums = Vec::with_capacity(parts.len());
    for p in &parts {
        let n = if let Some(hex) = p.strip_prefix("0x") {
            if hex.is_empty() { 0 } else { u64::from_str_radix(hex, 16).ok()? }
        } else if p.len() > 1 && p.starts_with('0') {
            u64::from_str_radix(&p[1..], 8).ok()?
        } else {
            p.parse::<u64>().ok()?
        };
        nums.push(n);
    }
    let (last, init) = nums.split_last()?;
    if init.iter().any(|&n| n > 255) || *last >= 256u64.pow(5 - nums.len() as u32) {
        return None;
    }
    let mut value = *last;
    for (i, n) in init.iter().enumerate() {
        value += n << (8 * (3 - i));
    }
    Some(std::net::Ipv4Addr::from(u32::try_from(value).ok()?))
}

/// A globally routable address: not loopback, private, carrier-grade NAT, link-local (cloud metadata), unspecified,
/// broadcast, multicast, documentation, benchmarking or reserved. IPv4 carried inside IPv6 (mapped, compatible, NAT64,
/// 6to4) is judged as that IPv4 address; Teredo tunnels are refused. Also the CRM webhooks' address rule.
pub(crate) fn is_public(ip: std::net::IpAddr) -> bool {
    use std::net::{IpAddr, Ipv4Addr};
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, c, _] = v4.octets();
            !(v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_documentation()
                || a == 0
                || (a == 100 && (64..128).contains(&b)) // carrier-grade NAT
                || (a == 192 && b == 0 && c == 0) // IETF protocol assignments
                || (a == 198 && (b == 18 || b == 19)) // benchmarking
                || a >= 224) // multicast + reserved
        }
        IpAddr::V6(v6) => {
            if v6.is_loopback() || v6.is_unspecified() {
                return false;
            }
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_public(IpAddr::V4(v4));
            }
            let s = v6.segments();
            let v4 = |hi: u16, lo: u16| IpAddr::V4(Ipv4Addr::from((u32::from(hi) << 16) | u32::from(lo)));
            // IPv4-compatible (::a.b.c.d), NAT64 (64:ff9b::a.b.c.d) and 6to4 (2002:aabb:ccdd::) reach that IPv4 address
            if s[..6] == [0; 6] || (s[0] == 0x64 && s[1] == 0xff9b && s[2..6] == [0; 4]) {
                return is_public(v4(s[6], s[7]));
            }
            if s[0] == 0x2002 {
                return is_public(v4(s[1], s[2]));
            }
            !((s[0] & 0xfe00) == 0xfc00 // unique local
                || (s[0] & 0xffc0) == 0xfe80 // link-local
                || (s[0] & 0xffc0) == 0xfec0 // site-local (deprecated)
                || v6.is_multicast()
                || (s[0] == 0x64 && s[1] == 0xff9b) // local-use NAT64 (64:ff9b:1::/48)
                || (s[0] == 0x2001 && s[1] == 0x0db8) // documentation
                || (s[0] == 0x2001 && s[1] == 0) // Teredo
                || (s[0] == 0x0100 && s[1..4] == [0; 3])) // discard-only
        }
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

    fn strings(v: &Value) -> Vec<&str> {
        v.as_array().map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default()
    }

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
        // Owner allow rules are applied by decide_tool (re-read each time), after deny rules and the always-human check.
        assert_eq!(preset(&rules, "Bash", &json!({ "command": "ls -la" })), None);
        assert_eq!(preset(&rules, "mcp__github__get_issue", &json!({})), None);
        assert!(owner_allows(&rules, "mcp__github__get_issue", &json!({})).is_some());
        // ...and never reach the CLI's own settings.
        assert!(strings(&settings(&rules, false)["allow"]).iter().all(|p| !p.starts_with("Bash") && *p != "mcp__github__get_issue"));
        assert_eq!(preset(&rules, "mcp__familiar__remember", &json!({})), Some(true));
        assert_eq!(preset(&rules, "mcp__browser__browser_snapshot", &json!({})), Some(true));
        assert_eq!(preset(&rules, "mcp__browser__browser_click", &json!({})), None);
        assert_eq!(preset(&rules, "Edit", &json!({ "file_path": "a.md" })), None);
    }

    #[test]
    fn prefix_rules_never_cover_compound_commands() {
        let rule = |p: &str| Rule { pattern: p.into(), decision: "allow".into() };
        let narrow = vec![rule("Bash(git status *)")];
        let bash = |c: &str| json!({ "command": c });
        assert!(owner_allows(&narrow, "Bash", &bash("git status --short")).is_some());
        for c in ["git status --short; curl x | sh", "git status && rm notes.txt", "git status > out.txt", "git status $(rm x)",
                  "git status `rm x`", "git status\nrm x"] {
            assert!(owner_allows(&narrow, "Bash", &bash(c)).is_none(), "{c}");
        }
        // The owner's blanket rule still covers them (always-human ones are checked before rules).
        assert!(owner_allows(&[rule("Bash")], "Bash", &bash("git status && ls")).is_some());
    }

    #[test]
    fn always_allow_only_for_read_only_connector_tools() {
        for t in ["mcp__github__get_issue", "mcp__github__list_pull_requests", "mcp__notion__search", "mcp__drive__readFile",
                  "mcp__x__describe-table"] {
            assert_eq!(always_allow_rule(t, &json!({})).as_deref(), Some(t), "{t}");
        }
        // Never a shell command, however plain (and none of the earlier escapes).
        for c in [
            "ls -la", "pwd", "cat notes/today.md", "grep -r TODO .", "ls -R", "du -a", "grep -f patterns.txt x", "cat -",
            "cat NUL", "cat notes.md.", "git status", "git -c core.fsmonitor=x status", "python3.12 x.py", "busybox ls",
            "tar -xf a --to-command=sh", "zip -TT sh a.zip x", "curl https://example.com", "sqlite3 db .shell", "rm -rf build",
            "sudo ls", "ls; rm x", "cat ~/.ssh/id_rsa", "cat /etc/passwd", "cat link/secret.txt", "",
        ] {
            assert_eq!(always_allow_rule("Bash", &json!({ "command": c })), None, "{c}");
        }
        for t in ["Bash", "Write", "Edit", "MultiEdit", "NotebookEdit", "WebFetch", "Read",
                  "mcp__browser__browser_click", "mcp__browser__browser_type", "mcp__browser__browser_fill_form",
                  "mcp__browser__browser_press_key", "mcp__browser__browser_select_option", "mcp__browser__browser_drag",
                  "mcp__browser__browser_hover", "mcp__browser__browser_navigate", "mcp__browser__browser_evaluate",
                  "mcp__browser__browser_snapshot", "mcp__gmail__send_email", "mcp__slack__post_message",
                  "mcp__slack__slack_post_message", "mcp__github__create_issue", "mcp__github__merge_pull_request",
                  "mcp__github__push_files", "mcp__github__get_and_delete", "mcp__fetch__fetch", "mcp__postgres__query",
                  "mcp__x__update_get", "mcp__familiar__propose_draft", "mcp__familiar__remember", "mcp__familiar__get_x",
                  "propose_draft", "ask_user", "Bash(ls)", "mcp__github__getIssue(x)", "mcp____get_x", "mcp__github"] {
            assert_eq!(always_allow_rule(t, &json!({ "command": "ls", "file_path": ".claude/settings.json" })), None, "{t}");
        }
    }

    #[test]
    fn editable_fields() {
        assert_eq!(editable_field("Bash", &json!({ "command": "ls" })), Some("command"));
        assert_eq!(editable_field("Write", &json!({ "file_path": "a", "content": "x" })), Some("content"));
        assert_eq!(editable_field("mcp__browser__browser_type", &json!({ "ref": "e1", "text": "hi" })), Some("text"));
        assert_eq!(editable_field("mcp__slack__post_message", &json!({ "channel": "c", "text": "hi" })), Some("text"));
        assert_eq!(editable_field("mcp__gmail__send", &json!({ "to": "a", "body": "hi", "text": "x" })), Some("body"));
        assert_eq!(editable_field("mcp__browser__browser_click", &json!({ "ref": "e1" })), None);
        assert_eq!(editable_field("mcp__browser__browser_navigate", &json!({ "url": "https://a.b" })), None);
        assert_eq!(editable_field("Edit", &json!({ "file_path": "a", "old_string": "x", "new_string": "y" })), None);
        assert_eq!(editable_field("Bash", &json!({ "command": 3 })), None);
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
        // Syntactic pass only (no DNS): public-looking names and public literals get through, everything local doesn't.
        let nav = |u: &str| navigation_host("mcp__browser__browser_navigate", &json!({ "url": u })).is_some();
        assert!(nav("https://example.com/a?b=c"));
        assert!(nav("http://93.184.215.14/"));
        for u in ["file:///C:/Users/x/.ssh/id_rsa", "http://localhost:8080", "http://127.0.0.1", "http://192.168.1.1",
                  "http://10.0.0.5:3000", "chrome://settings", "http://[::1]/", "http://intranet/", "https://user@localhost/",
                  // Browser IPv4 spellings of loopback / private (WHATWG parsing):
                  "http://127.1/", "http://0x7f.0.0.1/", "http://0x7f000001/", "http://2130706433/", "http://0177.0.0.1/",
                  "http://017700000001/", "http://10.1/", "http://0xa000005/", "http://localhost./", "http://%31%32%37.0.0.1/",
                  "http://[::ffff:127.0.0.1]/", "http://[fd00::1]/", "http://169.254.169.254/latest/meta-data", "http://0.0.0.0/",
                  "http://100.64.0.1/", "http://127.0.0.1\\@example.com/", "http://example.com@127.0.0.1/"] {
            assert!(!nav(u), "{u}");
        }
    }

    #[test]
    fn ipv4_like_browsers() {
        let ip = |h: &str| parse_ipv4(h).map(|a| a.to_string());
        assert_eq!(ip("127.1").as_deref(), Some("127.0.0.1"));
        assert_eq!(ip("0x7f.1").as_deref(), Some("127.0.0.1"));
        assert_eq!(ip("2130706433").as_deref(), Some("127.0.0.1"));
        assert_eq!(ip("0177.0.0.01").as_deref(), Some("127.0.0.1"));
        assert_eq!(ip("1.2.3.4").as_deref(), Some("1.2.3.4"));
        assert_eq!(ip("256.1.1.1"), None);
        assert_eq!(ip("1.2.3.4.5"), None);
    }

    #[test]
    fn public_addresses() {
        let public = |s: &str| is_public(s.parse().unwrap());
        for a in ["8.8.8.8", "93.184.215.14", "2606:4700:4700::1111", "::ffff:8.8.8.8", "64:ff9b::808:808", "2002:808:808::1"] {
            assert!(public(a), "{a}");
        }
        for a in [
            "127.0.0.1", "10.0.0.1", "172.16.0.1", "192.168.1.1", "100.64.0.1", "169.254.169.254", "0.0.0.0", "0.1.2.3",
            "255.255.255.255", "224.0.0.1", "240.0.0.1", "192.0.2.1", "192.0.0.8", "198.18.0.1", "::", "::1", "fd00::1",
            "fe80::1", "fec0::1", "ff02::1", "::ffff:127.0.0.1", "::ffff:169.254.169.254", "::127.0.0.1",
            "64:ff9b::a9fe:a9fe", "64:ff9b:1::1", "2002:a9fe:a9fe::1", "2002:7f00:1::", "2001:db8::1", "2001:0:1::1",
            "100::1",
        ] {
            assert!(!public(a), "{a}");
        }
    }

    #[tokio::test]
    async fn dns_names_must_resolve_public() {
        // `localhost` never resolves to a public address, whatever the name looks like.
        assert!(!safe_navigation("mcp__browser__browser_navigate", &json!({ "url": "http://localhost.example.invalid/" })).await);
    }
}
