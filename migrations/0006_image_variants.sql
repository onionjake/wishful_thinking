-- The sizes pages show a stored picture at (see src/images.rs), made when it's stored. Each is
-- a file named `<blob key>-<name>`. Rows go when their blob does; the purge deletes the files.
CREATE TABLE blob_variants (
    blob_id      INTEGER NOT NULL REFERENCES blobs(id) ON DELETE CASCADE,
    name         TEXT    NOT NULL,
    key          TEXT    NOT NULL,
    content_type TEXT    NOT NULL,
    byte_size    INTEGER NOT NULL,
    created_at   TEXT    NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (blob_id, name)
);
