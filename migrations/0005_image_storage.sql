-- Pictures are downloaded and kept, rather than hotlinked from the store.
-- A blob is one downloaded picture, named by the SHA-256 of its bytes, so a picture shared by
-- several wishes (copies, saves, gift history) is stored once. Only its resized copies are kept
-- (blob_variants), in the configured object store (local disk by default); content_type and
-- byte_size describe the original as downloaded.
-- AUTOINCREMENT so a purged blob's id is never reused: a stale id held elsewhere can then only
-- miss, never point at a different picture.
CREATE TABLE blobs (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    key          TEXT    NOT NULL UNIQUE,
    content_type TEXT    NOT NULL,
    byte_size    INTEGER NOT NULL,
    -- Where it was first downloaded from. Informational only: re-downloads use each wish's own
    -- address, since wishes sharing a blob (identical bytes) may have come from different places.
    source_url   TEXT,
    -- Downloading the picture again to remake a size that's missing (see src/images.rs).
    refetch_attempts   INTEGER NOT NULL DEFAULT 0,
    refetch_attempted_at TEXT,
    created_at   TEXT    NOT NULL DEFAULT (datetime('now'))
);

-- image_url stays the picture's original address (what the item form shows and edits).
-- image_blob_id is the downloaded copy, filled in by the background downloader; until then, or
-- if every attempt fails, pages fall back to image_url. The attempts are spaced 1, 2, 4… hours.
ALTER TABLE items ADD COLUMN image_blob_id INTEGER REFERENCES blobs(id) ON DELETE SET NULL;
ALTER TABLE items ADD COLUMN image_attempts INTEGER NOT NULL DEFAULT 0;
ALTER TABLE items ADD COLUMN image_attempted_at TEXT;
CREATE INDEX items_image_blob ON items(image_blob_id);
CREATE INDEX items_image_url ON items(image_url);
-- The downloader's queue: only rows still waiting for a picture.
CREATE INDEX items_image_due ON items(id) WHERE image_url IS NOT NULL AND image_blob_id IS NULL;

ALTER TABLE gift_history ADD COLUMN image_blob_id INTEGER REFERENCES blobs(id) ON DELETE SET NULL;
ALTER TABLE gift_history ADD COLUMN image_attempts INTEGER NOT NULL DEFAULT 0;
ALTER TABLE gift_history ADD COLUMN image_attempted_at TEXT;
CREATE INDEX gift_history_image_blob ON gift_history(image_blob_id);
CREATE INDEX gift_history_image_url ON gift_history(image_url);
CREATE INDEX gift_history_image_due ON gift_history(id)
    WHERE image_url IS NOT NULL AND image_blob_id IS NULL;
