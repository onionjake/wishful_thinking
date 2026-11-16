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

    /// The file's bytes, or `None` if it isn't there.
    pub async fn get(&self, key: &str) -> object_store::Result<Option<bytes::Bytes>> {
        match self.store.get(&self.path(key)).await {
            Ok(found) => found.bytes().await.map(Some),
            Err(object_store::Error::NotFound { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub async fn delete(&self, key: &str) -> object_store::Result<()> {
        match self.store.delete(&self.path(key)).await {
            Ok(()) | Err(object_store::Error::NotFound { .. }) => Ok(()),
            Err(e) => Err(e),
        }
    }
}

/// A key is the lowercase hex SHA-256 of the file. Anything else is rejected before it can be
/// turned into a storage path.
pub fn is_valid_key(key: &str) -> bool {
    key.len() == 64
        && key
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Store `bytes` and return the id of their blob, reusing the blob if these exact bytes are
/// already stored.
pub async fn create_blob(
    db: &Db,
    storage: &Storage,
    bytes: Vec<u8>,
    content_type: &str,
    source_url: Option<&str>,
) -> anyhow::Result<i64> {
    let key = format!("{:x}", Sha256::digest(&bytes));
    let size = bytes.len() as i64;
    // Write the file before the row, so a row always has its file behind it.
    storage.put(&key, bytes).await?;
    sqlx::query(
        "INSERT INTO blobs (key, content_type, byte_size, source_url) VALUES (?, ?, ?, ?)
         ON CONFLICT(key) DO NOTHING",
    )
    .bind(&key)
    .bind(content_type)
    .bind(size)
    .bind(source_url)
    .execute(db)
    .await?;
    Ok(sqlx::query_scalar("SELECT id FROM blobs WHERE key = ?")
        .bind(&key)
        .fetch_one(db)
        .await?)
}

/// What the image route needs to serve a blob.
pub async fn content_type(db: &Db, key: &str) -> sqlx::Result<Option<String>> {
    sqlx::query_scalar("SELECT content_type FROM blobs WHERE key = ?")
        .bind(key)
        .fetch_optional(db)
        .await
}

/// Delete blobs nothing points at any more (the wish or list was deleted, or its picture
/// changed), with their resized variants.
///
/// Only the picture worker may call this (tests aside): it is also the only thing that creates
/// blobs, so a blob it has just stored can't be swept away before it is attached.
pub async fn purge_unattached(db: &Db, storage: &Storage) -> anyhow::Result<usize> {
    const UNUSED: &str = "NOT EXISTS (SELECT 1 FROM items WHERE image_blob_id = blobs.id)
        AND NOT EXISTS (SELECT 1 FROM gift_history WHERE image_blob_id = blobs.id)";
    let candidates: Vec<(i64, String)> =
        sqlx::query_as(&format!("SELECT id, key FROM blobs WHERE {UNUSED}"))
            .fetch_all(db)
            .await?;
    let mut purged = 0;
    for (id, key) in candidates {
        let variants: Vec<String> = sqlx::query_scalar(
            "SELECT key FROM blob_variants WHERE blob_id = ? AND key IS NOT NULL",
        )
        .bind(id)
        .fetch_all(db)
        .await?;
        // Checked again in the delete itself, in case something attached it meanwhile.
        // Its variant rows go with it (ON DELETE CASCADE).
        let gone = sqlx::query(&format!("DELETE FROM blobs WHERE id = ? AND {UNUSED}"))
            .bind(id)
            .execute(db)
            .await?
            .rows_affected();
        if gone == 1 {
            for variant in variants {
                storage.delete(&variant).await?;
            }
            storage.delete(&key).await?;
            purged += 1;
        }
    }
    Ok(purged)
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
