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
use std::time::{Duration, Instant};

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

/// Who a row's picture belongs to, so downloads take turns between people (`t` is the row).
fn owner_of(table: &str) -> &'static str {
    match table {
        "items" => "(SELECT owner_id FROM wishlists WHERE id = t.wishlist_id)",
        _ => "t.buyer_id",
    }
}
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
        // Look for stray files at startup and then at most hourly, however often we're woken.
        let mut last_sweep: Option<Instant> = None;
        loop {
            // Purge first: it's quick, and shouldn't wait behind a long download queue.
            match purge_unused(&state).await {
                Ok(0) => {}
                Ok(n) => tracing::info!(purged = n, "deleted unused pictures"),
                Err(e) => tracing::warn!(error = %e, "picture purge failed"),
            }
            // Download in slices of a minute or so, so a long backlog (the first start after
            // this feature shipped, say) doesn't hold up purges.
            let more = match store_due(&state, Some(Instant::now() + Duration::from_secs(60))).await
            {
                Ok((stored, more)) => {
                    if stored > 0 {
                        tracing::info!(stored, "stored wish pictures");
                    }
                    more
                }
                Err(e) => {
                    tracing::warn!(error = %e, "picture download pass failed");
                    false
                }
            };
            if last_sweep.is_none_or(|t| t.elapsed() >= Duration::from_secs(3600)) {
                last_sweep = Some(Instant::now());
                match storage::sweep_orphans(&state.db, &state.storage, Duration::from_secs(3600))
                    .await
                {
                    Ok(0) => {}
                    Ok(n) => tracing::info!(swept = n, "deleted stray picture files"),
                    Err(e) => tracing::warn!(error = %e, "stray picture sweep failed"),
                }
            }
            if !more {
                let _ =
                    tokio::time::timeout(Duration::from_secs(3600), state.image_jobs.0.notified())
                        .await;
            }
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

/// A blob's file is gone (storage restored without it, or deleted by hand). Detach it so pages
/// fall back to the original address and the worker downloads the picture again. The blob row
/// itself is left for the worker's purge: only the worker deletes blobs, so it can never be
/// attaching one that a page request just deleted.
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
    tx.commit().await?;
    state.image_jobs.wake();
    Ok(())
}

/// Download every picture that is due, returning how many were stored. Tests call this directly
/// instead of running the worker.
pub async fn store_pending(state: &AppState) -> anyhow::Result<usize> {
    let mut total = 0;
    loop {
        let (stored, more) = store_due(state, None).await?;
        total += stored;
        if !more {
            return Ok(total);
        }
    }
}

/// Download pictures that are due until none are left or `deadline` passes. Returns how many
/// were stored and whether more are waiting. A problem with one picture never stops the rest.
async fn store_due(state: &AppState, deadline: Option<Instant>) -> anyhow::Result<(usize, bool)> {
    let mut stored = 0;
    for table in TABLES {
        loop {
            if deadline.is_some_and(|d| Instant::now() >= d) {
                return Ok((stored, true));
            }
            // Take turns between people: everyone's first picture, then everyone's second, and
            // so on, so one person adding thousands of wishes can't hold up everyone else's.
            let owner = owner_of(table);
            let due: Vec<(i64, String)> = sqlx::query_as(&format!(
                "SELECT t.id, t.image_url FROM {table} t
                  WHERE t.image_url IS NOT NULL AND t.image_blob_id IS NULL AND t.image_attempts < ?
                    AND (t.image_attempted_at IS NULL OR t.image_attempted_at < datetime('now', '-1 hour'))
                  ORDER BY ROW_NUMBER() OVER (PARTITION BY {owner} ORDER BY t.id), t.id
                  LIMIT ?"
            ))
            .bind(MAX_ATTEMPTS)
            .bind(BATCH)
            .fetch_all(&state.db)
            .await?;
            if due.is_empty() {
                break;
            }
            // A few at a time, so one slow store doesn't hold up everyone else's pictures.
            stored += futures_util::stream::iter(due)
                .map(|(id, url)| async move { store_one(state, table, id, &url).await })
                .buffer_unordered(DOWNLOADS_AT_ONCE)
                .filter(|&ok| std::future::ready(ok))
                .count()
                .await;
        }
    }
    Ok((stored, false))
}

/// Why a picture wasn't stored.
enum NotStored {
    /// The picture's fault (unreachable, not an image, too big): counts as a failed attempt.
    Picture,
    /// Ours (storage or database trouble): retried in an hour without using up an attempt.
    Ours(anyhow::Error),
}

impl From<anyhow::Error> for NotStored {
    fn from(e: anyhow::Error) -> Self {
        NotStored::Ours(e)
    }
}

impl From<sqlx::Error> for NotStored {
    fn from(e: sqlx::Error) -> Self {
        NotStored::Ours(e.into())
    }
}

/// Try to store one row's picture, returning whether it was stored.
async fn store_one(state: &AppState, table: &str, id: i64, url: &str) -> bool {
    // Count the attempt first, so a picture that crashes or hangs the download isn't retried
    // in a tight loop. The `image_url = ?` guards here and below skip rows whose picture was
    // changed while we worked; the new address gets its own turn.
    let counted = sqlx::query(&format!(
        "UPDATE {table} SET image_attempts = image_attempts + 1, image_attempted_at = datetime('now')
          WHERE id = ? AND image_url = ?"
    ))
    .bind(id)
    .bind(url)
    .execute(&state.db)
    .await;
    if let Err(e) = counted {
        tracing::warn!(table, id, error = %e, "couldn't start picture download");
        return false;
    }
    match try_store(state, table, id, url).await {
        Ok(attached) => attached,
        Err(NotStored::Picture) => false,
        Err(NotStored::Ours(e)) => {
            tracing::warn!(table, id, url, error = %e, "couldn't store picture; will retry");
            // Give the attempt back; image_attempted_at still holds the retry off for an hour.
            let _ = sqlx::query(&format!(
                "UPDATE {table} SET image_attempts = image_attempts - 1
                  WHERE id = ? AND image_url = ? AND image_attempts > 0"
            ))
            .bind(id)
            .bind(url)
            .execute(&state.db)
            .await;
            false
        }
    }
}

async fn try_store(state: &AppState, table: &str, id: i64, url: &str) -> Result<bool, NotStored> {
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
                    return Err(NotStored::Picture);
                }
            };
            // Only keep pictures we can make page sizes of; anything else stays hotlinked.
            // The original itself isn't kept (pages never show it, and it can be large): the
            // blob records its hash and source, and its sizes are stored.
            let key = storage::content_key(&bytes);
            let size = bytes.len();
            let variants = match make_variants_queued(bytes, content_type).await {
                Ok(variants) => variants,
                Err(e) => {
                    tracing::info!(table, id, url, error = %e, "not keeping picture");
                    return Err(NotStored::Picture);
                }
            };
            let blob_id =
                storage::record_blob(&state.db, &key, content_type, size, Some(url)).await?;
            save_variants(state, blob_id, &key, variants).await?;
            blob_id
        }
    };
    let attached = sqlx::query(&format!(
        "UPDATE {table} SET image_blob_id = ?1 WHERE id = ?2 AND image_url = ?3
            AND EXISTS (SELECT 1 FROM blobs WHERE id = ?1)"
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

/// Decoding is the expensive part (a large photo can take a second and around a hundred MB), so
/// only one picture is decoded at a time.
static DECODE: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);

/// Largest picture we keep, in pixels per side and in decoded bytes (about 24 megapixels with
/// transparency, 32 without). Anything bigger is left hotlinked rather than stored.
const MAX_SIDE: u32 = 6_000;
const MAX_DECODED_BYTES: u64 = 96 * 1024 * 1024;

/// One stored size: its bytes and content type.
type Resized = (Vec<u8>, &'static str);

/// Decode a picture once and make every [`Variant`] of it. Fails if it isn't a picture we can
/// decode within the limits, which is how the worker decides whether to keep it at all.
/// `content_type` is the original's, used when it is already the smallest form of a size.
pub fn make_variants(
    bytes: &[u8],
    content_type: &'static str,
) -> anyhow::Result<Vec<(Variant, Resized)>> {
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
        // Use the original's own bytes when they're already this size, the right way up, and
        // smaller than re-encoding. Either way a size is never bigger than its re-encoding.
        let keep_original = as_stored
            && (img.width(), img.height()) == original_size
            && encoded.0.len() >= bytes.len();
        let sized = if keep_original {
            (bytes.to_vec(), content_type)
        } else {
            encoded
        };
        out.push((variant, sized));
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

/// Decode `bytes` and make its variants, one picture at a time across the whole server. The
/// turn is held inside the blocking task, so it lasts exactly as long as the decode does.
async fn make_variants_queued(
    bytes: Vec<u8>,
    content_type: &'static str,
) -> anyhow::Result<Vec<(Variant, Resized)>> {
    let turn = DECODE.acquire().await?;
    tokio::task::spawn_blocking(move || {
        let _turn = turn;
        make_variants(&bytes, content_type)
    })
    .await?
}

/// Store a blob's variants and record them. Skipped quietly if the blob was purged meanwhile:
/// the purge deletes variant files by name, and the sweep catches any written after it.
async fn save_variants(
    state: &AppState,
    blob_id: i64,
    blob_key: &str,
    variants: Vec<(Variant, Resized)>,
) -> anyhow::Result<()> {
    for (variant, (bytes, content_type)) in variants {
        let key = variant.storage_key(blob_key);
        let size = bytes.len() as i64;
        state.storage.put(&key, bytes).await?;
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

/// A stored picture at the requested size, as a stream of its bytes and its content type, or
/// `None` if there isn't one. Page requests never decode anything: if a size's file has gone
/// missing, the picture is detached and the worker downloads it again.
pub async fn variant(
    state: &AppState,
    blob_key: &str,
    variant: Variant,
) -> anyhow::Result<Option<(storage::ByteStream, String)>> {
    let found: Option<(i64, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT b.id, v.key, v.content_type FROM blobs b
           LEFT JOIN blob_variants v ON v.blob_id = b.id AND v.name = ?
          WHERE b.key = ?",
    )
    .bind(variant.name())
    .bind(blob_key)
    .fetch_optional(&state.db)
    .await?;
    let Some((blob_id, key, content_type)) = found else {
        return Ok(None);
    };
    if let (Some(key), Some(content_type)) = (key, content_type) {
        if let Some(stream) = state.storage.get_stream(&key).await? {
            return Ok(Some((stream, content_type)));
        }
    }
    forget_missing(state, blob_id).await?;
    Ok(None)
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

    /// The picture at one size, decoded, with its type.
    fn sized(bytes: &[u8], want: Variant) -> (DynamicImage, &'static str) {
        let (_, (out, ct)) = make_variants(bytes, "image/png")
            .unwrap()
            .into_iter()
            .find(|(v, _)| *v == want)
            .unwrap();
        (image::load_from_memory(&out).unwrap(), ct)
    }

    #[test]
    fn shrinks_large_pictures_keeping_shape() {
        let big = encode(noisy(1200, 600, false), image::ImageFormat::Png);
        let (card, ct) = sized(&big, Variant::Card);
        assert_eq!(ct, "image/jpeg", "opaque pictures become JPEG");
        assert_eq!((card.width(), card.height()), (480, 240));
        let (thumb, _) = sized(&big, Variant::Thumb);
        assert_eq!((thumb.width(), thumb.height()), (160, 80));
    }

    #[test]
    fn keeps_transparency() {
        let big = encode(noisy(800, 800, true), image::ImageFormat::Png);
        let (img, ct) = sized(&big, Variant::Thumb);
        assert_eq!(ct, "image/png");
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
        assert!(make_variants(b"not a picture", "image/png").is_err());
        let too_wide = encode(noisy(MAX_SIDE + 1, 2, false), image::ImageFormat::Png);
        assert!(make_variants(&too_wide, "image/png").is_err());
    }

    #[test]
    fn variant_names_round_trip() {
        for v in Variant::ALL {
            assert_eq!(Variant::from_name(v.name()), Some(v));
        }
        assert_eq!(Variant::from_name("huge"), None);
    }
}
