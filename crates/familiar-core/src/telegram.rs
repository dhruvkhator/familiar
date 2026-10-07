//! Telegram channel: talk to your bots and approve their actions from your phone.
//! Long-polls the Bot API from this machine, so no public URL or webhook is needed. Only the paired chat is served.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use sqlx::FromRow;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use uuid::Uuid;

use crate::daemon::{Ctx, Signal};
use crate::text::reveal;

#[derive(Debug, Clone, FromRow)]
struct Channel {
    id: Uuid,
    config_enc: String,
    chat_id: Option<String>,
    pair_code: Option<String>,
    update_offset: i64,
}

/// Supervises the poller: (re)starts it whenever the channel config changes.
pub async fn run(ctx: Ctx, shutdown: CancellationToken) {
    let mut notices = ctx.notices.subscribe();
    let mut current: Option<(String, CancellationToken)> = None; // (config fingerprint, poller token)
    loop {
        match load(&ctx).await {
            Ok(Some((ch, token))) => {
                // Pairing updates `self.ch` in place; only a new channel or token restarts the poller.
                let fingerprint = format!("{}|{token}", ch.id);
                if current.as_ref().map(|c| &c.0) != Some(&fingerprint) {
                    if let Some((_, old)) = current.take() {
                        old.cancel();
                    }
                    let stop = shutdown.child_token();
                    let bot = Telegram::new(ctx.clone(), ch, token);
                    tokio::spawn(bot.serve(stop.clone()));
                    current = Some((fingerprint, stop));
                    info!("telegram channel active");
                }
            }
            Ok(None) => {
                if let Some((_, old)) = current.take() {
                    old.cancel();
                    info!("telegram channel stopped");
                }
            }
            Err(e) => warn!("telegram config: {e:#}"),
        }
        // Re-check when channels change, or every 5 minutes.
        let deadline = tokio::time::sleep(Duration::from_secs(300));
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                n = notices.recv() => match n {
                    Ok(n) if n.t == "channels" || n.t == "*" => break,
                    Ok(_) => continue,
                    Err(_) => break,
                },
                _ = &mut deadline => break,
                _ = shutdown.cancelled() => return,
            }
        }
    }
}

async fn load(ctx: &Ctx) -> Result<Option<(Channel, String)>> {
    let ch: Option<Channel> = sqlx::query_as(
        "select id, config_enc, chat_id, pair_code, update_offset from channels
         where owner_id = $1 and kind = 'telegram' and enabled",
    )
    .bind(ctx.db.owner)
    .fetch_optional(&ctx.db.pool)
    .await?;
    let Some(ch) = ch else { return Ok(None) };
    let Some(secrets) = ctx.secrets.as_ref() else {
        bail!("a Telegram channel is configured but the daemon has no `secret_key`");
    };
    let cfg: Value = serde_json::from_str(&secrets.decrypt(&ch.config_enc)?)?;
    let token = cfg["token"].as_str().context("telegram config has no token")?.to_owned();
    Ok(Some((ch, token)))
}

struct Telegram {
    ctx: Ctx,
    ch: Channel,
    api: String,
    http: reqwest::Client,
    /// approval id → (chat message id) so we can edit it once decided.
    approval_msgs: HashMap<Uuid, i64>,
    /// telegram message id of an ask_user question → approval id, answered by replying to it.
    questions: HashMap<i64, Uuid>,
}

impl Telegram {
    fn new(ctx: Ctx, ch: Channel, token: String) -> Self {
        Telegram {
            ctx,
            ch,
            api: format!("https://api.telegram.org/bot{token}"),
            http: reqwest::Client::new(),
            approval_msgs: HashMap::new(),
            questions: HashMap::new(),
        }
    }

    async fn call(&self, method: &str, body: Value) -> Result<Value> {
        let resp: Value = self
            .http
            .post(format!("{}/{method}", self.api))
            .json(&body)
            .timeout(Duration::from_secs(70))
            .send()
            .await
            .map_err(reqwest::Error::without_url)?
            .json()
            .await
            .map_err(reqwest::Error::without_url)?;
        if resp["ok"] != true {
            bail!("telegram {method}: {}", resp["description"].as_str().unwrap_or("error"));
        }
        Ok(resp["result"].clone())
    }

    async fn send(&self, text: &str, extra: Value) -> Option<i64> {
        let chat = self.ch.chat_id.as_deref()?;
        let mut body = json!({ "chat_id": chat, "text": clip(text, 4000), "disable_web_page_preview": true });
        if let (Value::Object(b), Value::Object(e)) = (&mut body, extra) {
            b.extend(e);
        }
        match self.call("sendMessage", body).await {
            Ok(m) => m["message_id"].as_i64(),
            Err(e) => {
                warn!("telegram send failed: {e:#}");
                None
            }
        }
    }

    async fn serve(mut self, stop: CancellationToken) {
        let mut notices = self.ctx.notices.subscribe();
        let mut signals = self.ctx.subscribe_signals();
        let mut offset: i64 = self.ch.update_offset;
        let (updates_tx, mut updates) = tokio::sync::mpsc::channel::<Value>(64);
        // Long polling in its own task so outbound pushes are never stuck behind a 50 s poll.
        {
            let (api, http, stop) = (self.api.clone(), self.http.clone(), stop.clone());
            let (db, channel) = (self.ctx.db.clone(), self.ch.id);
            tokio::spawn(async move {
                loop {
                    let req = http
                        .post(format!("{api}/getUpdates"))
                        .json(&json!({ "offset": offset, "timeout": 50, "allowed_updates": ["message", "callback_query"] }))
                        .timeout(Duration::from_secs(70))
                        .send();
                    let resp = tokio::select! { r = req => r, _ = stop.cancelled() => return };
                    let body: Value = match resp {
                        Ok(r) => r.json().await.unwrap_or_default(),
                        Err(e) => {
                            warn!("telegram poll failed: {}", e.without_url()); // the URL contains the bot token
                            tokio::time::sleep(Duration::from_secs(5)).await;
                            continue;
                        }
                    };
                    if body["ok"] != true {
                        warn!("telegram: {}", body["description"].as_str().unwrap_or("getUpdates failed"));
                        tokio::time::sleep(Duration::from_secs(30)).await;
                        continue;
                    }
                    for u in body["result"].as_array().into_iter().flatten() {
                        offset = offset.max(u["update_id"].as_i64().unwrap_or(0) + 1);
                        if updates_tx.send(u.clone()).await.is_err() {
                            return;
                        }
                    }
                    // Persist the position so a restart never replays handled updates.
                    let _ = sqlx::query("update channels set update_offset = $1 where id = $2 and owner_id = $3")
                        .bind(offset)
                        .bind(channel)
                        .bind(db.owner)
                        .execute(&db.pool)
                        .await;
                }
            });
        }
        loop {
            tokio::select! {
                Some(u) = updates.recv() => {
                    if let Err(e) = self.on_update(&u).await {
                        warn!("telegram update: {e:#}");
                        if u.get("message").is_some() {
                            self.send("Sorry, I could not handle that message. Check the Familiar app.", json!({})).await;
                        }
                    }
                }
                n = notices.recv() => if let Ok(n) = n {
                    let r = match n.t.as_str() {
                        "approvals" => self.on_approval(n.id).await,
                        "messages" => self.on_message(n.id).await,
                        "runs" => self.on_run(n.id).await,
                        _ => Ok(()),
                    };
                    if let Err(e) = r {
                        warn!("telegram push: {e:#}");
                    }
                },
                s = signals.recv() => if let Ok(Signal::Notify { bot, message, thread }) = s {
                    // Telegram threads already get every assistant message through on_message.
                    if self.thread_source(thread).await.as_deref() != Some("telegram") {
                        self.send(&format!("🔔 {bot}: {message}"), json!({})).await;
                    }
                },
                _ = stop.cancelled() => return,
            }
        }
    }

    async fn on_update(&mut self, u: &Value) -> Result<()> {
        if let Some(cb) = u.get("callback_query") {
            return self.on_callback(cb).await;
        }
        let Some(msg) = u.get("message") else { return Ok(()) };
        let chat = msg["chat"]["id"].as_i64().map(|c| c.to_string()).unwrap_or_default();
        let text = msg["text"].as_str().unwrap_or_default().trim().to_owned();

        if self.ch.chat_id.is_none() {
            // Pairing: only `/start <code>` from any chat, once.
            let code = text.strip_prefix("/start").map(str::trim).unwrap_or_default();
            // Private chats only: pairing a group would let every member drive the bots.
            let private = msg["chat"]["type"] == "private";
            if private && !code.is_empty() && Some(code) == self.ch.pair_code.as_deref() {
                sqlx::query("update channels set chat_id = $1, pair_code = null where id = $2 and owner_id = $3")
                    .bind(&chat)
                    .bind(self.ch.id)
                    .bind(self.ctx.db.owner)
                    .execute(&self.ctx.db.pool)
                    .await?;
                self.ch.chat_id = Some(chat);
                self.send("Paired with Familiar. Message me to talk to your bot; /bots lists them, /bot <slug> <message> picks one.", json!({})).await;
            }
            return Ok(());
        }
        if self.ch.chat_id.as_deref() != Some(chat.as_str()) || text.is_empty() {
            return Ok(()); // strangers and stickers
        }

        // A reply to an ask_user question answers it.
        if let Some(reply_to) = msg["reply_to_message"]["message_id"].as_i64() {
            if let Some(approval) = self.questions.remove(&reply_to) {
                let sent = self.decide(approval, true, Some(&text)).await?;
                self.send(if sent { "Answer sent." } else { "That question was already answered or expired." }, json!({})).await;
                return Ok(());
            }
        }
        if text == "/bots" || text == "/start" || text == "/help" {
            let bots: Vec<(String, String)> = sqlx::query_as("select slug, name from bots where owner_id = $1 order by name")
                .bind(self.ctx.db.owner)
                .fetch_all(&self.ctx.db.pool)
                .await?;
            let list = bots.iter().map(|(s, n)| format!("• {n} — /bot {s} …")).collect::<Vec<_>>().join("\n");
            self.send(&format!("Your bots:\n{list}\n\nPlain messages go to your default bot."), json!({})).await;
            return Ok(());
        }
        let (bot, body) = match text.strip_prefix("/bot ") {
            Some(rest) => {
                let (slug, body) = rest.split_once(' ').unwrap_or((rest, ""));
                let id: Option<Uuid> = sqlx::query_scalar("select id from bots where owner_id = $1 and slug = $2")
                    .bind(self.ctx.db.owner)
                    .bind(slug.trim())
                    .fetch_optional(&self.ctx.db.pool)
                    .await?;
                match id {
                    Some(id) if !body.trim().is_empty() => (id, body.trim().to_owned()),
                    Some(_) => {
                        self.send("Usage: /bot <slug> <message>", json!({})).await;
                        return Ok(());
                    }
                    None => {
                        self.send("No bot with that slug. /bots lists them.", json!({})).await;
                        return Ok(());
                    }
                }
            }
            None => match self.default_bot().await? {
                Some(id) => (id, text),
                None => {
                    self.send("No default bot set. Pick one in Familiar → Integrations, or use /bot <slug> <message>.", json!({})).await;
                    return Ok(());
                }
            },
        };
        let thread = self.telegram_thread(bot).await?;
        // The messages trigger queues the run.
        sqlx::query("insert into messages (thread_id, owner_id, role, content) values ($1, $2, 'user', $3)")
            .bind(thread)
            .bind(self.ctx.db.owner)
            .bind(&body)
            .execute(&self.ctx.db.pool)
            .await?;
        self.call("sendChatAction", json!({ "chat_id": self.ch.chat_id, "action": "typing" })).await.ok();
        Ok(())
    }

    async fn on_callback(&mut self, cb: &Value) -> Result<()> {
        // In a private chat the chat id is the user id: both the chat and the person pressing must be the owner.
        let owner = self.ch.chat_id.as_deref();
        let from_owner = cb["message"]["chat"]["id"].as_i64().map(|c| c.to_string()).as_deref() == owner
            && cb["from"]["id"].as_i64().map(|c| c.to_string()).as_deref() == owner;
        let data = cb["data"].as_str().unwrap_or_default();
        let answer = |text: &str| json!({ "callback_query_id": cb["id"], "text": text });
        if !from_owner {
            self.call("answerCallbackQuery", answer("Not yours.")).await.ok();
            return Ok(());
        }
        let Some((action, id)) = data.split_once(':') else { return Ok(()) };
        let Ok(id) = id.parse::<Uuid>() else { return Ok(()) };
        // Approve only what this chat could show whole (the button is only offered then; a stale or forged one is not).
        if action == "y" && !self.approvable(id).await? {
            self.call("answerCallbackQuery", answer("Review it in Familiar.")).await.ok();
            return Ok(());
        }
        let decided = self.decide(id, action == "y", None).await?;
        let text = if !decided { "Already decided." } else if action == "y" { "Approved" } else { "Denied" };
        self.call("answerCallbackQuery", answer(text)).await.ok();
        Ok(())
    }

    /// Can this approval be approved from Telegram (see [`approval_message`])?
    async fn approvable(&self, id: Uuid) -> Result<bool> {
        let row: Option<(String, Value, Option<String>, String)> = sqlx::query_as(
            "select a.tool_name, coalesce(a.input, '{}'::jsonb), a.reason, b.name
             from approvals a join bots b on b.id = a.bot_id where a.id = $1 and a.owner_id = $2",
        )
        .bind(id)
        .bind(self.ctx.db.owner)
        .fetch_optional(&self.ctx.db.pool)
        .await?;
        Ok(row.is_some_and(|(tool, input, reason, bot)| approval_message(&bot, &tool, &input, reason.as_deref()).1))
    }

    /// Returns false if the approval was no longer pending.
    async fn decide(&self, approval: Uuid, approve: bool, response: Option<&str>) -> Result<bool> {
        let r = sqlx::query(
            "update approvals set status = $1, decided_by = 'user', decided_at = now(), response = $2
             where id = $3 and owner_id = $4 and status = 'pending' and coalesce(expires_at > now(), true)",
        )
        .bind(if approve { "approved" } else { "denied" })
        .bind(response)
        .bind(approval)
        .bind(self.ctx.db.owner)
        .execute(&self.ctx.db.pool)
        .await?;
        Ok(r.rows_affected() == 1)
    }

    async fn on_approval(&mut self, id: Uuid) -> Result<()> {
        let row: Option<(String, String, Value, Option<String>, String)> = sqlx::query_as(
            "select a.status, a.tool_name, coalesce(a.input, '{}'::jsonb), a.reason, b.name
             from approvals a join bots b on b.id = a.bot_id where a.id = $1 and a.owner_id = $2",
        )
        .bind(id)
        .bind(self.ctx.db.owner)
        .fetch_optional(&self.ctx.db.pool)
        .await?;
        let Some((status, tool, input, reason, bot)) = row else { return Ok(()) };
        match (status.as_str(), self.approval_msgs.get(&id).copied()) {
            ("pending", None) if tool == "ask_user" => {
                let q = input["question"].as_str().unwrap_or("(no question)");
                let text = format!("❓ {bot} asks:\n{q}\n\nReply to this message to answer.");
                if let Some(mid) = self.send(&text, json!({ "reply_markup": { "force_reply": true } })).await {
                    self.questions.insert(mid, id);
                    self.approval_msgs.insert(id, mid);
                }
            }
            ("pending", None) => {
                // Nothing is approved unseen: without everything shown whole, only Deny is offered here.
                let (text, approvable) = approval_message(&bot, &tool, &input, reason.as_deref());
                let mut row = vec![json!({ "text": "❌ Deny", "callback_data": format!("n:{id}") })];
                if approvable {
                    row.insert(0, json!({ "text": "✅ Approve", "callback_data": format!("y:{id}") }));
                }
                let buttons = json!({ "reply_markup": { "inline_keyboard": [row] } });
                if let Some(mid) = self.send(&text, buttons).await {
                    self.approval_msgs.insert(id, mid);
                }
            }
            (decided, Some(mid)) if decided != "pending" => {
                self.approval_msgs.remove(&id);
                self.questions.retain(|_, a| *a != id);
                let label = if decided == "approved" { "✅ approved".to_owned() } else { format!("❌ {decided}") };
                self.call(
                    "editMessageReplyMarkup",
                    json!({ "chat_id": self.ch.chat_id, "message_id": mid, "reply_markup": { "inline_keyboard": [] } }),
                )
                .await
                .ok();
                self.send(&format!("{bot}: {} {label}", short_tool(&tool)), json!({ "reply_to_message_id": mid })).await;
            }
            _ => {}
        }
        Ok(())
    }

    async fn on_message(&self, id: Uuid) -> Result<()> {
        let row: Option<(String, String, String, String)> = sqlx::query_as(
            "select m.role, m.content, t.source, b.name from messages m
             join threads t on t.id = m.thread_id join bots b on b.id = t.bot_id
             where m.id = $1 and m.owner_id = $2",
        )
        .bind(id)
        .bind(self.ctx.db.owner)
        .fetch_optional(&self.ctx.db.pool)
        .await?;
        if let Some((role, content, source, bot)) = row {
            if role == "assistant" && source == "telegram" {
                self.send(&format!("{bot}: {content}"), json!({})).await;
            }
        }
        Ok(())
    }

    async fn on_run(&self, id: Uuid) -> Result<()> {
        let row: Option<(String, Option<String>, String, String, String)> = sqlx::query_as(
            "select r.status, r.error, r.kind, t.source, b.name from runs r
             join threads t on t.id = r.thread_id join bots b on b.id = r.bot_id
             where r.id = $1 and r.owner_id = $2",
        )
        .bind(id)
        .bind(self.ctx.db.owner)
        .fetch_optional(&self.ctx.db.pool)
        .await?;
        if let Some((status, error, kind, source, bot)) = row {
            // Failures of work the owner isn't watching (telegram chats, schedules, webhooks) are worth a ping.
            if status == "failed" && (source == "telegram" || kind != "chat") {
                self.send(&format!("⚠️ {bot}: {kind} run failed — {}", error.unwrap_or_default()), json!({})).await;
            }
        }
        Ok(())
    }

    async fn default_bot(&self) -> Result<Option<Uuid>> {
        // Re-read: the owner may have changed the default bot in the UI since pairing.
        let current: Option<Option<Uuid>> = sqlx::query_scalar("select default_bot_id from channels where id = $1 and owner_id = $2")
            .bind(self.ch.id)
            .bind(self.ctx.db.owner)
            .fetch_optional(&self.ctx.db.pool)
            .await?;
        if let Some(id) = current.flatten() {
            return Ok(Some(id));
        }
        // Single bot: it's the default.
        let bots: Vec<Uuid> = sqlx::query_scalar("select id from bots where owner_id = $1 limit 2")
            .bind(self.ctx.db.owner)
            .fetch_all(&self.ctx.db.pool)
            .await?;
        Ok((bots.len() == 1).then(|| bots[0]))
    }

    async fn telegram_thread(&self, bot: Uuid) -> Result<Uuid> {
        let existing: Option<Uuid> = sqlx::query_scalar(
            "select id from threads where owner_id = $1 and bot_id = $2 and source = 'telegram'
             order by updated_at desc limit 1",
        )
        .bind(self.ctx.db.owner)
        .bind(bot)
        .fetch_optional(&self.ctx.db.pool)
        .await?;
        if let Some(id) = existing {
            return Ok(id);
        }
        Ok(sqlx::query_scalar(
            "insert into threads (bot_id, owner_id, title, source) values ($1, $2, 'Telegram', 'telegram') returning id",
        )
        .bind(bot)
        .bind(self.ctx.db.owner)
        .fetch_one(&self.ctx.db.pool)
        .await?)
    }

    async fn thread_source(&self, thread: Uuid) -> Option<String> {
        sqlx::query_scalar("select source from threads where id = $1 and owner_id = $2")
            .bind(thread)
            .bind(self.ctx.db.owner)
            .fetch_optional(&self.ctx.db.pool)
            .await
            .ok()
            .flatten()
    }
}

/// The longest approval message that still shows whole (Telegram allows 4096 characters).
const MESSAGE_MAX: usize = 3800;

/// The Telegram message for a pending approval, and whether it may offer Approve. Like the app's approval cards,
/// every field the action would use is shown (a draft's kind, channel, recipient, subject, media and text; every input
/// field of a tool call) with hidden characters written out as `⟨U+202E⟩`, single-line fields on one line. If anything
/// had hidden characters or had to be cut, Approve is not offered: the owner reviews it in the app.
fn approval_message(bot: &str, tool: &str, input: &Value, reason: Option<&str>) -> (String, bool) {
    let mut whole = true;
    let mut show = |s: &str, multiline: bool, max: usize| -> String {
        let (t, hidden) = reveal(s, multiline);
        whole &= !hidden;
        if t.chars().count() > max {
            whole = false;
            clip(&t, max)
        } else {
            t
        }
    };
    let bot = show(bot, false, 100);
    let mut text;
    let mut shown: Vec<&str> = Vec::new();
    if tool == "propose_draft" {
        text = format!("🔐 {bot} wants to send this draft:\n");
        for (label, key) in [("Kind", "kind"), ("Channel", "channel"), ("To", "to"), ("Subject", "subject")] {
            if let Some(v) = input[key].as_str() {
                text.push_str(&format!("{label}: {}\n", show(v, false, 500)));
            }
            shown.push(key);
        }
        if let Some(media) = input["media"].as_array().filter(|m| !m.is_empty()) {
            let names: Vec<String> = media.iter().map(|m| m.as_str().map(str::to_owned).unwrap_or_else(|| m.to_string())).collect();
            text.push_str(&format!("Media: {}\n", show(&names.join(", "), false, 1000)));
        }
        shown.extend(["media", "body"]);
        text.push_str(&format!("Text:\n{}", show(input["body"].as_str().unwrap_or_default(), true, 3000)));
    } else {
        text = format!("🔐 {bot} wants to use {}:", show(short_tool(tool), false, 200));
        if let Some((key, v)) = ["command", "file_path", "url", "path", "element"]
            .into_iter()
            .find_map(|k| input.get(k).and_then(Value::as_str).map(|v| (k, v)))
        {
            text.push_str(&format!("\n{}", show(v, key == "command", 3000)));
            shown.push(key);
        }
    }
    // Every other field, so nothing the action receives is left out.
    match input {
        Value::Object(m) => {
            for (k, v) in m.iter().filter(|(k, _)| !shown.contains(&k.as_str())) {
                let v = v.as_str().map(str::to_owned).unwrap_or_else(|| serde_json::to_string_pretty(v).unwrap_or_default());
                text.push_str(&format!("\n{}: {}", show(k, false, 100), show(&v, true, 1500)));
            }
        }
        Value::Null => {}
        other => text.push_str(&format!("\n{}", show(&other.to_string(), true, 1500))),
    }
    if let Some(r) = reason.map(str::trim).filter(|r| !r.is_empty()) {
        let r = show(r, true, 1000);
        // A draft's note is the teammate's own words.
        text.push_str(&if tool == "propose_draft" { format!("\n\nBot says: “{r}”") } else { format!("\n\n{r}") });
    }
    if text.chars().count() > MESSAGE_MAX {
        whole = false;
        text = clip(&text, MESSAGE_MAX);
    }
    if !whole {
        text.push_str("\n\nNot all of it can be shown safely here: review it in Familiar.");
    }
    (text, whole)
}


fn short_tool(tool: &str) -> &str {
    tool.strip_prefix("mcp__").unwrap_or(tool)
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    s.chars().take(max).collect::<String>() + "…"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drafts_show_every_field_and_approve_only_when_whole() {
        let draft = json!({ "kind": "email", "channel": "Gmail", "to": "sam@example.com", "subject": "Hello",
                            "media": ["media/a.png", "b.pdf"], "body": "Hi Sam,\nquick question." });
        let (text, ok) = approval_message("Outbound", "propose_draft", &draft, Some("follow-up to Tuesday"));
        assert!(ok, "{text}");
        for want in ["Kind: email\n", "Channel: Gmail\n", "To: sam@example.com\n", "Subject: Hello\n", "Media: media/a.png, b.pdf\n",
                     "Text:\nHi Sam,\nquick question.", "Bot says: “follow-up to Tuesday”"] {
            assert!(text.contains(want), "{want:?} in {text}");
        }

        // A newline in a single-line field could fake a "Subject:" line: written out, and no Approve.
        let mut d = draft.clone();
        d["to"] = json!("sam@example.com\nSubject: Invoice");
        let (text, ok) = approval_message("Outbound", "propose_draft", &d, None);
        assert!(!ok && text.contains("To: sam@example.com⟨U+000A⟩Subject: Invoice"), "{text}");
        assert!(text.ends_with("review it in Familiar."));
        // Hidden characters in the body, a bidi override in the bot's name, the note: no Approve.
        for (bot, body, note) in [("Ada", "pay\u{202E}moc.live", None), ("A\u{200B}da", "fine", None), ("Ada", "fine", Some("x\u{2066}"))] {
            let mut d = draft.clone();
            d["body"] = json!(body);
            let (text, ok) = approval_message(bot, "propose_draft", &d, note);
            assert!(!ok && text.contains("⟨U+"), "{text}");
        }
        // Too long to show whole: clipped, no Approve.
        let mut d = draft.clone();
        d["body"] = json!("x".repeat(3500));
        assert!(!approval_message("Ada", "propose_draft", &d, None).1);
        // A field the draft shape doesn't know is shown too.
        let mut d = draft;
        d["bcc"] = json!("attacker@example.com");
        let (text, ok) = approval_message("Ada", "propose_draft", &d, None);
        assert!(ok && text.contains("bcc: attacker@example.com"), "{text}");
    }

    #[test]
    fn tool_calls_show_every_input_field() {
        let (text, ok) = approval_message("Ada", "Bash", &json!({ "command": "ls -la", "description": "look" }), None);
        assert!(ok && text.contains("\nls -la") && text.contains("description: look"), "{text}");
        // The field the old message left out is there now.
        let (text, ok) = approval_message("Ada", "mcp__browser__browser_navigate", &json!({ "url": "https://a.example", "body": "<secrets>" }), None);
        assert!(ok && text.contains("body: <secrets>"), "{text}");
        let (text, ok) = approval_message("Ada", "Bash", &json!({ "command": "echo safe\rrm -rf ~" }), None);
        assert!(!ok && text.contains("⟨U+000D⟩"), "{text}");
        let (_, ok) = approval_message("Ada", "Write", &json!({ "file_path": "a.md", "content": "y".repeat(2000) }), None);
        assert!(!ok, "clipped content is not approvable here");
        let (text, ok) = approval_message("Ada", "Bash", &json!({ "command": "x".repeat(5000) }), None);
        assert!(!ok && text.chars().count() < 4096);
    }
}
