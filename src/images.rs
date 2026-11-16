//! Keeping copies of wish pictures.
//!
//! Saving a wish stores the picture's original address (`image_url`) right away. A background
//! worker then downloads the picture into [`crate::storage`] and records the blob on the row
//! (`image_blob_id`), after which pages serve our copy from `/images/<key>`. Until then, or if the
//! download keeps failing, pages fall back to the original address.
//!
//! Rows are picked up by a query rather than a queue, so nothing is lost on restart and pictures
//! that were added before this existed are fetched too. A failed download is retried up to
//! [`MAX_ATTEMPTS`] times, at least an hour apart.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Notify;

use crate::storage;
use crate::AppState;

/// Downloads tried per picture before giving up and leaving the original address.
pub const MAX_ATTEMPTS: i64 = 3;
/// Tables with an `image_url` whose picture should be kept.
const TABLES: [&str; 2] = ["items", "gift_history"];
const BATCH: i64 = 20;

/// Wakes the worker when a wish with a new picture is saved.
#[derive(Clone, Default)]
pub struct ImageJobs(Arc<Notify>);

impl ImageJobs {
    pub fn wake(&self) {
        self.0.notify_one();
    }
}

/// Run the downloader for as long as the server runs. It works through everything pending, then
/// sleeps until woken by a save, or for an hour (to retry failures and purge unused files).
pub fn spawn_worker(state: AppState) {
    tokio::spawn(async move {
        loop {
            match store_pending(&state).await {
                Ok(0) => {}
                Ok(n) => tracing::info!(stored = n, "stored wish pictures"),
                Err(e) => tracing::warn!(error = %e, "picture download pass failed"),
            }
            match storage::purge_unattached(&state.db, &state.storage).await {
                Ok(0) => {}
                Ok(n) => tracing::info!(purged = n, "deleted unused pictures"),
                Err(e) => tracing::warn!(error = %e, "picture purge failed"),
            }
            let _ = tokio::time::timeout(Duration::from_secs(3600), state.image_jobs.0.notified())
                .await;
        }
    });
}

/// Download every picture that is due, returning how many were stored. Tests call this directly
/// instead of running the worker.
pub async fn store_pending(state: &AppState) -> anyhow::Result<usize> {
    let mut stored = 0;
    for table in TABLES {
        loop {
            let due: Vec<(i64, String)> = sqlx::query_as(&format!(
                "SELECT id, image_url FROM {table}
                  WHERE image_url IS NOT NULL AND image_blob_id IS NULL AND image_attempts < ?
                    AND (image_attempted_at IS NULL OR image_attempted_at < datetime('now', '-1 hour'))
                  ORDER BY id LIMIT ?"
            ))
            .bind(MAX_ATTEMPTS)
            .bind(BATCH)
            .fetch_all(&state.db)
            .await?;
            if due.is_empty() {
                break;
            }
            for (id, url) in due {
                if store_one(state, table, id, &url).await? {
                    stored += 1;
                }
            }
        }
    }
    Ok(stored)
}

async fn store_one(state: &AppState, table: &str, id: i64, url: &str) -> anyhow::Result<bool> {
    // Count the attempt first, so a picture that crashes or hangs the download isn't retried
    // in a tight loop. The `image_url = ?` guards here and below skip rows whose picture was
    // changed while we worked; the new address gets its own turn.
    sqlx::query(&format!(
        "UPDATE {table} SET image_attempts = image_attempts + 1, image_attempted_at = datetime('now')
          WHERE id = ? AND image_url = ?"
    ))
    .bind(id)
    .bind(url)
    .execute(&state.db)
    .await?;

    // The same address is often already stored (a copied wish, a gift bought from a list).
    let known: Option<i64> = sqlx::query_scalar(
        "SELECT image_blob_id FROM items WHERE image_url = ?1 AND image_blob_id IS NOT NULL
         UNION ALL
         SELECT image_blob_id FROM gift_history WHERE image_url = ?1 AND image_blob_id IS NOT NULL
         LIMIT 1",
    )
    .bind(url)
    .fetch_optional(&state.db)
    .await?;
    let blob_id = match known {
        Some(id) => id,
        None => match state.importer.download_image(url).await {
            Ok((bytes, content_type)) => {
                storage::create_blob(&state.db, &state.storage, bytes, content_type, Some(url))
                    .await?
            }
            Err(e) => {
                tracing::debug!(table, id, url, error = %e, "couldn't download picture");
                return Ok(false);
            }
        },
    };
    let attached = sqlx::query(&format!(
        "UPDATE {table} SET image_blob_id = ? WHERE id = ? AND image_url = ?"
    ))
    .bind(blob_id)
    .bind(id)
    .bind(url)
    .execute(&state.db)
    .await?
    .rows_affected();
    Ok(attached == 1)
}
