use axum::{
    Json,
    extract::State,
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use rusty_s3::{Bucket, Credentials, S3Action, UrlStyle};
use std::time::Duration;
use uuid::Uuid;

use super::{Id, Row, owned};
use crate::{
    S,
    auth::Auth,
    error::{ApiError, R},
};

/// S3-compatible store (R2, MinIO, AWS) used only to presign downloads.
pub struct S3 {
    bucket: Bucket,
    creds: Credentials,
}

impl S3 {
    pub fn from_env() -> Option<Self> {
        let var = |k: &str| {
            std::env::var(k)
                .ok()
                .or_else(|| k.strip_prefix("FAMILIAR_").and_then(|s| std::env::var(format!("ZED_{s}")).ok()))
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        let endpoint = var("FAMILIAR_S3_ENDPOINT")?.parse().ok()?;
        let bucket = Bucket::new(
            endpoint,
            UrlStyle::Path,
            var("FAMILIAR_S3_BUCKET")?,
            var("FAMILIAR_S3_REGION").unwrap_or_else(|| "auto".into()),
        )
        .ok()?;
        let creds = Credentials::new(
            var("FAMILIAR_S3_ACCESS_KEY_ID")?,
            var("FAMILIAR_S3_SECRET_ACCESS_KEY")?,
        );
        Some(Self { bucket, creds })
    }

    fn presign_get(&self, key: &str) -> String {
        self.bucket
            .get_object(Some(&self.creds), key)
            .sign(Duration::from_secs(600))
            .to_string()
    }
}

/// Artifact metadata; the `data` bytes and storage key are never listed.
const META: &str = "to_jsonb(a) - 'owner_id' - 'data' - 'r2_key'";

pub async fn for_run(State(st): State<S>, a: Auth, Id(id): Id) -> R<Json<Vec<Row>>> {
    owned(&st.pool, "runs", id, a.user).await?;
    list_where(&st, &a, "run_id", id).await
}

pub async fn for_bot(State(st): State<S>, a: Auth, Id(id): Id) -> R<Json<Vec<Row>>> {
    owned(&st.pool, "bots", id, a.user).await?;
    list_where(&st, &a, "bot_id", id).await
}

async fn list_where(st: &S, a: &Auth, col: &str, id: Uuid) -> R<Json<Vec<Row>>> {
    let rows = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "select {META} from artifacts a where a.{col} = $1 and a.owner_id = $2 order by a.created_at desc limit 200"
    )))
    .bind(id)
    .bind(a.user)
    .fetch_all(&st.pool)
    .await?;
    Ok(Json(rows))
}

#[allow(clippy::type_complexity)]
pub async fn download(State(st): State<S>, a: Auth, Id(id): Id) -> R<Response> {
    let row: Option<(
        String,
        String,
        Option<String>,
        Option<Vec<u8>>,
        Option<String>,
    )> = sqlx::query_as(
        "select name, storage, mime, data, r2_key from artifacts where id = $1 and owner_id = $2",
    )
    .bind(id)
    .bind(a.user)
    .fetch_optional(&st.pool)
    .await?;
    let (name, storage, mime, data, key) = row.ok_or(ApiError::NotFound)?;
    if storage == "s3" {
        let s3 = st
            .s3
            .as_ref()
            .ok_or_else(|| ApiError::Unavailable("S3 storage not configured".into()))?;
        let url = s3.presign_get(&key.ok_or(ApiError::NotFound)?);
        let loc = HeaderValue::from_str(&url).map_err(|_| ApiError::Internal)?;
        return Ok((StatusCode::FOUND, [(header::LOCATION, loc)]).into_response());
    }
    let data = data.ok_or(ApiError::NotFound)?;
    let ascii: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_graphic() && c != '"' && c != '\\' || c == ' ' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let mime = mime
        .filter(|m| HeaderValue::from_str(m).is_ok())
        .unwrap_or_else(|| "application/octet-stream".into());
    let headers = [
        (header::CONTENT_TYPE, mime),
        (
            header::CONTENT_DISPOSITION,
            format!("inline; filename=\"{ascii}\""),
        ),
        // artifacts are bot-produced content served from the API origin: never let them run scripts
        (header::CONTENT_SECURITY_POLICY, "sandbox".into()),
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff".into()),
    ];
    Ok((headers, data).into_response())
}
