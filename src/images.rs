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

/// Downloads tried per picture before giving up and leaving the original address. With the
/// doubling wait between them (see [`due`]) they span about a day and a half.
pub const MAX_ATTEMPTS: i64 = 6;
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
            // Touch nothing unless the store is the one the database describes.
            match storage::check_identity(&state.db, &state.storage).await {
                Ok(true) => {}
                Ok(false) => {
                    tracing::error!(
                        storage = ?state.storage,
                        "the picture store doesn't match the database (a wrong WT_STORAGE_URL or \
                         WT_DATABASE_URL, or a disk not mounted?), so pictures won't be stored or \
                         cleaned up; pages show their original addresses. If the store really was \
                         lost or replaced, run `wishful_thinking reset-pictures`."
                    );
                    let _ = tokio::time::timeout(
                        Duration::from_secs(3600),
                        state.image_jobs.0.notified(),
                    )
                    .await;
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    continue;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "couldn't check the picture store");
                    tokio::time::sleep(Duration::from_secs(60)).await;
                    continue;
                }
            }
            match confirm_missing(&state).await {
                Ok(0) => {}
                Ok(n) => tracing::info!(forgotten = n, "picture sizes confirmed missing"),
                Err(e) => tracing::warn!(error = %e, "checking missing pictures failed"),
            }
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
            // However often we're woken, run at most every few seconds.
            tokio::time::sleep(Duration::from_secs(5)).await;
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

/// Check the sizes that page requests found missing (`missing_at`). Page requests only flag
/// them, since anyone can make one; the worker decides, and only once it knows the store is the
/// right one ([`storage::check_identity`]). A size whose file is really gone is forgotten (see
/// [`forget_missing`]); one that's back clears its flag.
pub async fn confirm_missing(state: &AppState) -> anyhow::Result<usize> {
    let flagged: Vec<(i64, String, String)> =
        sqlx::query_as("SELECT blob_id, name, key FROM blob_variants WHERE missing_at IS NOT NULL")
            .fetch_all(&state.db)
            .await?;
    if flagged.is_empty() {
        return Ok(0);
    }
    let mut forgotten = 0;
    for (blob_id, name, key) in flagged {
        if state.storage.exists(&key).await? {
            sqlx::query(
                "UPDATE blob_variants SET missing_at = NULL WHERE blob_id = ? AND name = ?",
            )
            .bind(blob_id)
            .bind(&name)
            .execute(&state.db)
            .await?;
        } else if let Some(variant) = Variant::from_name(&name) {
            forget_missing(state, blob_id, variant).await?;
            forgotten += 1;
        }
    }
    Ok(forgotten)
}

/// Forget every stored picture, so each wish shows its original address and downloads it again,
/// and pair the database with `storage` as it is now. For when the store was really lost or
/// replaced (`wishful_thinking reset-pictures`).
pub async fn reset_all(db: &crate::db::Db, storage: &storage::Storage) -> anyhow::Result<u64> {
    let mut tx = db.begin().await?;
    let mut detached = 0;
    for table in TABLES {
        detached += sqlx::query(&format!(
            "UPDATE {table} SET image_blob_id = NULL, image_attempts = 0, image_attempted_at = NULL
              WHERE image_url IS NOT NULL"
        ))
        .execute(&mut *tx)
        .await?
        .rows_affected();
    }
    sqlx::query("DELETE FROM blobs").execute(&mut *tx).await?;
    tx.commit().await?;
    storage::pair(db, storage).await?;
    Ok(detached)
}

/// Give every picture still waiting to be stored, and every stored picture waiting for a lost
/// size, a fresh set of attempts. Stored pictures are untouched. For after an outage long enough
/// to use the attempts up (`wishful_thinking retry-pictures`).
pub async fn retry_all(db: &crate::db::Db) -> anyhow::Result<u64> {
    let mut tx = db.begin().await?;
    let mut reset = 0;
    for table in TABLES {
        reset += sqlx::query(&format!(
            "UPDATE {table} SET image_attempts = 0, image_attempted_at = NULL
              WHERE image_url IS NOT NULL AND image_blob_id IS NULL AND image_attempts > 0"
        ))
        .execute(&mut *tx)
        .await?
        .rows_affected();
    }
    sqlx::query("UPDATE blobs SET refetch_attempts = 0, refetch_attempted_at = NULL")
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(reset)
}

/// A stored size's file is confirmed gone. Forget that size so the worker downloads the picture
/// again and remakes it. The picture's other sizes keep working meanwhile, and if the download
/// never succeeds (the store's link has died) they're all that's left, so they must stay. Only
/// the worker calls this.
async fn forget_missing(state: &AppState, blob_id: i64, variant: Variant) -> anyhow::Result<()> {
    tracing::warn!(
        blob_id,
        size = variant.name(),
        "stored picture is missing; it will be downloaded again"
    );
    sqlx::query("DELETE FROM blob_variants WHERE blob_id = ? AND name = ?")
        .bind(blob_id)
        .bind(variant.name())
        .execute(&state.db)
        .await?;
    // With no size left there's nothing to show: detach the picture so the worker downloads
    // whatever its address serves now, like a new wish (the purge then removes the empty blob).
    let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM blob_variants WHERE blob_id = ?")
        .bind(blob_id)
        .fetch_one(&state.db)
        .await?;
    if left == 0 {
        for table in TABLES {
            sqlx::query(&format!(
                "UPDATE {table} SET image_blob_id = NULL, image_attempts = 0, image_attempted_at = NULL
                  WHERE image_blob_id = ?"
            ))
            .bind(blob_id)
            .execute(&state.db)
            .await?;
        }
        return Ok(());
    }
    // If earlier re-downloads of this picture gave up long ago, give it a fresh set of tries.
    sqlx::query(&format!(
        "UPDATE blobs SET refetch_attempts = 0, refetch_attempted_at = NULL
          WHERE id = ? AND refetch_attempts >= {MAX_ATTEMPTS}
            AND refetch_attempted_at < datetime('now', '-1 day')"
    ))
    .bind(blob_id)
    .execute(&state.db)
    .await?;
    Ok(())
}

/// SQL that's true when a row (with `{p}attempts` and `{p}attempted_at` columns) is due for
/// another try: under [`MAX_ATTEMPTS`], and waiting 1, 2, 4, 8… hours after each failure, so a
/// few hours' outage on our side or the store's doesn't use up a picture's chances.
fn due(p: &str) -> String {
    format!(
        "{p}attempts < {MAX_ATTEMPTS} AND ({p}attempted_at IS NULL
           OR {p}attempted_at < datetime('now', printf('-%d hours', 1 << max({p}attempts - 1, 0))))"
    )
}

/// Check flagged sizes, then download every picture that is due, returning how many were
/// stored: one full pass of the worker's work apart from purging. Tests call this directly
/// instead of running the worker.
pub async fn store_pending(state: &AppState) -> anyhow::Result<usize> {
    if !storage::check_identity(&state.db, &state.storage).await? {
        return Ok(0);
    }
    confirm_missing(state).await?;
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
/// were stored and whether more are waiting. A problem with one picture never stops the rest,
/// and each picture is tried at most once per pass (so the pass ends even if, say, the
/// database won't take writes and attempts can't be recorded).
async fn store_due(state: &AppState, deadline: Option<Instant>) -> anyhow::Result<(usize, bool)> {
    let mut stored = 0;
    let mut tried = std::collections::HashSet::new();
    // A batch from each table in turn, so neither can starve the other.
    let mut tables: Vec<&str> = TABLES.to_vec();
    while !tables.is_empty() {
        if deadline.is_some_and(|d| Instant::now() >= d) {
            return Ok((stored, true));
        }
        let mut still_due = Vec::new();
        for table in tables {
            // Take turns between people: everyone's first picture, then everyone's second, and
            // so on, so one person adding thousands of wishes can't hold up everyone else's.
            let owner = owner_of(table);
            let due: Vec<(i64, String)> = sqlx::query_as(&format!(
                "SELECT t.id, t.image_url FROM {table} t
                  WHERE t.image_url IS NOT NULL AND t.image_blob_id IS NULL AND {}
                  ORDER BY ROW_NUMBER() OVER (PARTITION BY {owner} ORDER BY t.id), t.id
                  LIMIT ?",
                due("t.image_")
            ))
            .bind(BATCH + tried.len() as i64)
            .fetch_all(&state.db)
            .await?;
            let due: Vec<_> = due
                .into_iter()
                .filter(|(id, _)| tried.insert((table, *id)))
                .take(BATCH as usize)
                .collect();
            if due.is_empty() {
                continue;
            }
            still_due.push(table);
            // A few at a time, so one slow store doesn't hold up everyone else's pictures.
            stored += futures_util::stream::iter(due)
                .map(|(id, url)| async move { store_one(state, table, id, &url).await })
                .buffer_unordered(DOWNLOADS_AT_ONCE)
                .filter(|&ok| std::future::ready(ok))
                .count()
                .await;
        }
        tables = still_due;
    }
    // Stored pictures missing a size (its file was lost, or the size is new): download again.
    let names: Vec<String> = Variant::ALL
        .iter()
        .map(|v| format!("'{}'", v.name()))
        .collect();
    let incomplete: Vec<(i64, String)> = sqlx::query_as(&format!(
        "SELECT b.id, b.key FROM blobs b
          WHERE {}
            AND (SELECT COUNT(*) FROM blob_variants v WHERE v.blob_id = b.id AND v.name IN ({}))
                < {}
            AND (EXISTS (SELECT 1 FROM items WHERE image_blob_id = b.id)
                 OR EXISTS (SELECT 1 FROM gift_history WHERE image_blob_id = b.id))
          ORDER BY b.id",
        due("b.refetch_"),
        names.join(", "),
        Variant::ALL.len()
    ))
    .fetch_all(&state.db)
    .await?;
    for (blob_id, key) in incomplete {
        if deadline.is_some_and(|d| Instant::now() >= d) {
            return Ok((stored, true));
        }
        if refetch_one(state, blob_id, &key).await {
            stored += 1;
        }
    }
    Ok((stored, false))
}

/// Why a picture wasn't stored.
enum NotStored {
    /// The picture's (unreachable, not an image, too big): counts as a failed attempt.
    Picture,
    /// Ours (storage or database trouble): retried later without using up an attempt.
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

/// Run one attempt at something recorded by `{p}attempts`/`{p}attempted_at` on `table` row
/// `id` (only while its `image_url` is still `guard`, if given), counting it first so a download that crashes or hangs isn't
/// retried in a tight loop, and giving it back if the failure was ours.
async fn attempt<F>(
    state: &AppState,
    table: &str,
    p: &str,
    id: i64,
    guard: Option<&str>,
    work: F,
) -> bool
where
    F: std::future::Future<Output = Result<bool, NotStored>>,
{
    let extra = if guard.is_some() {
        " AND image_url = ?"
    } else {
        ""
    };
    let sql = format!(
        "UPDATE {table} SET {p}attempts = {p}attempts + 1, {p}attempted_at = datetime('now')
          WHERE id = ?{extra}"
    );
    let mut count = sqlx::query(&sql).bind(id);
    if let Some(url) = guard {
        count = count.bind(url);
    }
    let counted = count.execute(&state.db).await;
    if let Err(e) = counted {
        tracing::warn!(table, id, error = %e, "couldn't start picture download");
        return false;
    }
    match work.await {
        Ok(done) => done,
        Err(NotStored::Picture) => false,
        Err(NotStored::Ours(e)) => {
            tracing::warn!(table, id, error = %e, "couldn't store picture; will retry");
            let sql = format!(
                "UPDATE {table} SET {p}attempts = {p}attempts - 1
                  WHERE id = ?{extra} AND {p}attempts > 0"
            );
            let mut give_back = sqlx::query(&sql).bind(id);
            if let Some(url) = guard {
                give_back = give_back.bind(url);
            }
            let _ = give_back.execute(&state.db).await;
            false
        }
    }
}

/// Try to store one row's picture, returning whether it was stored. The `image_url = ?`
/// guards skip rows whose picture was changed while we worked; the new address gets its own
/// turn.
async fn store_one(state: &AppState, table: &str, id: i64, url: &str) -> bool {
    attempt(state, table, "image_", id, Some(url), async {
        // The same address is often already stored (a copied wish, a gift bought from a list).
        // (Only one with sizes to show.)
        let known: Option<i64> = sqlx::query_scalar(
            "SELECT id FROM blobs b
              WHERE (id IN (SELECT image_blob_id FROM items WHERE image_url = ?1)
                     OR id IN (SELECT image_blob_id FROM gift_history WHERE image_url = ?1))
                AND EXISTS (SELECT 1 FROM blob_variants v
                             WHERE v.blob_id = b.id AND v.missing_at IS NULL)
              LIMIT 1",
        )
        .bind(url)
        .fetch_optional(&state.db)
        .await?;
        let blob_id = match known {
            Some(id) => id,
            None => download_and_store(state, url).await?.0,
        };
        let attached = sqlx::query(&format!(
            "UPDATE {table} SET image_blob_id = ?1 WHERE id = ?2 AND image_url = ?3
                AND EXISTS (SELECT 1 FROM blob_variants WHERE blob_id = ?1)"
        ))
        .bind(blob_id)
        .bind(id)
        .bind(url)
        .execute(&state.db)
        .await?
        .rows_affected();
        Ok(attached == 1)
    })
    .await
}

/// Download a stored picture again to remake its missing sizes, trying the addresses of the
/// wishes and gifts that use it. Only an address that still serves the very same picture (same
/// bytes) counts. Nothing ever moves to a different picture: an address may now serve a
/// placeholder, or belong to someone else who happened to share these bytes, and the sizes that
/// survived are better than either.
async fn refetch_one(state: &AppState, blob_id: i64, key: &str) -> bool {
    attempt(state, "blobs", "refetch_", blob_id, None, async {
        let urls: Vec<String> = sqlx::query_scalar(
            "SELECT image_url FROM items WHERE image_blob_id = ?1 AND image_url IS NOT NULL
             UNION
             SELECT image_url FROM gift_history WHERE image_blob_id = ?1 AND image_url IS NOT NULL
             LIMIT 5",
        )
        .bind(blob_id)
        .fetch_all(&state.db)
        .await?;
        for url in urls {
            let (bytes, content_type) = match state.importer.download_image(&url).await {
                Ok(downloaded) => downloaded,
                Err(_) => continue,
            };
            if storage::content_key(&bytes) != key {
                continue;
            }
            let variants = make_variants_queued(bytes, content_type)
                .await
                .map_err(|_| NotStored::Picture)?;
            save_variants(state, blob_id, key, variants).await?;
            sqlx::query(
                "UPDATE blobs SET refetch_attempts = 0, refetch_attempted_at = NULL WHERE id = ?",
            )
            .bind(blob_id)
            .execute(&state.db)
            .await?;
            return Ok(true);
        }
        Err(NotStored::Picture)
    })
    .await
}

/// Download a picture, make its sizes and store them, returning its blob's id and key. Only
/// pictures we can make page sizes of are kept. The original itself isn't (pages never show
/// it, and it can be large): the blob records its hash, type and source.
async fn download_and_store(state: &AppState, url: &str) -> Result<(i64, String), NotStored> {
    let (bytes, content_type) = match state.importer.download_image(url).await {
        Ok(downloaded) => downloaded,
        Err(e) => {
            tracing::debug!(url, error = %e, "couldn't download picture");
            return Err(NotStored::Picture);
        }
    };
    let key = storage::content_key(&bytes);
    let size = bytes.len();
    let variants = match make_variants_queued(bytes, content_type).await {
        Ok(variants) => variants,
        Err(e) => {
            tracing::info!(url, error = %e, "not keeping picture");
            return Err(NotStored::Picture);
        }
    };
    let blob_id = storage::record_blob(&state.db, &key, content_type, size, Some(url)).await?;
    save_variants(state, blob_id, &key, variants).await?;
    Ok((blob_id, key))
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

/// Largest picture we keep: pixels per side, pixels in all (16 megapixels, plenty for a product
/// photo) and decoded bytes. Anything bigger is left hotlinked rather than stored. The pixel cap
/// is what bounds memory: some decoders need several times the output size while they work (a
/// progressive CMYK JPEG keeps every coefficient), and the byte cap only counts the output.
const MAX_SIDE: u32 = 6_000;
const MAX_PIXELS: u64 = 16_000_000;
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
    let (width, height) = decoder.dimensions();
    anyhow::ensure!(
        u64::from(width) * u64::from(height) <= MAX_PIXELS
            && decoder.total_bytes() <= MAX_DECODED_BYTES,
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
                 content_type = excluded.content_type, byte_size = excluded.byte_size,
                 missing_at = NULL",
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

/// A stored picture at the requested size, as a stream of its bytes, its content type and
/// whether it really is that size (rather than a stand-in), or `None` if there's no such picture
/// or no stored size of it can be read. Page requests never decode anything: if this size's file
/// is missing, it's left for the worker to remake and another stored size is served meanwhile
/// (the page scales it to fit).
pub async fn variant(
    state: &AppState,
    blob_key: &str,
    variant: Variant,
) -> anyhow::Result<Option<(storage::ByteStream, String, bool)>> {
    // This size first, then the others; sizes already found missing last.
    let sizes: Vec<(i64, String, String, String, bool)> = sqlx::query_as(
        "SELECT b.id, v.name, v.key, v.content_type, v.missing_at IS NOT NULL FROM blobs b
           JOIN blob_variants v ON v.blob_id = b.id
          WHERE b.key = ?
          ORDER BY v.missing_at IS NOT NULL, v.name = ? DESC",
    )
    .bind(blob_key)
    .bind(variant.name())
    .fetch_all(&state.db)
    .await?;
    for (blob_id, name, key, content_type, flagged) in sizes {
        if let Some(stream) = state.storage.get_stream(&key).await? {
            return Ok(Some((stream, content_type, name == variant.name())));
        }
        if flagged {
            continue;
        }
        // Only flag it: the worker checks before anything is forgotten (see `confirm_missing`).
        // Pages stop using a picture once all its sizes are flagged.
        sqlx::query(
            "UPDATE blob_variants SET missing_at = datetime('now')
              WHERE blob_id = ? AND name = ? AND missing_at IS NULL",
        )
        .bind(blob_id)
        .bind(&name)
        .execute(&state.db)
        .await?;
    }
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
        // Within the side limit but over the pixel limit (5,000 × 3,300 = 16.5 MP).
        let flat = image::RgbImage::from_pixel(5_000, 3_300, image::Rgb([9, 9, 9]));
        let too_many = encode(DynamicImage::ImageRgb8(flat), image::ImageFormat::Png);
        assert!(make_variants(&too_many, "image/png").is_err());
    }

    #[test]
    fn variant_names_round_trip() {
        for v in Variant::ALL {
            assert_eq!(Variant::from_name(v.name()), Some(v));
        }
        assert_eq!(Variant::from_name("huge"), None);
    }
}
