-- Pictures are downloaded and kept, rather than hotlinked from the store.
-- A blob is one stored file, named by the SHA-256 of its bytes, so a picture shared by several
-- wishes (copies, saves, gift history) is stored once. The bytes live in the configured object
-- store (local disk by default); this table only records what is there.
CREATE TABLE blobs (
    id           INTEGER PRIMARY KEY,
    key          TEXT    NOT NULL UNIQUE,
    content_type TEXT    NOT NULL,
    byte_size    INTEGER NOT NULL,
    source_url   TEXT,
    created_at   TEXT    NOT NULL DEFAULT (datetime('now'))
);

-- image_url stays the picture's original address (what the item form shows and edits).
-- image_blob_id is the downloaded copy, filled in by the background downloader; until then, or
-- if every attempt fails, pages fall back to image_url.
ALTER TABLE items ADD COLUMN image_blob_id INTEGER REFERENCES blobs(id) ON DELETE SET NULL;
ALTER TABLE items ADD COLUMN image_attempts INTEGER NOT NULL DEFAULT 0;
ALTER TABLE items ADD COLUMN image_attempted_at TEXT;
CREATE INDEX items_image_blob ON items(image_blob_id);

ALTER TABLE gift_history ADD COLUMN image_blob_id INTEGER REFERENCES blobs(id) ON DELETE SET NULL;
ALTER TABLE gift_history ADD COLUMN image_attempts INTEGER NOT NULL DEFAULT 0;
ALTER TABLE gift_history ADD COLUMN image_attempted_at TEXT;
CREATE INDEX gift_history_image_blob ON gift_history(image_blob_id);
