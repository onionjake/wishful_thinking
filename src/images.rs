//! Keeping copies of wish pictures.
//!
//! Saving a wish stores the picture's original address (`image_url`) right away. A background
//! worker then downloads the picture into [`crate::storage`] and records the blob on the row
//! (`image_blob_id`), after which pages serve our copy from `/images/<key>`. Until then, or if the
//! download keeps failing, pages fall back to the original address.
//!
//! Pages show pictures through named [`Variant`]s, resized copies the worker makes when it
//! stores a picture, never the full original. Pictures it can't decode (or that are too big to)
//! aren't kept. Deleting or changing a wish wakes the worker, which deletes
//! pictures, and their variants, that nothing uses any more.
//!
//! Rows are picked up by a query rather than a queue, so nothing is lost on restart and pictures
//! that were added before this existed are fetched too. A failed download is retried up to
//! [`MAX_ATTEMPTS`] times, at least an hour apart.

use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
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
const DOWNLOADS_AT_ONCE: usize = 4;

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
        // Look for stray files at startup and then on each hourly (unprompted) pass.
        let mut sweep = true;
        loop {
            // Purge first: it's quick, and shouldn't wait behind a long download queue.
            match purge_unused(&state).await {
                Ok(0) => {}
                Ok(n) => tracing::info!(purged = n, "deleted unused pictures"),
                Err(e) => tracing::warn!(error = %e, "picture purge failed"),
            }
            match store_pending(&state).await {
                Ok(0) => {}
                Ok(n) => tracing::info!(stored = n, "stored wish pictures"),
                Err(e) => tracing::warn!(error = %e, "picture download pass failed"),
            }
            if sweep {
                match storage::sweep_orphans(&state.db, &state.storage, Duration::from_secs(3600))
                    .await
                {
                    Ok(0) => {}
                    Ok(n) => tracing::info!(swept = n, "deleted stray picture files"),
                    Err(e) => tracing::warn!(error = %e, "stray picture sweep failed"),
                }
            }
            sweep = tokio::time::timeout(Duration::from_secs(3600), state.image_jobs.0.notified())
                .await
                .is_err();
        }
    });
}

/// Delete pictures nothing uses any more, with all their sizes. Only the worker calls this
/// (tests aside); see [`storage::purge_unattached`].
pub async fn purge_unused(state: &AppState) -> anyhow::Result<usize> {
    storage::purge_unattached(&state.db, &state.storage, |key| {
        Variant::ALL.iter().map(|v| v.storage_key(key)).collect()
    })
    .await
}

/// A blob's file is gone (storage restored without it, or deleted by hand). Forget the blob so
/// pages fall back to the original address and the worker downloads the picture again.
pub async fn forget_missing(state: &AppState, blob_id: i64) -> anyhow::Result<()> {
    tracing::warn!(
        blob_id,
        "stored picture is missing; it will be downloaded again"
    );
    let mut tx = state.db.begin().await?;
    for table in TABLES {
        sqlx::query(&format!(
            "UPDATE {table} SET image_blob_id = NULL, image_attempts = 0, image_attempted_at = NULL
              WHERE image_blob_id = ?"
        ))
        .bind(blob_id)
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query("DELETE FROM blobs WHERE id = ?")
        .bind(blob_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    state.image_jobs.wake();
    Ok(())
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
            // A few at a time, so one slow store doesn't hold up everyone else's pictures.
            let results: Vec<anyhow::Result<bool>> = futures_util::stream::iter(due)
                .map(|(id, url)| async move { store_one(state, table, id, &url).await })
                .buffer_unordered(DOWNLOADS_AT_ONCE)
                .collect()
                .await;
            for result in results {
                if result? {
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
        None => {
            let (bytes, content_type) = match state.importer.download_image(url).await {
                Ok(downloaded) => downloaded,
                Err(e) => {
                    tracing::debug!(table, id, url, error = %e, "couldn't download picture");
                    return Ok(false);
                }
            };
            // Only keep pictures we can make page sizes of; anything else stays hotlinked.
            let bytes = bytes::Bytes::from(bytes);
            let variants = match make_variants_queued(bytes.clone()).await {
                Ok(variants) => variants,
                Err(e) => {
                    tracing::info!(table, id, url, error = %e, "not keeping picture");
                    return Ok(false);
                }
            };
            let blob_id = storage::create_blob(
                &state.db,
                &state.storage,
                bytes.to_vec(),
                content_type,
                Some(url),
            )
            .await?;
            let key: String = sqlx::query_scalar("SELECT key FROM blobs WHERE id = ?")
                .bind(blob_id)
                .fetch_one(&state.db)
                .await?;
            save_variants(state, blob_id, &key, variants).await?;
            blob_id
        }
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

/// Decoding is the expensive part (a large photo can take a second and a few hundred MB), so
/// only one picture is decoded at a time, whether by the worker or a page request.
static DECODE: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);

/// Largest picture we keep, in pixels per side and in decoded bytes. Anything bigger is left
/// hotlinked rather than stored.
const MAX_SIDE: u32 = 8_000;
const MAX_DECODED_BYTES: u64 = 160 * 1024 * 1024;

/// One resized copy: its bytes and type, or `None` to serve the original as is.
type Resized = Option<(Vec<u8>, &'static str)>;

/// Decode a picture once and make every [`Variant`] of it. Fails if it isn't a picture we can
/// decode within the limits, which is how the worker decides whether to keep it at all.
pub fn make_variants(bytes: &[u8]) -> anyhow::Result<Vec<(Variant, Resized)>> {
    use image::ImageDecoder;
    let mut reader = ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format()?;
    // Guard against "decompression bombs": tiny files that claim enormous dimensions.
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    limits.max_alloc = Some(MAX_DECODED_BYTES);
    reader.limits(limits);
    let mut decoder = reader.into_decoder()?;
    anyhow::ensure!(
        decoder.total_bytes() <= MAX_DECODED_BYTES,
        "picture too large to decode"
    );
    // Phone photos are often stored sideways with an EXIF note saying which way is up.
    let orientation = decoder.orientation()?;
    let mut img = DynamicImage::from_decoder(decoder)?;
    img.apply_orientation(orientation);
    let original_size = (img.width(), img.height());
    let as_stored = orientation == image::metadata::Orientation::NoTransforms;
    let mut out = Vec::new();
    // Largest first, so each smaller size is made from the previous one (much cheaper).
    let mut sizes = Variant::ALL;
    sizes.sort_by_key(|v| std::cmp::Reverse(v.max_side()));
    for variant in sizes {
        let max = variant.max_side();
        if img.width() > max || img.height() > max {
            img = img.resize(max, max, FilterType::Lanczos3);
        }
        let encoded = encode(&img)?;
        // Keep the original when it's no bigger and needs no turning; the variant's job is to
        // be small.
        let keep_original = as_stored
            && (img.width(), img.height()) == original_size
            && encoded.0.len() >= bytes.len();
        out.push((variant, (!keep_original).then_some(encoded)));
    }
    Ok(out)
}

/// Pictures with transparency stay PNG so they sit right on any background; the rest become
/// JPEG. Animated GIFs keep their first frame.
fn encode(img: &DynamicImage) -> anyhow::Result<(Vec<u8>, &'static str)> {
    let mut out = Vec::new();
    let content_type = if has_transparency(img) {
        img.write_with_encoder(PngEncoder::new_with_quality(
            &mut out,
            CompressionType::Best,
            PngFilter::Adaptive,
        ))?;
        "image/png"
    } else {
        DynamicImage::ImageRgb8(img.to_rgb8())
            .write_with_encoder(JpegEncoder::new_with_quality(&mut out, 82))?;
        "image/jpeg"
    };
    Ok((out, content_type))
}

/// Decode `bytes` and make its variants, one picture at a time across the whole server.
async fn make_variants_queued(bytes: bytes::Bytes) -> anyhow::Result<Vec<(Variant, Resized)>> {
    let _turn = DECODE.acquire().await?;
    tokio::task::spawn_blocking(move || make_variants(&bytes)).await?
}

/// Store a blob's variants and record them. Skipped quietly if the blob was purged meanwhile:
/// the purge deletes variant files by name, and the sweep catches any written after it.
async fn save_variants(
    state: &AppState,
    blob_id: i64,
    blob_key: &str,
    variants: Vec<(Variant, Resized)>,
) -> anyhow::Result<()> {
    for (variant, resized) in variants {
        let (key, content_type, size) = match resized {
            Some((bytes, content_type)) => {
                let key = variant.storage_key(blob_key);
                let size = bytes.len() as i64;
                state.storage.put(&key, bytes).await?;
                (Some(key), Some(content_type), Some(size))
            }
            None => (None, None, None),
        };
        sqlx::query(
            "INSERT INTO blob_variants (blob_id, name, key, content_type, byte_size)
             SELECT ?1, ?2, ?3, ?4, ?5 WHERE EXISTS (SELECT 1 FROM blobs WHERE id = ?1)
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
    }
    Ok(())
}

/// A stored picture at the requested size, as (bytes, content type). `None` if the blob isn't
/// stored. The worker makes every size when it stores a picture; this only has to make one again
/// if its file has gone missing.
pub async fn variant(
    state: &AppState,
    blob_key: &str,
    variant: Variant,
) -> anyhow::Result<Option<(bytes::Bytes, String)>> {
    let Some((blob_id, original_type)) = storage::find_blob(&state.db, blob_key).await? else {
        return Ok(None);
    };
    let mut remade = false;
    loop {
        let made: Option<(Option<String>, Option<String>)> = sqlx::query_as(
            "SELECT key, content_type FROM blob_variants WHERE blob_id = ? AND name = ?",
        )
        .bind(blob_id)
        .bind(variant.name())
        .fetch_optional(&state.db)
        .await?;
        if let Some((Some(key), Some(content_type))) = &made {
            if let Some(bytes) = state.storage.get(key).await? {
                return Ok(Some((bytes, content_type.clone())));
            }
        }
        let Some(original) = state.storage.get(blob_key).await? else {
            forget_missing(state, blob_id).await?;
            return Ok(None);
        };
        if matches!(made, Some((None, _))) || remade {
            // Recorded as "serve the original as is" (or remaking didn't produce a file).
            return Ok(Some((original, original_type)));
        }
        // The size is missing. Remake every size; if another request is already doing so, this
        // waits its turn and then finds the files on the next pass round the loop.
        let variants = match make_variants_queued(original).await {
            Ok(variants) => variants,
            Err(e) => {
                tracing::warn!(blob_key, error = %e, "couldn't remake picture sizes");
                remade = true;
                continue;
            }
        };
        save_variants(state, blob_id, blob_key, variants).await?;
        remade = true;
    }
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

    /// The picture at one size, decoded, with its type (`None` = the original as is).
    fn sized(bytes: &[u8], want: Variant) -> (DynamicImage, Option<&'static str>) {
        let (_, resized) = make_variants(bytes)
            .unwrap()
            .into_iter()
            .find(|(v, _)| *v == want)
            .unwrap();
        match resized {
            Some((out, ct)) => (image::load_from_memory(&out).unwrap(), Some(ct)),
            None => (image::load_from_memory(bytes).unwrap(), None),
        }
    }

    #[test]
    fn shrinks_large_pictures_keeping_shape() {
        let big = encode(noisy(1200, 600, false), image::ImageFormat::Png);
        let (card, ct) = sized(&big, Variant::Card);
        assert_eq!(ct, Some("image/jpeg"), "opaque pictures become JPEG");
        assert_eq!((card.width(), card.height()), (480, 240));
        let (thumb, _) = sized(&big, Variant::Thumb);
        assert_eq!((thumb.width(), thumb.height()), (160, 80));
    }

    #[test]
    fn keeps_transparency() {
        let big = encode(noisy(800, 800, true), image::ImageFormat::Png);
        let (img, ct) = sized(&big, Variant::Thumb);
        assert_eq!(ct, Some("image/png"));
        assert_eq!((img.width(), img.height()), (160, 160));
        assert!(img.to_rgba8().pixels().any(|p| p.0[3] == 0));
    }

    #[test]
    fn never_enlarges_small_pictures() {
        let small = encode(noisy(100, 80, false), image::ImageFormat::Png);
        for v in Variant::ALL {
            let (img, _) = sized(&small, v);
            assert_eq!((img.width(), img.height()), (100, 80));
        }
    }

    #[test]
    fn refuses_what_it_cant_or_shouldnt_decode() {
        assert!(make_variants(b"not a picture").is_err());
        let too_wide = encode(noisy(MAX_SIDE + 1, 2, false), image::ImageFormat::Png);
        assert!(make_variants(&too_wide).is_err());
    }

    #[test]
    fn variant_names_round_trip() {
        for v in Variant::ALL {
            assert_eq!(Variant::from_name(v.name()), Some(v));
        }
        assert_eq!(Variant::from_name("huge"), None);
    }
}
