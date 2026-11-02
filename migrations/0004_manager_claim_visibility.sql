-- Whether each co-manager sees reservations, so a parent can see them while the child the list
-- is for (also a co-manager) can't. The owner's own choice stays in wishlists.show_claims_to_owner.
-- Existing co-managers keep what they saw before: the list's setting.
ALTER TABLE wishlist_managers ADD COLUMN sees_claims INTEGER NOT NULL DEFAULT 0;
UPDATE wishlist_managers
   SET sees_claims = (SELECT w.show_claims_to_owner FROM wishlists w WHERE w.id = wishlist_id);
