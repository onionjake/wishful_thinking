-- Wishful Thinking initial schema.

CREATE TABLE users (
    id            INTEGER PRIMARY KEY,
    email         TEXT    NOT NULL UNIQUE COLLATE NOCASE,
    display_name  TEXT    NOT NULL,
    password_hash TEXT    NOT NULL,
    created_at    TEXT    NOT NULL DEFAULT (datetime('now'))
);

-- Session tokens are stored as SHA-256 hashes; the raw token only lives in the cookie.
CREATE TABLE sessions (
    token_hash TEXT    PRIMARY KEY,
    user_id    INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at TEXT    NOT NULL DEFAULT (datetime('now')),
    expires_at TEXT    NOT NULL
);
CREATE INDEX sessions_user ON sessions(user_id);

CREATE TABLE families (
    id          INTEGER PRIMARY KEY,
    name        TEXT    NOT NULL,
    description TEXT,
    created_by  INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at  TEXT    NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE family_members (
    family_id INTEGER NOT NULL REFERENCES families(id) ON DELETE CASCADE,
    user_id   INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    role      TEXT    NOT NULL DEFAULT 'member' CHECK (role IN ('owner', 'member')),
    joined_at TEXT    NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (family_id, user_id)
);
CREATE INDEX family_members_user ON family_members(user_id);

CREATE TABLE family_invites (
    id         INTEGER PRIMARY KEY,
    family_id  INTEGER NOT NULL REFERENCES families(id) ON DELETE CASCADE,
    token      TEXT    NOT NULL UNIQUE,
    created_by INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at TEXT    NOT NULL DEFAULT (datetime('now')),
    expires_at TEXT,
    max_uses   INTEGER,
    uses       INTEGER NOT NULL DEFAULT 0,
    revoked    INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE wishlists (
    id                   INTEGER PRIMARY KEY,
    owner_id             INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    title                TEXT    NOT NULL,
    recipient_name       TEXT,
    description          TEXT,
    event_date           TEXT,
    -- Unguessable token for the optional public link; NULL when the link is off.
    public_token         TEXT    UNIQUE,
    -- When a parent manages a child's list they usually want to see reservations;
    -- for a personal list the default keeps gifts a surprise.
    show_claims_to_owner INTEGER NOT NULL DEFAULT 0,
    archived             INTEGER NOT NULL DEFAULT 0,
    created_at           TEXT    NOT NULL DEFAULT (datetime('now')),
    updated_at           TEXT    NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX wishlists_owner ON wishlists(owner_id);

CREATE TABLE wishlist_families (
    wishlist_id INTEGER NOT NULL REFERENCES wishlists(id) ON DELETE CASCADE,
    family_id   INTEGER NOT NULL REFERENCES families(id) ON DELETE CASCADE,
    PRIMARY KEY (wishlist_id, family_id)
);
CREATE INDEX wishlist_families_family ON wishlist_families(family_id);

-- Co-managers can edit a list as if they owned it (e.g. both parents).
CREATE TABLE wishlist_managers (
    wishlist_id INTEGER NOT NULL REFERENCES wishlists(id) ON DELETE CASCADE,
    user_id     INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    PRIMARY KEY (wishlist_id, user_id)
);

CREATE TABLE items (
    id            INTEGER PRIMARY KEY,
    wishlist_id   INTEGER NOT NULL REFERENCES wishlists(id) ON DELETE CASCADE,
    title         TEXT    NOT NULL,
    url           TEXT,
    image_url     TEXT,
    price_cents   INTEGER,
    currency      TEXT    NOT NULL DEFAULT 'USD',
    store         TEXT,
    notes         TEXT,
    -- 1 = most wanted, 2 = would love, 3 = nice to have
    priority      INTEGER NOT NULL DEFAULT 2 CHECK (priority BETWEEN 1 AND 3),
    quantity      INTEGER NOT NULL DEFAULT 1 CHECK (quantity >= 1),
    received      INTEGER NOT NULL DEFAULT 0,
    import_source TEXT,
    created_at    TEXT    NOT NULL DEFAULT (datetime('now')),
    updated_at    TEXT    NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX items_wishlist ON items(wishlist_id);

-- A reservation ("I'll get this"). Either a registered user or a guest from the public link.
CREATE TABLE claims (
    id          INTEGER PRIMARY KEY,
    item_id     INTEGER NOT NULL REFERENCES items(id) ON DELETE CASCADE,
    user_id     INTEGER REFERENCES users(id) ON DELETE CASCADE,
    guest_name  TEXT,
    guest_token TEXT,
    quantity    INTEGER NOT NULL DEFAULT 1 CHECK (quantity >= 1),
    purchased   INTEGER NOT NULL DEFAULT 0,
    created_at  TEXT    NOT NULL DEFAULT (datetime('now')),
    CHECK (user_id IS NOT NULL OR guest_name IS NOT NULL)
);
CREATE INDEX claims_item ON claims(item_id);
CREATE INDEX claims_user ON claims(user_id);
