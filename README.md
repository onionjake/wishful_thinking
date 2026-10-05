# Wishful Thinking 🎁

Wish lists for the whole family, written in Rust with SQLite.

- **Accounts and multiple lists.** Make a list for yourself, or one per kid. Lists can have a recipient,
  an event date (with a countdown), and notes for gift-givers.
- **Families.** Anyone can start a family and create invite links. Links can expire, have a use limit,
  and be revoked. If the invitee has no account yet, they create one right from the link and join afterwards.
- **Sharing.** Each list can be shared with any of your families, and can also have an optional
  **public link** with a 144-bit random token. You can regenerate or turn off the link at any time.
- **Secret reservations.** Gift-givers tap *I'll get this* and everyone else sees the item is taken, but the
  list owner never sees it. For lists you run for someone else (like a child's), turn on
  *Show me what's been reserved*. Guests on a public link reserve with just their name.
- **Shopping list.** One page shows every gift you've promised, the total still to buy, and *Mark bought*.
- **Import from any URL.** Paste a product link and the name, price, currency, photo and store are filled in.
  An LLM can optionally help with pages that have no structured data (see below).
- Also included: priorities (*Most wanted / Would love / Nice to have*), quantities ("wants 2"), list
  co-managers (e.g. both parents), *Got it ✓* for received gifts, copying or moving items between lists,
  sorting, archiving, a browser bookmarklet, dark mode, and a mobile layout.

## Running

```sh
cargo run --release
# → http://127.0.0.1:3000
```

The database (`wishful.db`) is created and migrated automatically on startup.

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

## Security notes

- Passwords are hashed with Argon2. Session tokens are 256-bit random and stored as SHA-256 hashes.
- Cookies are `HttpOnly` and `SameSite=Lax`, and cross-origin form posts are rejected.
- The importer blocks private, loopback and link-local addresses (including after DNS resolution and
  redirects), caps response size, and times out slow sites.
- Reservation details are filtered on the server. A list owner's page never contains who reserved what
  unless the list opts in.

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
  importer/           fetch (SSRF-safe), extract (structured data), price, llm, orchestration
  templates.rs        Askama template contexts
templates/            HTML templates
static/               CSS, a little progressive-enhancement JS, favicon
migrations/           SQL schema
```
