-- Resized copies of stored pictures, made the first time a page asks for a size (see
-- src/images.rs). A NULL key means the original is already small enough (or couldn't be
-- decoded), so it is served as is. Rows go when their blob does; the purge deletes the files.
CREATE TABLE blob_variants (
    blob_id      INTEGER NOT NULL REFERENCES blobs(id) ON DELETE CASCADE,
    name         TEXT    NOT NULL,
    key          TEXT,
    content_type TEXT,
    byte_size    INTEGER,
    created_at   TEXT    NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (blob_id, name)
);
