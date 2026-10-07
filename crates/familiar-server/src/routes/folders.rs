//! Folders on this PC shared with a teammate (see `familiar_core::folders`). The API checks a folder when it is added
//! (resolved, never a drive, the home folder, a hidden settings folder, app data, Familiar's data, a workspace, a system
//! folder or a network folder); the daemon checks it again at the start of every run.

use axum::{Json, extract::State, http::StatusCode};
use familiar_core::folders;
use serde::Deserialize;

use super::{Body, Id, Row, found, one_of, owned};
use crate::{
    S,
    auth::Auth,
    error::{ApiError, R},
};

const MODES: [&str; 2] = ["read", "write"];

pub async fn list(State(st): State<S>, a: Auth, Id(bot): Id) -> R<Json<Vec<Row>>> {
    owned(&st.pool, "bots", bot, a.user).await?;
    let rows = sqlx::query_scalar(
        "select to_jsonb(f) - 'owner_id' from bot_folders f where f.bot_id = $1 and f.owner_id = $2 order by f.created_at",
    )
    .bind(bot)
    .bind(a.user)
    .fetch_all(&st.pool)
    .await?;
    Ok(Json(rows))
}

#[derive(Deserialize)]
pub struct NewFolder {
    path: String,
    /// read (default) | write
    mode: Option<String>,
}

pub async fn create(State(st): State<S>, a: Auth, Id(bot): Id, Body(b): Body<NewFolder>) -> R<(StatusCode, Json<Row>)> {
    owned(&st.pool, "bots", bot, a.user).await?;
    let mode = one_of(b.mode.as_deref().unwrap_or("read"), "mode", &MODES)?;
    let Some(bots_dir) = st.bots_dir.clone() else {
        return Err(ApiError::bad("Folders can only be shared from the Familiar app on the PC that has them."));
    };
    let raw = b.path;
    let path = tokio::task::spawn_blocking(move || folders::check(&raw, &folders::Env::current(&bots_dir)))
        .await
        .map_err(|_| ApiError::Internal)?
        .map_err(ApiError::bad)?;
    let count: i64 = sqlx::query_scalar("select count(*) from bot_folders where bot_id = $1 and owner_id = $2")
        .bind(bot)
        .bind(a.user)
        .fetch_one(&st.pool)
        .await?;
    if count >= folders::MAX_FOLDERS as i64 {
        return Err(ApiError::bad(format!("A teammate can have at most {} folders.", folders::MAX_FOLDERS)));
    }
    let row: Row = sqlx::query_scalar(
        "insert into bot_folders (owner_id, bot_id, path, mode) values ($1, $2, $3, $4)
         returning to_jsonb(bot_folders) - 'owner_id'",
    )
    .bind(a.user)
    .bind(bot)
    .bind(path.display().to_string())
    .bind(&mode)
    .fetch_one(&st.pool)
    .await
    .map_err(|e| match ApiError::from(e) {
        ApiError::Conflict(_) => ApiError::conflict("This folder is already shared with this teammate."),
        other => other,
    })?;
    Ok((StatusCode::CREATED, Json(row)))
}

#[derive(Deserialize)]
pub struct FolderPatch {
    mode: String,
}

pub async fn update(State(st): State<S>, a: Auth, Id(id): Id, Body(p): Body<FolderPatch>) -> R<Json<Row>> {
    let mode = one_of(&p.mode, "mode", &MODES)?;
    let row = sqlx::query_scalar(
        "update bot_folders set mode = $3 where id = $1 and owner_id = $2 returning to_jsonb(bot_folders) - 'owner_id'",
    )
    .bind(id)
    .bind(a.user)
    .bind(mode)
    .fetch_optional(&st.pool)
    .await?;
    found(row)
}

pub async fn remove(State(st): State<S>, a: Auth, Id(id): Id) -> R<StatusCode> {
    let n = sqlx::query("delete from bot_folders where id = $1 and owner_id = $2")
        .bind(id)
        .bind(a.user)
        .execute(&st.pool)
        .await?
        .rows_affected();
    if n == 0 { Err(ApiError::NotFound) } else { Ok(StatusCode::NO_CONTENT) }
}
