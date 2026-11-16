//! Stored files, in the spirit of Rails' ActiveStorage.
//!
//! Bytes go to an [`object_store`] backend picked by URL (`WT_STORAGE_URL`), so moving from local
//! disk to S3, GCS or Azure later means enabling that `object_store` feature and changing the URL,
//! not changing any code that stores or serves files. The `blobs` table records what has been
//! stored; rows elsewhere (items, gift history) point at a blob by id.
//!
//! Blobs are content-addressed: the key is the SHA-256 of the bytes, so the same picture is stored
//! once however many wishes use it, and a stored file never changes (it can be cached forever).

use std::sync::Arc;

use object_store::path::Path;
use object_store::{ObjectStore, ObjectStoreExt, PutPayload};
use sha2::{Digest, Sha256};
use url::Url;

use crate::db::Db;

/// A stored file's contents, read a piece at a time.
pub type ByteStream = futures_util::stream::BoxStream<'static, object_store::Result<bytes::Bytes>>;

#[derive(Clone)]
pub struct Storage {
    store: Arc<dyn ObjectStore>,
    prefix: Path,
    description: String,
}

impl std::fmt::Debug for Storage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.description)
    }
}

impl Storage {
    /// Open the store named by `raw`: an `object_store` URL such as `file:///srv/data/storage`,
    /// `memory:///` or (with the matching crate feature) `s3://bucket/prefix`, or a plain
    /// directory path. Local directories are created if missing.
    pub fn open(raw: &str) -> anyhow::Result<Self> {
        let url = if raw.contains("://") {
            Url::parse(raw)?
        } else {
            std::fs::create_dir_all(raw)?;
            let abs = std::fs::canonicalize(raw)?;
            Url::from_directory_path(&abs)
                .map_err(|_| anyhow::anyhow!("bad storage directory {}", abs.display()))?
        };
        if url.scheme() == "file" {
            let dir = url
                .to_file_path()
                .map_err(|_| anyhow::anyhow!("bad storage URL {url}"))?;
            std::fs::create_dir_all(&dir)?;
        }
        let (store, prefix) = object_store::parse_url(&url)
            .map_err(|e| anyhow::anyhow!("can't open storage {url}: {e}"))?;
        Ok(Storage {
            store: Arc::from(store),
            prefix,
            description: url.to_string(),
        })
    }

    /// Spread files over two directory levels (`ab/cd/abcd…`) so no directory gets huge.
    fn path(&self, key: &str) -> Path {
        let mut path = self.prefix.clone();
        for part in [&key[..2], &key[2..4], key] {
            path = path.join(part);
        }
        path
    }

    pub async fn put(&self, key: &str, bytes: Vec<u8>) -> object_store::Result<()> {
        self.store
            .put(&self.path(key), PutPayload::from(bytes))
            .await
            .map(|_| ())
    }

    /// The file's bytes as a stream, or `None` if it isn't there. Streaming means a slow reader
    /// holds a small buffer, not the whole file.
    pub async fn get_stream(&self, key: &str) -> object_store::Result<Option<ByteStream>> {
        match self.store.get(&self.path(key)).await {
            Ok(found) => Ok(Some(found.into_stream())),
            Err(object_store::Error::NotFound { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// The file's bytes, or `None` if it isn't there.
    pub async fn get(&self, key: &str) -> object_store::Result<Option<bytes::Bytes>> {
        match self.store.get(&self.path(key)).await {
            Ok(found) => found.bytes().await.map(Some),
            Err(object_store::Error::NotFound { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub async fn exists(&self, key: &str) -> object_store::Result<bool> {
        match self.store.head(&self.path(key)).await {
            Ok(_) => Ok(true),
            Err(object_store::Error::NotFound { .. }) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// The store's identity marker (see [`check_identity`]).
    fn marker(&self) -> Path {
        self.prefix.clone().join(MARKER)
    }

    pub async fn delete(&self, key: &str) -> object_store::Result<()> {
        match self.store.delete(&self.path(key)).await {
            Ok(()) | Err(object_store::Error::NotFound { .. }) => Ok(()),
            Err(e) => Err(e),
        }
    }
}

/// The file naming the store, so the app can tell it's looking at the store its database
/// describes. Not a valid key, so the sweep never touches it.
const MARKER: &str = "wishful-thinking-store";

/// Whether the store is the one the database describes: both hold the same random id. Nothing
/// that writes or deletes stored files may run otherwise. A wrong `WT_STORAGE_URL` or an
/// unmounted disk (the store lacks the marker), or a new or wrong database pointed at a real
/// store (the database lacks it), would otherwise make the app forget pictures, store new ones
/// in the wrong place, or sweep away files it doesn't know about. A first run (neither has it)
/// pairs them.
pub async fn check_identity(db: &Db, storage: &Storage) -> anyhow::Result<bool> {
    let in_db: Option<String> = sqlx::query_scalar("SELECT id FROM storage_identity")
        .fetch_optional(db)
        .await?;
    let in_store = match storage.store.get(&storage.marker()).await {
        Ok(found) => Some(
            String::from_utf8_lossy(&found.bytes().await?)
                .trim()
                .to_string(),
        ),
        Err(object_store::Error::NotFound { .. }) => None,
        Err(e) => return Err(e.into()),
    };
    match (in_db, in_store) {
        (Some(a), Some(b)) => Ok(a == b),
        (None, None) => {
            pair(db, storage).await?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

/// Give the database and the store a fresh shared id, so they're recognised as a pair.
pub async fn pair(db: &Db, storage: &Storage) -> anyhow::Result<()> {
    use rand::Rng;
    let id: String = rand::thread_rng()
        .sample_iter(&rand::distributions::Alphanumeric)
        .take(32)
        .map(char::from)
        .collect();
    storage
        .store
        .put(&storage.marker(), PutPayload::from(id.clone().into_bytes()))
        .await?;
    let mut tx = db.begin().await?;
    sqlx::query("DELETE FROM storage_identity")
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO storage_identity (id) VALUES (?)")
        .bind(&id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// A key is the lowercase hex SHA-256 of the file. Anything else is rejected before it can be
/// turned into a storage path.
pub fn is_valid_key(key: &str) -> bool {
    key.len() == 64
        && key
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The key for a file: the lowercase hex SHA-256 of its bytes.
pub fn content_key(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Record a blob and return its id, reusing the row if this key is already recorded. Files made
/// from it are stored by the caller under keys starting with `key`.
pub async fn record_blob(
    db: &Db,
    key: &str,
    content_type: &str,
    byte_size: usize,
    source_url: Option<&str>,
) -> sqlx::Result<i64> {
    sqlx::query(
        "INSERT INTO blobs (key, content_type, byte_size, source_url) VALUES (?, ?, ?, ?)
         ON CONFLICT(key) DO NOTHING",
    )
    .bind(key)
    .bind(content_type)
    .bind(byte_size as i64)
    .bind(source_url)
    .execute(db)
    .await?;
    sqlx::query_scalar("SELECT id FROM blobs WHERE key = ?")
        .bind(key)
        .fetch_one(db)
        .await
}

/// Delete blobs nothing points at any more (the wish or list was deleted, or its picture
/// changed), with their files. `derived_keys` names files made from a blob (its resized
/// variants), which are deleted whether or not anything recorded them.
///
/// Only the picture worker may call this (tests aside): it is also the only thing that creates
/// blobs, so a blob it has just stored can't be swept away before it is attached. A file that
/// can't be deleted right now is left for [`sweep_orphans`].
pub async fn purge_unattached(
    db: &Db,
    storage: &Storage,
    derived_keys: impl Fn(&str) -> Vec<String>,
) -> anyhow::Result<usize> {
    const UNUSED: &str = "NOT EXISTS (SELECT 1 FROM items WHERE image_blob_id = blobs.id)
        AND NOT EXISTS (SELECT 1 FROM gift_history WHERE image_blob_id = blobs.id)";
    let candidates: Vec<(i64, String)> =
        sqlx::query_as(&format!("SELECT id, key FROM blobs WHERE {UNUSED}"))
            .fetch_all(db)
            .await?;
    let mut purged = 0;
    for (id, key) in candidates {
        // Checked again in the delete itself, in case something attached it meanwhile.
        // Its variant rows go with it (ON DELETE CASCADE).
        let gone = sqlx::query(&format!("DELETE FROM blobs WHERE id = ? AND {UNUSED}"))
            .bind(id)
            .execute(db)
            .await?
            .rows_affected();
        if gone == 1 {
            for file in derived_keys(&key).iter().chain([&key]) {
                if let Err(e) = storage.delete(file).await {
                    tracing::warn!(file, error = %e, "couldn't delete stored file; the sweep will retry");
                }
            }
            purged += 1;
        }
    }
    Ok(purged)
}

/// Delete stored files whose blob no longer exists: left by a failed delete, a crash between
/// storing a file and recording it, or a resized copy finished just after its blob was purged.
/// Files are named `<blob key>` or `<blob key>-<variant>`. Files newer than `min_age` are
/// skipped, as their blob row may be about to be written.
pub async fn sweep_orphans(
    db: &Db,
    storage: &Storage,
    min_age: std::time::Duration,
) -> anyhow::Result<usize> {
    use futures_util::TryStreamExt;
    let cutoff = time::OffsetDateTime::now_utc().unix_timestamp() - min_age.as_secs() as i64;
    let mut files = storage.store.list(Some(&storage.prefix));
    let mut swept = 0;
    while let Some(meta) = files.try_next().await? {
        let Some(name) = meta.location.filename() else {
            continue;
        };
        let blob_key = name.get(..64).unwrap_or(name);
        if !is_valid_key(blob_key) || meta.last_modified.timestamp() > cutoff {
            continue;
        }
        // A half-written file left by a crash (local storage writes `<name>#<n>`, then renames).
        if name.contains('#') {
            match storage.store.delete(&meta.location).await {
                Ok(()) => swept += 1,
                Err(e) => {
                    tracing::warn!(file = %meta.location, error = %e, "couldn't delete stray file")
                }
            }
            continue;
        }
        let known: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM blobs WHERE key = ?)")
            .bind(blob_key)
            .fetch_one(db)
            .await?;
        if !known {
            match storage.store.delete(&meta.location).await {
                Ok(()) => swept += 1,
                Err(e) => {
                    tracing::warn!(file = %meta.location, error = %e, "couldn't delete stray file")
                }
            }
        }
    }
    Ok(swept)
}

/// The URL pages use for a stored image.
pub fn image_path(key: &str) -> String {
    format!("/images/{key}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn round_trip_on_disk() {
        let dir = std::env::temp_dir().join(format!("wt-storage-{}", std::process::id()));
        let storage = Storage::open(dir.to_str().unwrap()).unwrap();
        let key = format!("{:x}", Sha256::digest(b"hello"));
        storage.put(&key, b"hello".to_vec()).await.unwrap();
        assert!(dir.join(&key[..2]).join(&key[2..4]).join(&key).is_file());
        assert_eq!(storage.get(&key).await.unwrap().unwrap().as_ref(), b"hello");
        storage.delete(&key).await.unwrap();
        assert!(storage.get(&key).await.unwrap().is_none());
        storage.delete(&key).await.unwrap();
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn keys_are_hex_digests() {
        assert!(is_valid_key(&"a1".repeat(32)));
        assert!(!is_valid_key("../../etc/passwd"));
        assert!(!is_valid_key(&"A1".repeat(32)));
        assert!(!is_valid_key("abc"));
    }
}
