-- The account a list is for (e.g. a child with their own login). That person never sees who
-- reserved what, even if they co-manage the list or see it through a family, until each giver's
-- reveal month. NULL means the list isn't tied to anyone's account.
ALTER TABLE wishlists ADD COLUMN recipient_user_id INTEGER REFERENCES users(id) ON DELETE SET NULL;
