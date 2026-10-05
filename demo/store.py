#!/usr/bin/env python3
"""A tiny fake web shop for demos and screenshots.

Serves product pages with schema.org JSON-LD (so the importer can read them) and
simple SVG product pictures. Pick the shop by Host header:

    sudo python3 demo/store.py 80     # with tinytoyshop.example / cozyhome.example in /etc/hosts
"""
import html
import json
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PRODUCTS = {
    # slug: (name, price, emoji, background, description)
    "robot-kit": ("Build-a-Bot Coding Robot Kit", "49.99", "🤖", "#dcecff", "Snap-together robot that kids program with picture cards. Ages 6+."),
    "rainbow-stacker": ("Wooden Rainbow Stacker, 7 pieces", "38.50", "🌈", "#ffe3d6", "Hand-finished beech wood rainbow for open-ended play."),
    "art-easel": ("Double-Sided Art Easel", "64.00", "🎨", "#fff1c2", "Chalkboard on one side, whiteboard on the other, paper roll on top."),
    "bike-helmet": ("Kids Bike Helmet — Starry Night", "34.95", "⛑️", "#e3f4ea", "Lightweight, adjustable dial fit, ages 5–8."),
    "story-books": ("Bedtime Stories Picture Book Set", "32.50", "📚", "#f1e4ff", "Five hardcover picture books in a keepsake box."),
    "telescope": ("First Telescope for Young Astronomers", "89.00", "🔭", "#d9e6ff", "70mm refractor with tabletop tripod and moon map."),
    "dino-set": ("Jurassic Dinosaur Figure Set (12)", "27.99", "🦕", "#e5f6d9", "Twelve hand-painted dinosaurs with a fact card for each."),
    "train-set": ("Wooden Railway Starter Set", "74.00", "🚂", "#ffe0e0", "52-piece figure-eight track with a bridge and two engines."),
    "plush-fox": ("Snuggly Fox Plush, Large", "24.00", "🦊", "#ffe8cc", "Extra-soft 18\" fox. Machine washable."),
    "scooter": ("3-Wheel Kick Scooter with Light-Up Wheels", "59.99", "🛴", "#dff7f7", "Lean-to-steer, adjustable handlebar, ages 3–8."),
}
HOME = {
    "knit-blanket": ("Chunky Knit Throw Blanket, Oatmeal", "89.00", "🧶", "#f3ead9", "Hand-knit from soft recycled cotton. 50\" × 60\"."),
    "pour-over": ("Ceramic Pour-Over Coffee Set", "42.00", "☕", "#efe3d6", "Dripper, carafe and two cups in speckled stoneware."),
    "planter": ("Self-Watering Ceramic Planter, Large", "36.00", "🪴", "#e3f1e0", "Hidden reservoir keeps plants happy for two weeks."),
    "novel": ("The Lighthouse Keeper's Daughter (Hardcover)", "27.00", "📖", "#e6e9f5", "The book-club favourite everyone is talking about."),
    "running-shoes": ("Cloudstride Running Shoes, Women's", "130.00", "👟", "#ffe5ec", "Cushioned daily trainer. Size 8, please!"),
    "candle": ("Cedar & Sage Soy Candle", "22.00", "🕯️", "#f5efe0", "60-hour burn, cotton wick, reusable glass jar."),
    "headphones": ("Wireless Noise-Cancelling Headphones", "199.00", "🎧", "#e4e4ea", "30-hour battery, folds flat for travel."),
    "cast-iron": ("Enameled Cast Iron Dutch Oven, 5.5 qt", "110.00", "🍲", "#ffe1d9", "Perfect for bread, braises and soups."),
}
SHOPS = {
    "tinytoyshop.example": ("Tiny Toy Shop", PRODUCTS, "#e2554b"),
    "cozyhome.example": ("Cozy Home Co.", HOME, "#3c7a5a"),
}


def svg(emoji, bg):
    return f"""<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 400 400">
<rect width="400" height="400" fill="{bg}"/>
<circle cx="200" cy="215" r="120" fill="#ffffff" opacity=".55"/>
<ellipse cx="200" cy="335" rx="110" ry="14" fill="#000" opacity=".07"/>
<text x="200" y="250" font-size="170" text-anchor="middle" font-family="Noto Color Emoji, Apple Color Emoji, Segoe UI Emoji, sans-serif">{emoji}</text>
</svg>"""


def product_page(host, shop, color, slug, p):
    name, price, emoji, bg, desc = p
    ld = {
        "@context": "https://schema.org",
        "@type": "Product",
        "name": name,
        "image": [f"http://{host}/img/{slug}.svg"],
        "description": desc,
        "brand": {"@type": "Brand", "name": shop},
        "offers": {"@type": "Offer", "price": price, "priceCurrency": "USD", "availability": "https://schema.org/InStock"},
    }
    return f"""<!doctype html><html><head><meta charset="utf-8"><title>{html.escape(name)} | {shop}</title>
<meta property="og:site_name" content="{shop}">
<script type="application/ld+json">{json.dumps(ld)}</script>
<style>body{{font-family:system-ui;margin:0;background:#fff;color:#222}}header{{background:{color};color:#fff;padding:14px 32px;font-weight:800;font-size:20px}}
main{{display:flex;gap:40px;padding:40px 32px;max-width:1000px}}img{{width:380px;border-radius:16px}}.price{{font-size:28px;font-weight:700;color:{color}}}
button{{background:{color};color:#fff;border:0;padding:14px 28px;border-radius:999px;font-size:16px;font-weight:700}}</style></head>
<body><header>{shop}</header><main><img src="/img/{slug}.svg" alt=""><div><h1>{html.escape(name)}</h1>
<p class="price">${price}</p><p>{html.escape(desc)}</p><button>Add to cart</button></div></main></body></html>"""


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        host = (self.headers.get("Host") or "").split(":")[0]
        shop, products, color = SHOPS.get(host, SHOPS["tinytoyshop.example"])
        path = self.path.split("?")[0]
        if path.startswith("/img/") and path.endswith(".svg"):
            slug = path[5:-4]
            p = products.get(slug) or PRODUCTS.get(slug) or HOME.get(slug)
            if p:
                return self.send(200, "image/svg+xml", svg(p[2], p[3]))
        if path.startswith("/products/"):
            slug = path[len("/products/"):].strip("/")
            if slug in products:
                return self.send(200, "text/html; charset=utf-8", product_page(host, shop, color, slug, products[slug]))
        if path == "/":
            links = "".join(f'<li><a href="/products/{s}">{html.escape(p[0])}</a></li>' for s, p in products.items())
            return self.send(200, "text/html; charset=utf-8", f"<h1>{shop}</h1><ul>{links}</ul>")
        self.send(404, "text/plain", "not found")

    def send(self, code, ctype, body):
        data = body.encode()
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def log_message(self, *args):
        pass


if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 8099
    ThreadingHTTPServer(("0.0.0.0", port), Handler).serve_forever()
