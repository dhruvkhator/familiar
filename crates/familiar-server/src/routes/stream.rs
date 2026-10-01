//! Live updates: one shared `LISTEN familiar` task fans notices out to every SSE client of the matching owner.

use axum::{
    extract::State,
    response::sse::{Event, KeepAlive, Sse},
};
use serde_json::Value;
use sqlx::{PgPool, postgres::PgListener};
use std::{convert::Infallible, time::Duration};
use tokio::sync::broadcast;
use tokio_stream::{Stream, StreamExt, wrappers::BroadcastStream};

use crate::{S, auth::Auth};

#[derive(Clone, Debug)]
pub enum Push {
    /// `owner` is kept for filtering; `data` is the payload with `owner` removed.
    Notice { owner: String, data: String },
    /// Best-effort live token delta from the daemon.
    Delta { owner: String, data: String },
    /// Listener (re)connected: notifications in the gap were lost, clients should refetch.
    Resync,
}

pub fn spawn_listener(pool: PgPool, tx: broadcast::Sender<Push>) {
    tokio::spawn(async move {
        loop {
            if let Err(e) = listen(&pool, &tx).await {
                tracing::warn!("notify listener: {e}; reconnecting");
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    });
}

async fn listen(pool: &PgPool, tx: &broadcast::Sender<Push>) -> sqlx::Result<()> {
    let mut l = PgListener::connect_with(pool).await?;
    l.listen_all(["familiar", "familiar_delta"]).await?;
    let _ = tx.send(Push::Resync);
    loop {
        match l.try_recv().await? {
            Some(n) => {
                let Ok(Value::Object(mut m)) = serde_json::from_str::<Value>(n.payload()) else {
                    continue;
                };
                let Some(Value::String(owner)) = m.remove("owner") else {
                    continue;
                };
                let data = Value::Object(m).to_string();
                let _ = tx.send(if n.channel() == "familiar_delta" {
                    Push::Delta { owner, data }
                } else {
                    Push::Notice { owner, data }
                });
            }
            // sqlx reconnected underneath us
            None => {
                let _ = tx.send(Push::Resync);
            }
        }
    }
}

pub async fn stream(
    State(st): State<S>,
    a: Auth,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let me = a.user.to_string();
    let events = BroadcastStream::new(st.events.subscribe()).filter_map(move |m| match m {
        Ok(Push::Notice { owner, data }) if owner == me => {
            Some(Ok(Event::default().event("notice").data(data)))
        }
        Ok(Push::Delta { owner, data }) if owner == me => {
            Some(Ok(Event::default().event("delta").data(data)))
        }
        Ok(Push::Notice { .. } | Push::Delta { .. }) => None,
        // Resync, or this client lagged and missed notices
        Ok(Push::Resync) | Err(_) => Some(Ok(Event::default().event("resync").data("{}"))),
    });
    Sse::new(events).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(25))
            .text("ping"),
    )
}
