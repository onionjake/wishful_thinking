//! Keeping copies of wish pictures.
//!
//! Saving a wish stores the picture's original address (`image_url`) right away. A background
//! worker then downloads the picture into [`crate::storage`] and records the blob on the row
//! (`image_blob_id`), after which pages serve our copy from `/images/<key>`. Until then, or if the
//! download keeps failing, pages fall back to the original address.
//!
//! Pages show pictures through named [`Variant`]s (resized copies made on first request and then
//! stored), never the full original. Deleting or changing a wish wakes the worker, which deletes
//! pictures, and their variants, that nothing uses any more.
//!
//! Rows are picked up by a query rather than a queue, so nothing is lost on restart and pictures
//! that were added before this existed are fetched too. A failed download is retried up to
//! [`MAX_ATTEMPTS`] times, at least an hour apart.

use std::sync::Arc;
use std::time::Duration;

use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::{CompressionType, FilterType as PngFilter, PngEncoder};
use image::imageops::FilterType;
use image::{DynamicImage, ImageReader, Limits};
use tokio::sync::Notify;

use crate::storage;
use crate::AppState;

/// Downloads tried per picture before giving up and leaving the original address.
pub const MAX_ATTEMPTS: i64 = 3;
/// Tables with an `image_url` whose picture should be kept.
const TABLES: [&str; 2] = ["items", "gift_history"];
const BATCH: i64 = 20;

/// Wakes the worker when a wish's picture is added, changed or deleted.
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

/// The sizes pages ask for. Each fits the picture inside a square box (keeping its shape) sized
/// for 2× screens: `Thumb` for the 56px rows, `Card` for the 132px item cards and list previews.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Variant {
    Thumb,
    Card,
}

impl Variant {
    pub const ALL: [Variant; 2] = [Variant::Thumb, Variant::Card];

    pub fn name(self) -> &'static str {
        match self {
            Variant::Thumb => "thumb",
            Variant::Card => "card",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.name() == name)
    }

    fn max_side(self) -> u32 {
        match self {
            Variant::Thumb => 160,
            Variant::Card => 480,
        }
    }

    /// Where a blob's copy at this size is stored.
    pub fn storage_key(self, blob_key: &str) -> String {
        format!("{blob_key}-{}", self.name())
    }
}

/// The page address of a stored picture at a given size.
pub fn variant_path(blob_key: &str, variant: Variant) -> String {
    format!("{}/{}", storage::image_path(blob_key), variant.name())
}

/// A stored picture at the requested size, as (bytes, content type), making and storing the
/// resized copy the first time it's asked for. `None` if the blob isn't stored.
pub async fn variant(
    state: &AppState,
    blob_key: &str,
    variant: Variant,
) -> anyhow::Result<Option<(bytes::Bytes, String)>> {
    let Some((blob_id, original_type)) =
        sqlx::query_as::<_, (i64, String)>("SELECT id, content_type FROM blobs WHERE key = ?")
            .bind(blob_key)
            .fetch_optional(&state.db)
            .await?
    else {
        return Ok(None);
    };
    let made: Option<(Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT key, content_type FROM blob_variants WHERE blob_id = ? AND name = ?",
    )
    .bind(blob_id)
    .bind(variant.name())
    .fetch_optional(&state.db)
    .await?;
    match made {
        Some((Some(key), Some(content_type))) => {
            if let Some(bytes) = state.storage.get(&key).await? {
                return Ok(Some((bytes, content_type)));
            }
            // The file went missing: make it again below.
        }
        Some(_) => {
            return Ok(state
                .storage
                .get(blob_key)
                .await?
                .map(|bytes| (bytes, original_type)));
        }
        None => {}
    }

    let Some(original) = state.storage.get(blob_key).await? else {
        return Ok(None);
    };
    let input = original.clone();
    let max = variant.max_side();
    let resized = tokio::task::spawn_blocking(move || resize(&input, max)).await?;
    let (key, content_type, size, bytes) = match resized {
        Ok(Some((bytes, content_type))) => {
            let key = variant.storage_key(blob_key);
            state.storage.put(&key, bytes.clone()).await?;
            (
                Some(key),
                Some(content_type),
                Some(bytes.len() as i64),
                bytes.into(),
            )
        }
        Ok(None) => (None, None, None, original.clone()),
        Err(e) => {
            // Undecodable here (e.g. AVIF, which we keep but can't resize): serve the original.
            tracing::debug!(blob_key, error = %e, "couldn't resize picture");
            (None, None, None, original.clone())
        }
    };
    sqlx::query(
        "INSERT INTO blob_variants (blob_id, name, key, content_type, byte_size) VALUES (?, ?, ?, ?, ?)
         ON CONFLICT(blob_id, name) DO UPDATE SET key = excluded.key,
             content_type = excluded.content_type, byte_size = excluded.byte_size",
    )
    .bind(blob_id)
    .bind(variant.name())
    .bind(&key)
    .bind(content_type)
    .bind(size)
    .execute(&state.db)
    .await?;
    Ok(Some((
        bytes,
        content_type.map_or(original_type, str::to_string),
    )))
}

/// Shrink a picture to fit in a `max`×`max` box. `Ok(None)` when the original is already small
/// enough, or shrinking wouldn't make the file smaller. Pictures with transparency stay PNG so
/// they sit right on any background; the rest become JPEG. Animated GIFs keep their first frame.
pub fn resize(bytes: &[u8], max: u32) -> anyhow::Result<Option<(Vec<u8>, &'static str)>> {
    let mut reader = ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format()?;
    // Guard against "decompression bombs": tiny files that claim enormous dimensions.
    let mut limits = Limits::default();
    limits.max_image_width = Some(12_000);
    limits.max_image_height = Some(12_000);
    limits.max_alloc = Some(256 * 1024 * 1024);
    reader.limits(limits);
    let img = reader.decode()?;
    if img.width() <= max && img.height() <= max {
        return Ok(None);
    }
    let small = img.resize(max, max, FilterType::Lanczos3);
    let mut out = Vec::new();
    let content_type = if has_transparency(&small) {
        small.write_with_encoder(PngEncoder::new_with_quality(
            &mut out,
            CompressionType::Best,
            PngFilter::Adaptive,
        ))?;
        "image/png"
    } else {
        DynamicImage::ImageRgb8(small.to_rgb8())
            .write_with_encoder(JpegEncoder::new_with_quality(&mut out, 82))?;
        "image/jpeg"
    };
    Ok((out.len() < bytes.len()).then_some((out, content_type)))
}

/// Whether any pixel is see-through (many PNGs have an alpha channel but use none of it).
fn has_transparency(img: &DynamicImage) -> bool {
    img.color().has_alpha() && img.to_rgba8().pixels().any(|p| p.0[3] < 255)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode(img: DynamicImage, format: image::ImageFormat) -> Vec<u8> {
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, format).unwrap();
        out.into_inner()
    }

    fn noisy(w: u32, h: u32, alpha: bool) -> DynamicImage {
        let img = image::RgbaImage::from_fn(w, h, |x, y| {
            let v = ((x * 7 + y * 13) ^ (x * y)) as u8;
            image::Rgba([
                v,
                v.wrapping_mul(3),
                v.wrapping_add(90),
                if alpha && x < w / 2 { 0 } else { 255 },
            ])
        });
        DynamicImage::ImageRgba8(img)
    }

    #[test]
    fn shrinks_large_pictures_keeping_shape() {
        let big = encode(noisy(1200, 600, false), image::ImageFormat::Png);
        let (out, content_type) = resize(&big, 480).unwrap().expect("resized");
        assert_eq!(content_type, "image/jpeg", "opaque pictures become JPEG");
        let img = image::load_from_memory(&out).unwrap();
        assert_eq!((img.width(), img.height()), (480, 240));
    }

    #[test]
    fn keeps_transparency() {
        let big = encode(noisy(800, 800, true), image::ImageFormat::Png);
        let (out, content_type) = resize(&big, 160).unwrap().expect("resized");
        assert_eq!(content_type, "image/png");
        let img = image::load_from_memory(&out).unwrap();
        assert_eq!((img.width(), img.height()), (160, 160));
        assert!(img.to_rgba8().pixels().any(|p| p.0[3] == 0));
    }

    #[test]
    fn leaves_small_pictures_alone() {
        let small = encode(noisy(100, 80, false), image::ImageFormat::Png);
        assert!(resize(&small, 160).unwrap().is_none());
        assert!(resize(b"not a picture", 160).is_err());
    }

    #[test]
    fn variant_names_round_trip() {
        for v in Variant::ALL {
            assert_eq!(Variant::from_name(v.name()), Some(v));
        }
        assert_eq!(Variant::from_name("huge"), None);
    }
}
