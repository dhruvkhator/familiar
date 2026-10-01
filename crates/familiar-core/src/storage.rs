//! Run artifacts (screenshots, files the bot attaches): Postgres for small files, or any S3-compatible store.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use rusty_s3::{Bucket, Credentials, S3Action, UrlStyle};
use serde_json::json;
use uuid::Uuid;

use crate::daemon::Ctx;
use crate::db::{Run, Stored};
use crate::runner::Events;

const DB_LIMIT: usize = 5 * 1024 * 1024;
const S3_LIMIT: usize = 100 * 1024 * 1024;

pub async fn save(ctx: &Ctx, run: &Run, path: &Path, events: &Events) -> Result<Uuid> {
    let data = tokio::fs::read(path).await.with_context(|| format!("reading {}", path.display()))?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
    let mime = mime_for(&name);
    let id = match &ctx.cfg.s3 {
        Some(s3) => {
            if data.len() > S3_LIMIT {
                bail!("file is larger than 100 MB");
            }
            let key = format!("{}/{}/{}-{}", ctx.db.owner, run.bot_id, Uuid::new_v4().simple(), name);
            let bucket = Bucket::new(s3.endpoint.parse()?, UrlStyle::Path, s3.bucket.clone(), s3.region.clone())?;
            let creds = Credentials::new(s3.access_key_id.clone(), s3.secret_access_key.clone());
            let url = bucket.put_object(Some(&creds), &key).sign(Duration::from_secs(600));
            let resp = reqwest::Client::builder()
                .timeout(Duration::from_secs(300))
                .build()?
                .put(url)
                .header("content-type", mime)
                .body(data.clone())
                .send()
                .await?;
            if !resp.status().is_success() {
                bail!("upload failed: HTTP {}", resp.status());
            }
            ctx.db.insert_artifact(run.id, run.bot_id, &name, mime, data.len() as i64, Stored::S3(&key)).await?
        }
        None => {
            if data.len() > DB_LIMIT {
                bail!("file is larger than 5 MB; configure an S3/R2 store for bigger files");
            }
            ctx.db.insert_artifact(run.id, run.bot_id, &name, mime, data.len() as i64, Stored::Db(&data)).await?
        }
    };
    let _ = events.send(("artifact", json!({ "artifact_id": id, "name": name, "mime": mime, "bytes": data.len() })));
    Ok(id)
}

fn mime_for(name: &str) -> &'static str {
    let ext = name.rsplit('.').next().unwrap_or_default().to_ascii_lowercase();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "pdf" => "application/pdf",
        "json" => "application/json",
        "csv" => "text/csv",
        "md" => "text/markdown",
        "txt" | "log" => "text/plain",
        "html" | "htm" => "text/html",
        "zip" => "application/zip",
        _ => "application/octet-stream",
    }
}
