-- The first day of the month from which the list owner may see a reservation, chosen by the
-- gift-giver (usually the month after the occasion). NULL keeps it hidden from the owner, as before.
ALTER TABLE claims ADD COLUMN reveal_on TEXT;
