-- Admins can see every account and act on anyone's behalf (see src/routes/admin.rs). The flag is
-- only ever set by hand in the database: nothing in the app writes it.
ALTER TABLE users ADD COLUMN is_admin INTEGER NOT NULL DEFAULT 0;

-- The account an admin's session is currently acting as. Only honoured while the session's own
-- user is still an admin, so clearing is_admin ends it at once.
ALTER TABLE sessions ADD COLUMN acting_as INTEGER REFERENCES users(id) ON DELETE SET NULL;

-- Which list each bought gift was on, so an admin acting as the buyer still doesn't see gifts
-- meant for the admin after the wish's reservation is gone (wish deleted or moved, list
-- deleted). A plain id with no foreign key: it outlives the list on purpose.
-- SQLite can give a deleted list's id to a new one, so the list's created_at is kept as well
-- and both must match.
ALTER TABLE gift_history ADD COLUMN wishlist_id INTEGER;
ALTER TABLE gift_history ADD COLUMN wishlist_created_at TEXT;
UPDATE gift_history
   SET (wishlist_id, wishlist_created_at) =
       (SELECT w.id, w.created_at FROM claims c JOIN items i ON i.id = c.item_id
          JOIN wishlists w ON w.id = i.wishlist_id
         WHERE c.id = gift_history.claim_id);

-- Taking the flag away (by hand, in the database) also forgets who their sessions were acting
-- as, so setting it again later doesn't resume acting as someone.
CREATE TRIGGER users_admin_revoked AFTER UPDATE OF is_admin ON users
WHEN NEW.is_admin = 0
BEGIN
    UPDATE sessions SET acting_as = NULL WHERE user_id = NEW.id;
END;
