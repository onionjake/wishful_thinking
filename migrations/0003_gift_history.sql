-- What each person has bought, and for whom. Rows copy the gift's details rather than pointing
-- at them, so history survives the item or list being edited or deleted by its owner.
CREATE TABLE gift_history (
    id           INTEGER PRIMARY KEY,
    buyer_id     INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    claim_id     INTEGER UNIQUE REFERENCES claims(id) ON DELETE SET NULL,
    title        TEXT    NOT NULL,
    url          TEXT,
    image_url    TEXT,
    price_cents  INTEGER,
    currency     TEXT    NOT NULL DEFAULT 'USD',
    store        TEXT,
    quantity     INTEGER NOT NULL DEFAULT 1,
    recipient    TEXT    NOT NULL,
    occasion     TEXT    NOT NULL,
    event_date   TEXT,
    purchased_at TEXT    NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX gift_history_buyer ON gift_history(buyer_id);

-- Gifts already marked bought. We never recorded when, so the reservation date stands in.
INSERT INTO gift_history (buyer_id, claim_id, title, url, image_url, price_cents, currency, store,
                          quantity, recipient, occasion, event_date, purchased_at)
SELECT c.user_id, c.id, i.title, i.url, i.image_url, i.price_cents, i.currency, i.store,
       c.quantity, COALESCE(w.recipient_name, u.display_name), w.title, w.event_date, c.created_at
  FROM claims c
  JOIN items i ON i.id = c.item_id
  JOIN wishlists w ON w.id = i.wishlist_id
  JOIN users u ON u.id = w.owner_id
 WHERE c.purchased = 1 AND c.user_id IS NOT NULL;
