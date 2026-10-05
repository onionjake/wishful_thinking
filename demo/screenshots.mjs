// Capture walkthrough screenshots of a seeded instance (see demo/seed.py).
//   NODE_PATH=$(npm root -g) node demo/screenshots.mjs http://127.0.0.1:3000 out-dir
import { createRequire } from "module";
const require = createRequire(import.meta.url);
const { chromium } = require("playwright");

const BASE = process.argv[2] || "http://127.0.0.1:3000";
const OUT = process.argv[3] || "screenshots";
const PASSWORD = "wishful-demo";
// Share links are rendered with WT_BASE_URL (if set); map them back to the local server.
const PUBLIC_BASE = process.env.PUBLIC_BASE || BASE;
const local = (u) => u.replace(PUBLIC_BASE, BASE);

const browser = await chromium.launch({ executablePath: process.env.CHROMIUM_PATH || undefined });

async function session(email, opts = {}) {
  const ctx = await browser.newContext({ viewport: { width: 1280, height: 860 }, deviceScaleFactor: 2, ...opts });
  const page = await ctx.newPage();
  if (email) {
    await page.goto(`${BASE}/login`);
    await page.fill("input[name=email]", email);
    await page.fill("input[name=password]", PASSWORD);
    await Promise.all([page.waitForNavigation(), page.click("form[action='/login'] button")]);
  }
  return page;
}

async function shot(page, name, { full = false, clip } = {}) {
  await page.waitForLoadState("networkidle");
  await page.evaluate(() => document.querySelectorAll(".flash").forEach((f) => f.remove()));
  await page.screenshot({ path: `${OUT}/${name}.png`, fullPage: full, clip });
  console.log("saved", name);
}

// Logged out
const anon = await session(null);
await anon.goto(BASE);
await shot(anon, "01-landing");
await anon.goto(`${BASE}/signup`);
await shot(anon, "02-signup");

// Sarah: dashboard, kid list, import, own list, sharing, family
const sarah = await session("sarah@example.com");
await sarah.goto(BASE);
await shot(sarah, "03-dashboard", { full: true });
await sarah.goto(`${BASE}/lists/1`);
await shot(sarah, "04-kid-list-parent-view", { full: true });

await sarah.goto(`${BASE}/lists/2/items/new`);
await sarah.fill("#import-url", "http://tinytoyshop.example/products/art-easel");
await shot(sarah, "05a-add-item-empty");
await sarah.click("form[data-import] button");
await sarah.waitForSelector("[data-import-status] .notice");
await sarah.waitForTimeout(400);
await shot(sarah, "05b-add-item-imported", { full: true });

await sarah.goto(`${BASE}/lists/3`);
await shot(sarah, "06-own-list-surprise-kept", { full: true });
await sarah.goto(`${BASE}/lists/3/share`);
await shot(sarah, "08-share-settings", { full: true });
await sarah.goto(`${BASE}/families/1`);
await shot(sarah, "09-family", { full: true });
const inviteUrl = await sarah.inputValue("input[id^=invite-]");
const publicUrl = await (async () => {
  await sarah.goto(`${BASE}/lists/3/share`);
  return sarah.inputValue("#public-url");
})();

// Grandma Jo: family member view + her shopping list
const jo = await session("jo@example.com");
await jo.goto(BASE);
await shot(jo, "07a-grandma-dashboard", { full: true });
await jo.goto(`${BASE}/lists/3`);
await shot(jo, "07b-family-member-view", { full: true });
await jo.goto(`${BASE}/reservations`);
await shot(jo, "12-shopping-list", { full: true });

// Invite link, logged out
const invited = await session(null);
await invited.goto(local(inviteUrl));
await shot(invited, "10-join-invite");

// Public link guest
const guest = await session(null);
await guest.goto(local(publicUrl));
await shot(guest, "11-public-link-guest", { full: true });

// Store page the item was imported from
const store = await session(null);
await store.goto("http://tinytoyshop.example/products/art-easel");
await shot(store, "05-store-page");

// Mobile and dark mode
const mobile = await session("jo@example.com", { viewport: { width: 390, height: 844 }, deviceScaleFactor: 3, isMobile: true, hasTouch: true });
await mobile.goto(`${BASE}/lists/1`);
await shot(mobile, "13-mobile");
const dark = await session("sarah@example.com", { colorScheme: "dark" });
await dark.goto(`${BASE}/lists/2`);
await shot(dark, "14-dark-mode");

await browser.close();
