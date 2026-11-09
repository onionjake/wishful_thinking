// "Add to Wishful Thinking" bookmarklet. Runs on a store's product page in the user's own
// browser, copies the product details the page already contains, and opens the add-a-wish
// form with them in a new tab, so stores that block our server (robot checks) still import fully.
//
// Built into a javascript: URL by templates::bookmarklet(): full-line comments and line
// breaks are removed and __BASE__ becomes the site address. Keep every statement ended with
// a semicolon, and never use the percent or hash characters (browsers decode the former
// in javascript: URLs; the latter is written as \x23).
(function () {
  var d = document;
  var cut = function (s, n) { return String(s || "").replace(/\s+/g, " ").trim().slice(0, n); };
  var p = { title: cut(d.title, 300), meta: [], json_ld: [], microdata: [], h1: "", image: "", price: "", text: "" };
  d.querySelectorAll("meta[content]").forEach(function (m) {
    var k = m.getAttribute("property") || m.getAttribute("name") || m.getAttribute("itemprop") || "";
    if (p.meta.length < 60 && /^(og:|product:|twitter:|description$|application-name$)/i.test(k)) p.meta.push([k, cut(m.content, 1500)]);
  });
  d.querySelectorAll("script[type='application/ld+json']").forEach(function (s) {
    var t = s.textContent || "";
    if (p.json_ld.length >= 4 || !/product/i.test(t)) return;
    try { t = JSON.stringify(JSON.parse(t), function (k, v) { return /^reviews?$/i.test(k) ? undefined : v; }); } catch (e) {}
    p.json_ld.push(t.slice(0, 40000));
  });
  var scope = d.querySelector("[itemtype*='schema.org/Product']");
  if (scope) scope.querySelectorAll("[itemprop]").forEach(function (e) {
    if (p.microdata.length < 40) p.microdata.push([e.getAttribute("itemprop"), cut(e.getAttribute("content") || e.currentSrc || e.getAttribute("src") || e.getAttribute("href") || e.textContent, 500)]);
  });
  var h = d.querySelector("h1");
  if (h) p.h1 = cut(h.innerText || h.textContent, 300);
  var best = 0;
  d.querySelectorAll("img").forEach(function (i) {
    var r = i.getBoundingClientRect(), src = i.currentSrc || i.src || "";
    if (r.width >= 150 && r.height >= 150 && r.width * r.height > best && /^https?:/.test(src)) { best = r.width * r.height; p.image = src; }
  });
  var prices = ["[itemprop='price']", "[id='corePrice_feature_div'] .a-offscreen", "[id='corePriceDisplay_desktop_feature_div'] .a-offscreen", "[data-testid='product-price']", "[data-test='product-price']", "[data-automation-id='product-price']", ".product-price", ".price .amount"];
  for (var n = 0; n < prices.length && !p.price; n++) {
    var e = d.querySelector(prices[n]);
    if (e) p.price = cut(e.getAttribute("content") || e.textContent, 80);
  }
  p.text = cut((d.querySelector("main") || d.body || {}).innerText, 6000);
  var u = __BASE__ + "/add?clip=1&url=" + encodeURIComponent(location.href) + "\x23clip=" + encodeURIComponent(JSON.stringify(p));
  // A new tab keeps the shopper's place in the store; close it after saving to get back.
  // If the browser blocks the tab, use this one instead.
  var w = window.open(u, "_blank");
  if (w) { try { w.opener = null; } catch (e) {} } else { location.href = u; }
})();
