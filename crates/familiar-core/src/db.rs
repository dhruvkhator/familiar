//! Postgres access. The daemon connects as `postgres` and bypasses RLS, so every query is scoped by owner.

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::{FromRow, PgPool, types::Json};
use uuid::Uuid;

#[derive(Debug, Clone, FromRow)]
pub struct Bot {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    pub persona: Option<String>,
    pub model: String,
    /// `claude` (Claude Code CLI) or `codex` (OpenAI Codex CLI).
    pub engine: String,
}

#[derive(Debug, Clone, FromRow)]
pub struct Thread {
    pub id: Uuid,
    pub claude_session_id: Option<Uuid>,
    /// Codex's own thread id (a string) when the bot runs on the Codex engine.
    pub codex_thread_id: Option<String>,
}

#[derive(Debug, Clone, FromRow)]
pub struct Run {
    pub id: Uuid,
    pub bot_id: Uuid,
    pub thread_id: Uuid,
    pub kind: String,
    pub prompt: String,
}

impl Run {
    /// Research-only runs (proactive, nightly dream): they can look things up but never act.
    pub fn research(&self) -> bool {
        matches!(self.kind.as_str(), "proactive" | "dream")
    }
}

#[derive(Debug, Clone, FromRow)]
pub struct Rule {
    pub pattern: String,
    pub decision: String,
}

#[derive(Debug, Clone, FromRow)]
pub struct Message {
    pub role: String,
    pub content: String,
}

#[derive(Clone)]
pub struct Db {
    pub pool: PgPool,
    pub owner: Uuid,
}

impl Db {
    pub async fn fail_stale_runs(&self) -> Result<u64> {
        let mut tx = self.pool.begin().await?;
        let stale: Vec<Uuid> = sqlx::query_scalar(
            "update runs set status = 'failed', error = 'daemon restarted', finished_at = now()
             where owner_id = $1 and status in ('running', 'waiting_approval') returning id",
        )
        .bind(self.owner)
        .fetch_all(&mut *tx)
        .await?;
        sqlx::query(
            "update approvals set status = 'expired', decided_by = 'rule', decided_at = now()
             where owner_id = $1 and status = 'pending' and run_id = any($2)",
        )
        .bind(self.owner)
        .bind(&stale)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(stale.len() as u64)
    }

    pub async fn heartbeat(&self, device: Uuid, name: &str) -> Result<()> {
        sqlx::query(
            "insert into devices (id, owner_id, name, version, last_seen_at) values ($1, $2, $3, $4, now())
             on conflict (id) do update set name = excluded.name, version = excluded.version, last_seen_at = now()",
        )
        .bind(device)
        .bind(self.owner)
        .bind(name)
        .bind(env!("CARGO_PKG_VERSION"))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Queued runs that may start now: oldest in their thread, bot not paused and not busy.
    pub async fn eligible_runs(&self, limit: i64) -> Result<Vec<Run>> {
        Ok(sqlx::query_as(
            "select r.id, r.bot_id, r.thread_id, r.kind, r.prompt from runs r
             join bots b on b.id = r.bot_id
             where r.owner_id = $1 and r.status = 'queued' and not b.paused
               and not exists (select 1 from runs x where x.bot_id = r.bot_id
                               and x.status in ('running', 'waiting_approval'))
               and not exists (select 1 from runs y where y.thread_id = r.thread_id
                               and y.status = 'queued' and y.created_at < r.created_at)
             order by r.created_at limit $2",
        )
        .bind(self.owner)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?)
    }

    /// Atomically move a run from queued to running. False if someone else got it (or it was cancelled).
    pub async fn claim_run(&self, run: Uuid) -> Result<bool> {
        let r = sqlx::query(
            "update runs set status = 'running', started_at = now()
             where id = $1 and owner_id = $2 and status = 'queued'",
        )
        .bind(run)
        .bind(self.owner)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected() == 1)
    }

    pub async fn bot(&self, id: Uuid) -> Result<Bot> {
        Ok(sqlx::query_as("select id, slug, name, persona, model, engine from bots where id = $1 and owner_id = $2")
            .bind(id)
            .bind(self.owner)
            .fetch_one(&self.pool)
            .await?)
    }

    pub async fn thread(&self, id: Uuid) -> Result<Thread> {
        Ok(sqlx::query_as("select id, claude_session_id, codex_thread_id from threads where id = $1 and owner_id = $2")
            .bind(id)
            .bind(self.owner)
            .fetch_one(&self.pool)
            .await?)
    }

    pub async fn set_session(&self, thread: Uuid, session: Option<Uuid>) -> Result<()> {
        sqlx::query("update threads set claude_session_id = $1 where id = $2 and owner_id = $3")
            .bind(session)
            .bind(thread)
            .bind(self.owner)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn set_codex_thread(&self, thread: Uuid, codex_thread: Option<&str>) -> Result<()> {
        sqlx::query("update threads set codex_thread_id = $1 where id = $2 and owner_id = $3")
            .bind(codex_thread)
            .bind(thread)
            .bind(self.owner)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn memories(&self, bot: Uuid) -> Result<Vec<String>> {
        Ok(sqlx::query_scalar(
            "select content from memories where bot_id = $1 and owner_id = $2 and status = 'active' order by created_at",
        )
        .bind(bot)
        .bind(self.owner)
        .fetch_all(&self.pool)
        .await?)
    }

    /// Rules for a bot, bot-specific first so they win over global ones.
    pub async fn rules(&self, bot: Uuid) -> Result<Vec<Rule>> {
        Ok(sqlx::query_as(
            "select pattern, decision from rules where owner_id = $1 and (bot_id = $2 or bot_id is null)
             order by (bot_id is null), created_at",
        )
        .bind(self.owner)
        .bind(bot)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn recent_messages(&self, thread: Uuid, limit: i64) -> Result<Vec<Message>> {
        let mut rows: Vec<Message> = sqlx::query_as(
            "select role, content from messages where thread_id = $1 and owner_id = $2
             order by created_at desc limit $3",
        )
        .bind(thread)
        .bind(self.owner)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.reverse();
        Ok(rows)
    }

    pub async fn insert_event(&self, run: Uuid, seq: i32, kind: &str, payload: &Value) -> Result<()> {
        sqlx::query("insert into events (run_id, owner_id, seq, kind, payload) values ($1, $2, $3, $4, $5)")
            .bind(run)
            .bind(self.owner)
            .bind(seq)
            .bind(kind)
            .bind(Json(payload))
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn run_status(&self, run: Uuid) -> Result<Option<String>> {
        Ok(sqlx::query_scalar("select status from runs where id = $1 and owner_id = $2")
            .bind(run)
            .bind(self.owner)
            .fetch_optional(&self.pool)
            .await?)
    }

    /// Only moves between running and waiting_approval; never overwrites a cancel.
    pub async fn set_run_waiting(&self, run: Uuid, waiting: bool) -> Result<()> {
        let (from, to) = if waiting { ("running", "waiting_approval") } else { ("waiting_approval", "running") };
        // Parallel tool calls can have several approvals open: stay waiting until the last one is decided.
        sqlx::query(
            "update runs set status = $1 where id = $2 and owner_id = $3 and status = $4
               and ($1 = 'waiting_approval' or not exists
                    (select 1 from approvals a where a.run_id = runs.id and a.status = 'pending'))",
        )
            .bind(to)
            .bind(run)
            .bind(self.owner)
            .bind(from)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn finish_run(
        &self,
        run: Uuid,
        status: &str,
        error: Option<&str>,
        cost: Option<f64>,
        usage: Option<&Value>,
    ) -> Result<()> {
        // A user cancel wins over whatever the process reported.
        sqlx::query(
            "update runs set status = case when status = 'cancelled' then status else $1 end,
                    error = $2, cost_usd = $3, usage = $4, finished_at = now()
             where id = $5 and owner_id = $6",
        )
        .bind(status)
        .bind(error)
        .bind(cost)
        .bind(usage.map(Json))
        .bind(run)
        .bind(self.owner)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn insert_message(&self, thread: Uuid, role: &str, content: &str, run: Uuid) -> Result<()> {
        sqlx::query(
            "insert into messages (thread_id, owner_id, role, content, run_id) values ($1, $2, $3, $4, $5)",
        )
        .bind(thread)
        .bind(self.owner)
        .bind(role)
        .bind(content)
        .bind(run)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// A pending approval. `editable` / `allow_rule`: what the owner may do besides approve and deny (see
    /// [`crate::runner::Offer`]).
    #[allow(clippy::too_many_arguments)]
    pub async fn create_approval(
        &self,
        run: &Run,
        tool_use_id: Option<&str>,
        tool_name: &str,
        input: &Value,
        reason: Option<&str>,
        editable: &[String],
        allow_rule: Option<&str>,
        timeout: std::time::Duration,
    ) -> Result<Uuid> {
        Ok(sqlx::query_scalar(
            "insert into approvals (run_id, bot_id, owner_id, tool_use_id, tool_name, input, reason, status, editable,
                                    allow_rule, expires_at)
             values ($1, $2, $3, $4, $5, $6, $7, 'pending', $8, $9, now() + make_interval(secs => $10)) returning id",
        )
        .bind(run.id)
        .bind(run.bot_id)
        .bind(self.owner)
        .bind(tool_use_id)
        .bind(tool_name)
        .bind(Json(input))
        .bind(reason)
        .bind(editable)
        .bind(allow_rule)
        .bind(timeout.as_secs_f64())
        .fetch_one(&self.pool)
        .await?)
    }

    /// (status, the owner's answer or note, the input as the owner edited it).
    pub async fn approval_status(&self, id: Uuid) -> Result<(String, Option<String>, Option<Value>)> {
        let (status, response, edited): (String, Option<String>, Option<Json<Value>>) =
            sqlx::query_as("select status, response, edited_input from approvals where id = $1 and owner_id = $2")
                .bind(id)
                .bind(self.owner)
                .fetch_one(&self.pool)
                .await?;
        Ok((status, response, edited.map(|j| j.0)))
    }

    /// Resolve a still-pending approval (expiry, or the run ending while it waited).
    pub async fn close_approval(&self, id: Uuid, status: &str, by: &str) -> Result<()> {
        sqlx::query(
            "update approvals set status = $1, decided_by = $2, decided_at = now()
             where id = $3 and owner_id = $4 and status = 'pending'",
        )
        .bind(status)
        .bind(by)
        .bind(id)
        .bind(self.owner)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Enabled schedules that the SQL cron job consumed (next_run_at null) and need their next fire time.
    pub async fn schedules_without_next(&self) -> Result<Vec<(Uuid, String)>> {
        Ok(sqlx::query_as(
            "select id, cron from schedules where owner_id = $1 and enabled and next_run_at is null",
        )
        .bind(self.owner)
        .fetch_all(&self.pool)
        .await?)
    }

    /// Turn due schedules into queued runs (the SQL side of scheduling; fire times are computed in Rust).
    pub async fn enqueue_due_schedules(&self) -> Result<i32> {
        Ok(sqlx::query_scalar("select public.familiar_enqueue_due_schedules()").fetch_one(&self.pool).await?)
    }

    /// Keep the database small: drop activity older than 30 days.
    pub async fn prune_events(&self) -> Result<u64> {
        let r = sqlx::query("delete from events where owner_id = $1 and created_at < now() - interval '30 days'")
            .bind(self.owner)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected())
    }

    pub async fn set_next_run(&self, schedule: Uuid, at: Option<DateTime<Utc>>) -> Result<()> {
        sqlx::query("update schedules set next_run_at = $1 where id = $2 and owner_id = $3")
            .bind(at)
            .bind(schedule)
            .bind(self.owner)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // ---- bot tools (Familiar MCP server) ----

    /// None when the same memory already exists (in any status, so rejected ones are not re-proposed).
    pub async fn insert_memory(&self, bot: Uuid, content: &str, source: &str, status: &str) -> Result<Option<Uuid>> {
        Ok(sqlx::query_scalar(
            "insert into memories (bot_id, owner_id, content, source, status)
             select $1, $2, $3, $4, $5
             where not exists (select 1 from memories where bot_id = $1 and lower(trim(content)) = lower(trim($3)))
             returning id",
        )
        .bind(bot)
        .bind(self.owner)
        .bind(content)
        .bind(source)
        .bind(status)
        .fetch_optional(&self.pool)
        .await?)
    }

    /// Memories not yet active (proposed, rejected): dreams must not propose them again.
    pub async fn memories_not_active(&self, bot: Uuid) -> Result<Vec<(String, String)>> {
        Ok(sqlx::query_as(
            "select content, status from memories where bot_id = $1 and owner_id = $2 and status <> 'active' order by created_at desc limit 100",
        )
        .bind(bot)
        .bind(self.owner)
        .fetch_all(&self.pool)
        .await?)
    }

    /// Bots that worked since their last dream and haven't dreamed today (local `since`), with their Dreams thread.
    pub async fn bots_due_for_dream(&self, since: DateTime<Utc>) -> Result<Vec<Uuid>> {
        Ok(sqlx::query_scalar(
            "select b.id from bots b where b.owner_id = $1 and not b.paused
               and coalesce(b.last_dreamed_at, b.created_at) < $2
               and exists (select 1 from runs r where r.bot_id = b.id and r.kind <> 'dream'
                           and r.created_at > coalesce(b.last_dreamed_at, '-infinity'))
               and not exists (select 1 from runs r where r.bot_id = b.id and r.kind = 'dream'
                               and r.status in ('queued', 'running'))",
        )
        .bind(self.owner)
        .bind(since)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn queue_dream(&self, bot: Uuid) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        let thread: Option<Uuid> = sqlx::query_scalar(
            "select id from threads where bot_id = $1 and owner_id = $2 and title = 'Dreams' and source = 'schedule' limit 1",
        )
        .bind(bot)
        .bind(self.owner)
        .fetch_optional(&mut *tx)
        .await?;
        let thread = match thread {
            Some(t) => t,
            None => sqlx::query_scalar(
                "insert into threads (bot_id, owner_id, title, source) values ($1, $2, 'Dreams', 'schedule') returning id",
            )
            .bind(bot)
            .bind(self.owner)
            .fetch_one(&mut *tx)
            .await?,
        };
        sqlx::query("insert into runs (bot_id, owner_id, thread_id, kind, prompt) values ($1, $2, $3, 'dream', '(dream)')")
            .bind(bot)
            .bind(self.owner)
            .bind(thread)
            .execute(&mut *tx)
            .await?;
        // Mark now so a failing dream doesn't requeue every tick.
        sqlx::query("update bots set last_dreamed_at = now() where id = $1 and owner_id = $2")
            .bind(bot)
            .bind(self.owner)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    /// What the bot did and discussed since `since` (for its dream), newest last, capped.
    pub async fn activity_since(&self, bot: Uuid, since: Option<DateTime<Utc>>) -> Result<String> {
        let rows: Vec<(String, String, String)> = sqlx::query_as(
            "select t.title, m.role, m.content from messages m join threads t on t.id = m.thread_id
             where t.bot_id = $1 and m.owner_id = $2 and t.title <> 'Dreams'
               and m.created_at > coalesce($3, now() - interval '7 days')
             order by m.created_at desc limit 200",
        )
        .bind(bot)
        .bind(self.owner)
        .bind(since)
        .fetch_all(&self.pool)
        .await?;
        let mut out = String::new();
        for (thread, role, content) in rows.into_iter().rev() {
            let line: String = content.chars().take(1500).collect();
            out.push_str(&format!("[{thread}] {role}: {line}\n"));
        }
        let start = out.len().saturating_sub(40_000);
        Ok(out[out.ceil_char_boundary(start)..].to_owned())
    }

    pub async fn last_dreamed(&self, bot: Uuid) -> Result<Option<DateTime<Utc>>> {
        Ok(sqlx::query_scalar("select last_dreamed_at from bots where id = $1 and owner_id = $2")
            .bind(bot)
            .bind(self.owner)
            .fetch_one(&self.pool)
            .await?)
    }

    pub async fn insert_schedule(&self, bot: Uuid, cron: &str, prompt: &str, kind: &str) -> Result<Uuid> {
        Ok(sqlx::query_scalar(
            "insert into schedules (bot_id, owner_id, cron, prompt, kind) values ($1, $2, $3, $4, $5) returning id",
        )
        .bind(bot)
        .bind(self.owner)
        .bind(cron)
        .bind(prompt)
        .bind(kind)
        .fetch_one(&self.pool)
        .await?)
    }

    pub async fn list_schedules(&self, bot: Uuid) -> Result<Value> {
        Ok(sqlx::query_scalar(
            "select coalesce(jsonb_agg(jsonb_build_object('id', id, 'cron', cron, 'prompt', prompt, 'kind', kind,
                     'enabled', enabled, 'next_run_at', next_run_at) order by cron), '[]'::jsonb)
             from schedules where bot_id = $1 and owner_id = $2",
        )
        .bind(bot)
        .bind(self.owner)
        .fetch_one(&self.pool)
        .await?)
    }

    pub async fn delete_schedule(&self, bot: Uuid, id: Uuid) -> Result<bool> {
        let r = sqlx::query("delete from schedules where id = $1 and bot_id = $2 and owner_id = $3")
            .bind(id)
            .bind(bot)
            .bind(self.owner)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected() == 1)
    }

    pub async fn list_bots(&self) -> Result<Value> {
        Ok(sqlx::query_scalar(
            "select coalesce(jsonb_agg(jsonb_build_object('slug', slug, 'name', name, 'persona', left(persona, 200),
                     'paused', paused) order by name), '[]'::jsonb)
             from bots where owner_id = $1",
        )
        .bind(self.owner)
        .fetch_one(&self.pool)
        .await?)
    }

    pub async fn bot_by_slug(&self, slug: &str) -> Result<Option<Bot>> {
        Ok(sqlx::query_as("select id, slug, name, persona, model, engine from bots where slug = $1 and owner_id = $2")
            .bind(slug)
            .bind(self.owner)
            .fetch_optional(&self.pool)
            .await?)
    }

    /// New thread + queued run on another bot. Returns the run id.
    pub async fn handoff(&self, to: Uuid, title: &str, prompt: &str, parent: Uuid) -> Result<Uuid> {
        let mut tx = self.pool.begin().await?;
        let thread: Uuid = sqlx::query_scalar(
            "insert into threads (bot_id, owner_id, title, source) values ($1, $2, $3, 'handoff') returning id",
        )
        .bind(to)
        .bind(self.owner)
        .bind(title)
        .fetch_one(&mut *tx)
        .await?;
        let run: Uuid = sqlx::query_scalar(
            "insert into runs (bot_id, owner_id, thread_id, kind, prompt, parent_run_id)
             values ($1, $2, $3, 'handoff', $4, $5) returning id",
        )
        .bind(to)
        .bind(self.owner)
        .bind(thread)
        .bind(prompt)
        .bind(parent)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(run)
    }

    /// (status, error, final assistant message) of a run.
    pub async fn run_outcome(&self, run: Uuid) -> Result<(String, Option<String>, Option<String>)> {
        Ok(sqlx::query_as(
            "select r.status, r.error,
                    (select content from messages m where m.run_id = r.id and m.role = 'assistant'
                     order by created_at desc limit 1)
             from runs r where r.id = $1 and r.owner_id = $2",
        )
        .bind(run)
        .bind(self.owner)
        .fetch_one(&self.pool)
        .await?)
    }

    pub async fn insert_artifact(
        &self,
        run: Uuid,
        bot: Uuid,
        name: &str,
        mime: &str,
        bytes: i64,
        stored: Stored<'_>,
    ) -> Result<Uuid> {
        let (storage, data, key) = match stored {
            Stored::Db(d) => ("db", Some(d), None),
            Stored::S3(k) => ("s3", None, Some(k)),
        };
        Ok(sqlx::query_scalar(
            "insert into artifacts (run_id, bot_id, owner_id, name, mime, bytes, storage, data, r2_key)
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9) returning id",
        )
        .bind(run)
        .bind(bot)
        .bind(self.owner)
        .bind(name)
        .bind(mime)
        .bind(bytes)
        .bind(storage)
        .bind(data)
        .bind(key)
        .fetch_one(&self.pool)
        .await?)
    }

    /// Mirror the workspace skills folder: upsert these (name, description, body), delete the bot's other rows.
    pub async fn sync_skills(&self, bot: Uuid, skills: &[(String, String, String)]) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        let names: Vec<&str> = skills.iter().map(|s| s.0.as_str()).collect();
        sqlx::query("delete from skills where bot_id = $1 and owner_id = $2 and not (name = any($3))")
            .bind(bot)
            .bind(self.owner)
            .bind(&names)
            .execute(&mut *tx)
            .await?;
        for (name, description, body) in skills {
            sqlx::query(
                "insert into skills (bot_id, owner_id, name, description, body, updated_at)
                 values ($1, $2, $3, $4, $5, now())
                 on conflict (bot_id, name) do update set description = excluded.description, body = excluded.body,
                   updated_at = now()
                 where skills.description is distinct from excluded.description or skills.body <> excluded.body",
            )
            .bind(bot)
            .bind(self.owner)
            .bind(name)
            .bind(description)
            .bind(body)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Enabled connectors linked to a bot.
    pub async fn bot_connectors(&self, bot: Uuid) -> Result<Vec<Connector>> {
        Ok(sqlx::query_as(
            "select c.name, c.transport, c.command, c.args, c.url, c.secrets_enc
             from connectors c join bot_connectors bc on bc.connector_id = c.id
             where bc.bot_id = $1 and c.owner_id = $2 and c.enabled order by c.name",
        )
        .bind(bot)
        .bind(self.owner)
        .fetch_all(&self.pool)
        .await?)
    }

    /// Gate command of the schedule that owns this thread, if any.
    pub async fn schedule_gate(&self, thread: Uuid) -> Result<Option<String>> {
        Ok(sqlx::query_scalar(
            "select s.gate_command from threads t join schedules s on s.id = t.schedule_id
             where t.id = $1 and t.owner_id = $2 and nullif(trim(s.gate_command), '') is not null",
        )
        .bind(thread)
        .bind(self.owner)
        .fetch_optional(&self.pool)
        .await?
        .flatten())
    }

    /// Ephemeral token stream for the UI (no table write). Payload must stay under the 8 KB NOTIFY limit.
    pub async fn notify_delta(&self, run: Uuid, kind: &str, text: &str) -> Result<()> {
        let payload = serde_json::json!({ "owner": self.owner, "run": run, "kind": kind, "text": text });
        sqlx::query("select pg_notify('familiar_delta', $1)").bind(payload.to_string()).execute(&self.pool).await?;
        Ok(())
    }

    pub async fn set_device_info(&self, device: Uuid, info: &Value) -> Result<()> {
        sqlx::query("update devices set info = $1 where id = $2 and owner_id = $3")
            .bind(Json(info))
            .bind(device)
            .bind(self.owner)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

pub enum Stored<'a> {
    Db(&'a [u8]),
    S3(&'a str),
}

#[derive(Debug, Clone, FromRow)]
pub struct Connector {
    pub name: String,
    pub transport: String,
    pub command: Option<String>,
    pub args: Json<Vec<String>>,
    pub url: Option<String>,
    pub secrets_enc: Option<String>,
}
