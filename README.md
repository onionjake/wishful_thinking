# Wishful Thinking 🎁

Wish lists for the whole family, written in Rust with SQLite.

- **Accounts and multiple lists.** Make a list for yourself, or one per kid. Lists can have a recipient,
  an event date (with a countdown), and notes for gift-givers.
- **Families.** Anyone can start a family and create invite links. Links can expire, have a use limit,
  and be revoked. If the invitee has no account yet, they create one right from the link and join afterwards.
- **Sharing.** Each list can be shared with any of your families, and can also have an optional
  **public link** with a 144-bit random token. You can regenerate or turn off the link at any time.
- **Secret reservations.** Gift-givers tap *I'll get this* and everyone else sees the item is taken, but the
  list owner doesn't see it. When reserving, the giver picks the month the owner may find out (the
  month after the occasion is suggested, e.g. January for Christmas), so months later the owner can see
  what they were given and by whom. For lists you run for someone else (like a child's), turn on
  *Show me what's been reserved*. Each co-manager has their own setting too, so you can make a
  child's list now and, once they have an account, add them as a co-manager with reservations
  hidden: they can add wishes without seeing who's getting what, while you and the other parent can. Guests on a public link reserve with just their name.
- **Shopping list.** One page shows every gift you've promised, the total still to buy, and *Mark bought*.
- **Gift history.** Everything you've marked bought, grouped by who it was for and searchable, so you can
  look up who a gift is for. It keeps its own copy of each gift, so it survives the list being changed or deleted.
- **Import from any URL.** Paste a product link and the name, price, currency, photo and store are filled in.
  An LLM can optionally help with pages that have no structured data (see below).
- **Add-to-list bookmarklet.** From the Account page, add a bookmark that reads the product from the
  page you're looking at. Because it runs in your own browser, it works for stores like Walmart that
  show our server a robot check instead of the product.
- **Import an Amazon wish list.** Paste the link to a public or shared list and every item is copied over,
  including its comment, priority and quantity. Importing again only adds what's new.
- Also included: priorities (*Most wanted / Would love / Nice to have*), quantities ("wants 2"), list
  co-managers (e.g. both parents, or the child themselves), *Got it ✓* for received gifts, copying or moving items between lists,
  *Copy to one of my own lists* (in a wish's ⋯ menu) to save something you spot on someone else's list,
  sorting, archiving, dark mode, and a mobile layout. It can be installed as an app
  (*Add to Home Screen* on phones, *Install* in desktop Chrome/Edge); it still needs a connection to work.

## Running

```sh
cargo run --release
# → http://127.0.0.1:3000
```

The database (`wishful.db`) is created and migrated automatically on startup.

The app sends no email, so there is no "forgot password" link. To let someone back in, give their
account a new random password and pass it on; they can change it on their account page. This also signs
them out everywhere:

```sh
wishful_thinking reset-password sarah@example.com
# with the same WT_DATABASE_URL the server uses, e.g. in production:
WT_DATABASE_URL=sqlite:///srv/wishful/data/wishful.db /srv/wishful/wishful_thinking reset-password sarah@example.com
```

### Configuration

| Variable | Default | Purpose |
|---|---|---|
| `WT_BIND` | `127.0.0.1:3000` | Listen address |
| `WT_DATABASE_URL` | `sqlite://wishful.db` | SQLite database |
| `WT_BASE_URL` | from `Host` header | Absolute URL used in invite and public links, e.g. `https://wishes.example.com` |
| `WT_SECURE_COOKIES` | off | Set to `1` when serving over HTTPS |
| `WT_LLM_PROVIDER` | `anthropic` if `ANTHROPIC_API_KEY` is set, otherwise none | `anthropic`, `openai` or `none` |
| `WT_LLM_MODEL` | `claude-opus-5-5` for Anthropic | Model name |
| `WT_LLM_API_KEY` | `ANTHROPIC_API_KEY` / `OPENAI_API_KEY` | API key |
| `WT_LLM_BASE_URL` | provider default | Point at a proxy or a local server |
| `WT_LLM_MODE` | `fallback` | `off`, `fallback` (only when structured data is incomplete) or `always` |
| `WT_STORAGE_URL` | `storage/` next to the database | Where downloaded pictures are kept: a directory, `file:///path`, or `memory:///` (see *Stored pictures*) |
| `WT_ALLOW_PRIVATE_FETCH` | off | Let the importer fetch private/loopback addresses. Development only |

## How URL import works

Product pages vary wildly, so the importer (`src/importer/`) tries sources from most to least reliable:

1. **schema.org JSON-LD** `Product` / `ProductGroup`, including `@graph`, offer arrays, `AggregateOffer`,
   and variants.
2. **Microdata** (`itemprop="price"` …).
3. **OpenGraph / Twitter / `product:price:*`** meta tags.
4. **Known store layouts** (Amazon's `#productTitle` and dynamic images, common Shopify themes) and the
   **Shopify product JSON** endpoint for stores that render prices client-side.
5. **Generic fallbacks**: `<h1>`, `<title>`, `image_src`, the first large image, and finally a name
   guessed from the URL slug.

Prices are parsed in many formats (`$1,299.99`, `1.299,00 €`, `¥3,980`, `1 299,95 kr`), Shopify image URLs
are upgraded to full size, and page titles are cleaned up (`Amazon.com: Foo : Toys & Games` → `Foo`).

The importer also detects anti-bot pages (Walmart, Amazon captcha, Cloudflare) and redirects to unrelated
pages. In those cases it warns the user instead of saving a "Robot or human?" item.

### Pluggable LLM assistance

If a provider is configured, the importer builds a compact digest of the page (meta tags, JSON-LD,
candidate images, visible text). It sends the digest to the model with a JSON schema and merges the result:

- **Structured data always wins.** The model only replaces generic guesses and fills gaps. It never
  overrides a JSON-LD price.
- The model can only pick an image URL that actually appears on the page, so it can't invent one.
- LLM failures become warnings. The import still succeeds with whatever the heuristics found.

Two providers ship:

```sh
# Claude (Messages API, structured outputs)
WT_LLM_PROVIDER=anthropic ANTHROPIC_API_KEY=sk-ant-... cargo run --release

# Any OpenAI-compatible server: OpenAI, OpenRouter, Ollama, LM Studio, vLLM, llama.cpp …
WT_LLM_PROVIDER=openai WT_LLM_BASE_URL=http://localhost:11434/v1 WT_LLM_MODEL=llama3.2 cargo run --release
```

To add another provider, implement the `LlmProvider` trait in `src/importer/llm.rs` (one method,
`complete_json`).

Try the importer from the command line:

```sh
cargo run --example import -- https://www.allbirds.com/products/mens-tree-runners
```

## Stored pictures

Wish pictures are downloaded and served from the app instead of being hotlinked from the store,
so they keep working when a store changes its links and viewers' browsers don't contact the
store. It works much like Rails' ActiveStorage:

- Saving a wish stores the picture's original address (`image_url`) straight away. A background
  worker then downloads it (4 at a time, 10 s each, up to 10 MB, through the same SSRF-safe client
  as the importer, taking turns between people so no one can hold up everyone else) and checks by
  its bytes that it's a JPEG, PNG, GIF or WebP. Until it's stored, or if all 6 attempts fail (waiting 1, 2,
  4, 8 and 16 hours between them, so outages don't use them up), pages show the original
  address. Problems on our side (disk, database) don't use up attempts; they're retried an hour later.
- The worker decodes the picture (one at a time, applying EXIF rotation) and stores the two sizes
  pages use: `/images/<key>/thumb` (fits 160px, for rows) and `/images/<key>/card` (fits 480px,
  for item cards and list previews). Pictures with transparency stay PNG; the rest become JPEG,
  unless the original is already smaller. **The full original isn't kept**: pages never show it,
  and keeping only small sizes bounds what each picture can cost on disk. The `blobs` table
  records the original's hash, type and source address; `blob_variants` records the sizes.
- Pictures it can't decode, or that are over 6,000 px a side or 16 megapixels, aren't kept: the
  wish keeps showing the original address. (Decoding a picture at the limit can briefly take a
  couple of hundred MB; consider a `MemoryMax=` in the systemd unit on a small server.)
- Blobs are named by the SHA-256 of the original, so a picture shared by copies, saves and gift
  history is stored once and can be cached forever. Files are streamed, not loaded whole.
- Deleting a wish or list, or changing a wish's picture, wakes the worker, which deletes pictures
  no wish or gift history uses any more, with all their sizes. It also checks hourly, and hourly
  sweeps away stray files with no blob behind them (from a crash or a failed delete). Gift
  history keeps its picture after the wish is gone.
- If a stored size's file goes missing (say, the database was restored without `storage/`),
  another stored size of the picture is served in its place, and the worker downloads the
  picture again from the wishes' own addresses to remake it, but only if an address still serves
  the very same picture. Nothing ever moves to a different picture (a dead product link often
  serves a placeholder), so the sizes that survived keep being served. Page requests never decode
  anything and never redirect elsewhere.
- Pictures existing before this feature are picked up by the same worker on startup.

To add a size, add a `Variant` in `src/images.rs`. The worker downloads stored pictures again to
make it; until then, or where the store no longer has the picture, another size stands in.

The bytes go through the [`object_store`](https://docs.rs/object_store) crate, which also has S3,
Google Cloud Storage and Azure backends. To move to one, enable its feature in `Cargo.toml` (e.g.
`object_store = { version = "0.14", features = ["aws"] }`), set `WT_STORAGE_URL=s3://bucket/prefix`
and copy the existing `storage/` directory into the bucket. No other code changes are needed.
Back up `storage/` together with the database.

## Security notes

- Passwords are hashed with Argon2. Session tokens are 256-bit random and stored as SHA-256 hashes.
- Cookies are `HttpOnly` and `SameSite=Lax`, and cross-origin form posts are rejected.
- The importer blocks private, loopback and link-local addresses (including after DNS resolution and
  redirects), caps response size, and times out slow sites.
- Stored pictures are checked by their content type (SVG and anything that isn't a known raster
  format is refused), decoded within size limits before they're kept, and served with `nosniff`
  and a sandboxing CSP.
- The importer and picture downloader ignore `HTTP(S)_PROXY`: a proxy would resolve addresses
  itself and get round the check that refuses private ones. A deployment that can only reach the
  internet through a proxy can't import from URLs.
- There is no limit on how many wishes (and so pictures) an account can add. Each stored picture
  costs well under 1 MB (its two sizes); if that matters for an open-signup server, add a quota.
- `/images/…` needs no sign-in: these are product photos, and public lists show them to anyone.
  Their addresses are content hashes, so someone who already has a picture's exact bytes could
  check whether it's stored here, but not which list or person it belongs to.
- Reservation details are filtered on the server. A list owner's page never contains who reserved what
  unless the list opts in or the giver's chosen reveal month has arrived.

## Development

```sh
cargo test            # unit tests + end-to-end HTTP tests + importer tests against fake LLM servers
```

`demo/` has a fake shop (`store.py`), a seeding script (`seed.py`) that builds a demo family through the
app's own endpoints, and the Playwright script (`screenshots.mjs`) used for the walkthrough screenshots.

Project layout:

```
src/
  main.rs, lib.rs     startup, AppState
  config.rs           environment configuration
  db.rs               SQLite pool, models, shared queries, access control
  auth.rs             passwords, sessions, flash messages, CSRF check
  routes/             account, lists, items (+ reservations, import API), families, pages
  importer/           fetch (SSRF-safe), extract (structured data), price, llm, amazon_list, orchestration
  storage.rs          blob storage over object_store (local disk now, cloud later)
  images.rs           background worker that downloads and keeps wish pictures
  templates.rs        Askama template contexts
templates/            HTML templates
static/               CSS, a little progressive-enhancement JS, favicon, app icons, web app manifest, service worker
migrations/           SQL schema
```
