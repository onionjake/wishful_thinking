#!/usr/bin/env python3
"""Fill a running Wishful Thinking instance with a demo family, using only its web endpoints.

    python3 demo/seed.py http://127.0.0.1:3000

Accounts all use the password "wishful-demo".
"""
import html
import http.cookiejar
import re
import sys
import urllib.parse
import urllib.request

BASE = sys.argv[1].rstrip("/") if len(sys.argv) > 1 else "http://127.0.0.1:3000"
PASSWORD = "wishful-demo"
TOYS = "http://tinytoyshop.example"
HOME = "http://cozyhome.example"


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        return None


class User:
    def __init__(self, name, email):
        self.name, self.email = name, email
        self.jar = http.cookiejar.CookieJar()
        self.opener = urllib.request.build_opener(
            urllib.request.HTTPCookieProcessor(self.jar), NoRedirect, urllib.request.ProxyHandler({})
        )

    def request(self, method, path, form=None):
        data = urllib.parse.urlencode(form or {}, doseq=True).encode() if method == "POST" else None
        req = urllib.request.Request(BASE + path, data=data, method=method)
        try:
            with self.opener.open(req) as r:
                return r.status, r.headers.get("Location"), r.read().decode()
        except urllib.error.HTTPError as e:
            return e.code, e.headers.get("Location"), e.read().decode()

    def get(self, path):
        return self.request("GET", path)

    def post(self, path, form=None):
        status, loc, body = self.request("POST", path, form)
        if status >= 400:
            raise SystemExit(f"POST {path} failed with {status}: {body[:300]}")
        return loc

    def signup(self):
        self.post("/signup", {"display_name": self.name, "email": self.email, "password": PASSWORD})
        return self


def last_id(loc):
    return int(re.search(r"/(\d+)(?:[#?].*)?$", loc).group(1))


def item_ids(user, list_id):
    _, _, body = user.get(f"/lists/{list_id}")
    ids = {}
    for m in re.finditer(r'id="item-(\d+)".*?<h3>.*?>([^<]+)</a>', body, re.S):
        ids[html.unescape(m.group(2).strip())] = int(m.group(1))
    return ids


sarah = User("Sarah Lee", "sarah@example.com").signup()
david = User("David Lee", "david@example.com").signup()
jo = User("Grandma Jo", "jo@example.com").signup()
ben = User("Uncle Ben", "ben@example.com").signup()
priya = User("Priya Shah", "priya@example.com").signup()

# Families and invites.
lees = last_id(sarah.post("/families", {"name": "The Lees", "description": "Sarah, David, the kids, Grandma Jo & Uncle Ben"}))
sarah.post(f"/families/{lees}/invites", {"expires_days": "30", "max_uses": "0"})
_, _, page = sarah.get(f"/families/{lees}")
token = re.search(r"/join/([A-Za-z0-9_-]+)", page).group(1)
for u in (david, jo, ben):
    u.post(f"/join/{token}")
book_club = last_id(sarah.post("/families", {"name": "Book Club Gift Swap", "description": "Holiday swap — $40 limit"}))
sarah.post(f"/families/{book_club}/invites", {"expires_days": "7", "max_uses": "10"})
_, _, page = sarah.get(f"/families/{book_club}")
priya.post("/join/" + re.search(r"/join/([A-Za-z0-9_-]+)", page).group(1))

# Lists.
def new_list(user, title, recipient="", date="", families=(), show=False, notes=""):
    form = {"title": title, "recipient_name": recipient, "event_date": date, "description": notes, "families": [str(f) for f in families]}
    if show:
        form["show_claims_to_owner"] = "1"
    return last_id(user.post("/lists", form))


def add(user, list_id, shop, slug, name, price, priority=2, quantity=1, notes="", store=None):
    user.post(
        f"/lists/{list_id}/items",
        {
            "title": name, "price": price, "currency": "USD", "url": f"{shop}/products/{slug}",
            "image_url": f"{shop}/img/{slug}.svg", "store": store or ("Tiny Toy Shop" if shop == TOYS else "Cozy Home Co."),
            "priority": str(priority), "quantity": str(quantity), "notes": notes, "import_source": "structured",
        },
    )


maya = new_list(sarah, "Maya's 7th Birthday", "Maya", "2026-11-14", [lees], show=True,
                notes="Maya loves space, drawing and anything rainbow. She already has plenty of stuffed animals!")
add(sarah, maya, TOYS, "robot-kit", "Build-a-Bot Coding Robot Kit", "49.99", 1)
add(sarah, maya, TOYS, "telescope", "First Telescope for Young Astronomers", "89.00", 1)
add(sarah, maya, TOYS, "art-easel", "Double-Sided Art Easel", "64.00", 2)
add(sarah, maya, TOYS, "bike-helmet", "Kids Bike Helmet — Starry Night", "34.95", 2, notes="Size S (head 50–54cm)")
add(sarah, maya, TOYS, "story-books", "Bedtime Stories Picture Book Set", "32.50", 3)
add(sarah, maya, TOYS, "rainbow-stacker", "Wooden Rainbow Stacker, 7 pieces", "38.50", 3)

leo = new_list(sarah, "Leo's Holiday Wishes", "Leo", "2026-12-25", [lees], show=True,
               notes="Leo is 4 and obsessed with dinosaurs and trains.")
add(sarah, leo, TOYS, "dino-set", "Jurassic Dinosaur Figure Set (12)", "27.99", 1)
add(sarah, leo, TOYS, "train-set", "Wooden Railway Starter Set", "74.00", 1)
add(sarah, leo, TOYS, "scooter", "3-Wheel Kick Scooter with Light-Up Wheels", "59.99", 2)
add(sarah, leo, TOYS, "plush-fox", "Snuggly Fox Plush, Large", "24.00", 3, quantity=2, notes="One for Leo, one for daycare")

mine = new_list(sarah, "Sarah's Wish List", "", "2026-12-25", [lees, book_club])
add(sarah, mine, HOME, "knit-blanket", "Chunky Knit Throw Blanket, Oatmeal", "89.00", 1)
add(sarah, mine, HOME, "running-shoes", "Cloudstride Running Shoes, Women's", "130.00", 1, notes="Size 8, the pink colourway")
add(sarah, mine, HOME, "pour-over", "Ceramic Pour-Over Coffee Set", "42.00", 2)
add(sarah, mine, HOME, "planter", "Self-Watering Ceramic Planter, Large", "36.00", 2)
add(sarah, mine, HOME, "novel", "The Lighthouse Keeper's Daughter (Hardcover)", "27.00", 3)
add(sarah, mine, HOME, "candle", "Cedar & Sage Soy Candle", "22.00", 3, quantity=2)
sarah.post(f"/lists/{mine}/public", {"action": "enable"})

dlist = new_list(david, "David's Wishes", "", "2026-12-25", [lees])
add(david, dlist, HOME, "cast-iron", "Enameled Cast Iron Dutch Oven, 5.5 qt", "110.00", 1)
add(david, dlist, HOME, "headphones", "Wireless Noise-Cancelling Headphones", "199.00", 2)

# David co-manages the kids' lists.
for lst in (maya, leo):
    sarah.post(f"/lists/{lst}/managers", {"user_id": "2"})

# Reservations.
m = item_ids(jo, maya)
jo.post(f"/items/{m['Build-a-Bot Coding Robot Kit']}/claim")
jo.post(f"/items/{m['First Telescope for Young Astronomers']}/claim")
ben.post(f"/items/{m['Double-Sided Art Easel']}/claim")
l = item_ids(jo, leo)
jo.post(f"/items/{l['Jurassic Dinosaur Figure Set (12)']}/claim")
ben.post(f"/items/{l['Snuggly Fox Plush, Large']}/claim", {"quantity": "1"})
s = item_ids(jo, mine)
jo.post(f"/items/{s['Chunky Knit Throw Blanket, Oatmeal']}/claim")
shoes = s["Cloudstride Running Shoes, Women's"]
novel = s["The Lighthouse Keeper's Daughter (Hardcover)"]
david.post(f"/items/{shoes}/claim")
priya.post(f"/items/{novel}/claim")
d = item_ids(sarah, dlist)
sarah.post(f"/items/{d['Enameled Cast Iron Dutch Oven, 5.5 qt']}/claim")

# Grandma has already bought one gift.
_, _, page = jo.get("/reservations")
first_claim = re.search(r'/claims/(\d+)/purchased', page).group(1)
jo.post(f"/claims/{first_claim}/purchased", {"back": "/reservations"})

print(f"Seeded. Sign in at {BASE}/login as sarah@example.com / {PASSWORD}")
print(f"IDs: maya={maya} leo={leo} sarah={mine} david={dlist} lees={lees} book_club={book_club}")
