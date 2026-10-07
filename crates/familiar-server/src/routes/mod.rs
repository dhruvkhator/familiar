//! Shared helpers for route modules. Rows are built in SQL with `to_jsonb`, so timestamps come out
//! as RFC3339 strings, uuids as strings and jsonb as JSON; `owner_id` is always stripped.

use axum::extract::{FromRequest, FromRequestParts};
use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::{ApiError, R};

pub mod approvals;
pub mod artifacts;
pub mod bots;
pub mod channels;
pub mod connectors;
pub mod folders;
pub mod live;
pub mod memories;
pub mod models;
pub mod overview;
pub mod rules;
pub mod runs;
pub mod schedules;
pub mod skills;
pub mod stream;
pub mod templates;
pub mod threads;
pub mod triggers;

/// A JSON row produced by `to_jsonb(..)`.
pub type Row = sqlx::types::Json<Value>;

#[derive(FromRequest)]
#[from_request(via(axum::Json), rejection(ApiError))]
pub struct Body<T>(pub T);

#[derive(FromRequestParts)]
#[from_request(via(axum::extract::Query), rejection(ApiError))]
pub struct Q<T>(pub T);

/// Trim, require non-empty, cap length in characters.
pub fn text(v: &str, field: &str, max: usize) -> R<String> {
    let t = v.trim();
    if t.is_empty() {
        return Err(ApiError::bad(format!("{field} must not be empty")));
    }
    if t.chars().count() > max {
        return Err(ApiError::bad(format!("{field} too long (max {max})")));
    }
    Ok(t.to_string())
}

pub fn one_of(v: &str, field: &str, allowed: &[&str]) -> R<String> {
    if allowed.contains(&v) {
        Ok(v.to_string())
    } else {
        Err(ApiError::bad(format!(
            "{field} must be one of: {}",
            allowed.join(", ")
        )))
    }
}

pub fn clamp(limit: Option<i64>, default: i64, max: i64) -> i64 {
    limit.unwrap_or(default).clamp(1, max)
}

pub fn parse_ts(s: &str) -> R<DateTime<Utc>> {
    // a '+' offset arrives as ' ' when the client forgot to URL-encode it
    DateTime::parse_from_rfc3339(&s.replace(' ', "+"))
        .map(|d| d.with_timezone(&Utc))
        .map_err(|_| ApiError::bad("`before` must be an RFC3339 timestamp"))
}

/// 404 unless `table` has a row with this id owned by `owner`. `table` is always a literal.
pub async fn owned(pool: &PgPool, table: &'static str, id: Uuid, owner: Uuid) -> R<()> {
    let ok: bool = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "select exists(select 1 from {table} where id = $1 and owner_id = $2)"
    )))
    .bind(id)
    .bind(owner)
    .fetch_one(pool)
    .await?;
    if ok { Ok(()) } else { Err(ApiError::NotFound) }
}

pub fn found(row: Option<Row>) -> R<axum::Json<Row>> {
    row.map(axum::Json).ok_or(ApiError::NotFound)
}
pub struct Id(pub Uuid);

impl<S: Send + Sync> FromRequestParts<S> for Id {
    type Rejection = ApiError;
    async fn from_request_parts(parts: &mut axum::http::request::Parts, st: &S) -> R<Self> {
        let axum::extract::Path(id) =
            axum::extract::Path::<Uuid>::from_request_parts(parts, st).await?;
        Ok(Id(id))
    }
}
