// Progressive enhancements. Every feature also works without JavaScript.
(function () {
  "use strict";

  // Register the (no-op) service worker so the site can be installed as an app.
  if ("serviceWorker" in navigator) {
    navigator.serviceWorker.register("/sw.js").catch(function () {});
  }

  // Confirmation prompts on destructive forms.
  document.querySelectorAll("form[data-confirm]").forEach(function (form) {
    form.addEventListener("submit", function (e) {
      if (!confirm(form.dataset.confirm)) e.preventDefault();
    });
  });
  document.querySelectorAll("[data-confirm-click]").forEach(function (btn) {
    btn.addEventListener("click", function (e) {
      if (!confirm(btn.dataset.confirmClick)) e.preventDefault();
    });
  });

  // Copy-to-clipboard buttons.
  document.querySelectorAll("[data-copy]").forEach(function (btn) {
    btn.addEventListener("click", function () {
      var input = document.querySelector(btn.dataset.copy);
      if (!input) return;
      input.select();
      var done = function () {
        var old = btn.textContent;
        btn.textContent = "Copied!";
        setTimeout(function () { btn.textContent = old; }, 1600);
      };
      if (navigator.clipboard) navigator.clipboard.writeText(input.value).then(done, function () { document.execCommand("copy"); done(); });
      else { document.execCommand("copy"); done(); }
    });
  });

  // Close the account menu and the ⋯ menus on wishes when tapping/clicking outside them or
  // pressing Escape. pointerdown (not click) so taps on non-interactive areas register on iOS.
  var usermenus = document.querySelectorAll("details.usermenu, details.item-menu");
  if (usermenus.length) {
    document.addEventListener("pointerdown", function (e) {
      usermenus.forEach(function (menu) {
        if (menu.open && !menu.contains(e.target)) menu.open = false;
      });
    });
    document.addEventListener("keydown", function (e) {
      if (e.key !== "Escape") return;
      usermenus.forEach(function (menu) {
        if (!menu.open) return;
        menu.open = false;
        menu.querySelector("summary").focus();
      });
    });
  }

  // Auto-submit sort dropdowns.
  document.querySelectorAll("select[data-autosubmit]").forEach(function (sel) {
    sel.addEventListener("change", function () { sel.form.submit(); });
  });

  // Live picture preview on the item form.
  var preview = document.querySelector("[data-preview]");
  var imageInput = document.querySelector("[data-image-input]");
  function showPreview(src) {
    if (!preview) return;
    preview.innerHTML = "";
    if (src) {
      var img = document.createElement("img");
      img.src = src;
      img.alt = "";
      img.referrerPolicy = "no-referrer";
      img.onerror = function () { preview.innerHTML = "<span>🖼️</span>"; };
      preview.appendChild(img);
    } else {
      preview.innerHTML = "<span>🎁</span>";
    }
  }
  if (imageInput) imageInput.addEventListener("change", function () { showPreview(imageInput.value.trim()); });

  // Bookmarklet hand-off: the store page's details arrive in the URL fragment (#clip=…),
  // which never reaches the server. Keep it when picking a list on /add.
  var clip = location.hash.indexOf("#clip=") === 0 ? location.hash.slice(6) : null;
  if (clip) {
    document.querySelectorAll("a[data-keep-hash]").forEach(function (a) { a.href += location.hash; });
  }

  // Import without a page reload: fill the item form in place.
  var importForm = document.querySelector("form[data-import]");
  var itemForm = document.querySelector("form[data-item-form]");
  var status = document.querySelector("[data-import-status]");
  if (importForm && itemForm && window.fetch) {
    var urlInput = importForm.querySelector("input[name=url]");
    importForm.addEventListener("submit", function (e) {
      var url = urlInput.value.trim();
      if (!url) return;
      e.preventDefault();
      runImport("/api/import", { url: url });
    });
    var fromBookmarklet = /[?&]clip=1/.test(location.search);
    if (fromBookmarklet) {
      // Tidy the address bar so a reload doesn't repeat the hand-off.
      history.replaceState(null, "", location.pathname + location.search.replace(/&?clip=1/, "").replace(/\?$/, ""));
      var page = null;
      try { page = clip && JSON.parse(decodeURIComponent(clip)); } catch (err) { page = null; }
      var url = urlInput.value.trim();
      if (page && url) runImport("/api/import-page", { url: url, page: page });
      else if (url) runImport("/api/import", { url: url });
    }
  }
  function runImport(endpoint, body) {
    importForm.classList.add("loading");
    status.innerHTML = '<p class="muted small">Reading the page…</p>';
    fetch(endpoint, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body),
      credentials: "same-origin"
    })
      .then(function (r) { return r.json(); })
      .then(function (data) {
        importForm.classList.remove("loading");
        status.innerHTML = "";
        if (!data.ok) {
          addMsg("form-error", data.error || "Import failed");
          setField("url", data.url);
          return;
        }
        var set = function (name, value) { if (value) setField(name, value); };
        set("url", data.url);
        set("title", data.title);
        set("price", data.price);
        set("store", data.store);
        set("image_url", data.image_url);
        set("import_source", data.import_source);
        if (data.currency) {
          var sel = itemForm.querySelector("select[name=currency]");
          if (![].some.call(sel.options, function (o) { return o.value === data.currency; })) {
            var opt = document.createElement("option");
            opt.textContent = data.currency;
            sel.appendChild(opt);
          }
          sel.value = data.currency;
        }
        if (data.image_url) showPreview(data.image_url);
        if (data.found.length) {
          addMsg("notice", "✨ Found " + data.found.join(", ") + (data.llm_used ? " (with help from " + data.llm_used + ")" : "") + ". Check the details below, then save.");
        }
        (data.warnings || []).forEach(function (w) { addMsg("warning", w); });
        var title = itemForm.querySelector("input[name=title]");
        if (title) title.focus();
      })
      .catch(function () {
        importForm.classList.remove("loading");
        status.innerHTML = "";
        addMsg("form-error", "Couldn't reach the server. Try again.");
      });
  }
  function setField(name, value) {
    var el = itemForm.querySelector("[name=" + name + "]");
    if (el) el.value = value;
  }
  function addMsg(cls, text) {
    var p = document.createElement("p");
    p.className = cls;
    p.textContent = text;
    status.appendChild(p);
  }
})();
