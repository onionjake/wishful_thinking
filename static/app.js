// Progressive enhancements. Every feature also works without JavaScript.
(function () {
  "use strict";

  // Register the (no-op) service worker so the site can be installed as an app.
  if ("serviceWorker" in navigator) {
    navigator.serviceWorker.register("/sw.js").catch(function () {});
  }

  // For admins: say whose page this is on every change, so a tab left open from before they
  // started or stopped acting as someone can't do it as the wrong person (see src/auth.rs).
  var pageUser = document.body.dataset.as;
  if (pageUser) {
    document.addEventListener("submit", function (e) {
      var form = e.target;
      if ((form.getAttribute("method") || "").toLowerCase() !== "post") return;
      var url = new URL(form.getAttribute("action") || location.href, location.href);
      url.searchParams.set("wt_as", pageUser);
      form.setAttribute("action", url.pathname + url.search + url.hash);
    }, true);
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

  // Reserving a surprise gift: check the reveal month with the giver before saving, since from
  // then on the list owner sees who gave it. "Change month" goes back to the picker.
  document.querySelectorAll("select[data-reveal-confirm]").forEach(function (sel) {
    var form = sel.form, confirmed = false;
    form.addEventListener("submit", function (e) {
      if (confirmed || !sel.value) return;
      var parts = sel.value.split("-");
      var when = new Date(+parts[0], +parts[1] - 1, 1).toLocaleDateString(undefined, { month: "long", day: "numeric", year: "numeric" });
      var question = "Do you plan on giving this gift before " + when + "?";
      var detail = (sel.dataset.recipient || "They") + " will be able to see who it's from starting " + when + ".";
      e.preventDefault();
      e.stopImmediatePropagation();
      askReveal(question, detail, function () {
        confirmed = true;
        if (form.requestSubmit) form.requestSubmit(); else form.submit();
      }, function () {
        sel.focus();
        if (sel.showPicker) { try { sel.showPicker(); } catch (err) { /* needs a user gesture */ } }
      });
    }, true);
  });
  function askReveal(question, detail, yes, change) {
    if (typeof HTMLDialogElement !== "function") {
      if (confirm(question + "\n\n" + detail + "\n\nPress Cancel to pick a later month.")) yes(); else change();
      return;
    }
    var dlg = document.createElement("dialog");
    dlg.className = "confirm-dialog";
    dlg.innerHTML = '<p class="confirm-q"></p><p class="muted small"></p>' +
      '<div class="confirm-actions"><button type="button" class="btn btn-small btn-ghost" data-act="change">Change month</button>' +
      '<button type="button" class="btn btn-small" data-act="yes">Yes, reserve it</button></div>';
    dlg.querySelector(".confirm-q").textContent = question;
    dlg.querySelector(".muted").textContent = detail;
    var answer = "change";
    dlg.addEventListener("click", function (e) {
      var act = e.target.dataset && e.target.dataset.act;
      if (act) { answer = act; dlg.close(); }
    });
    dlg.addEventListener("close", function () {
      dlg.remove();
      if (answer === "yes") yes(); else change();
    });
    document.body.appendChild(dlg);
    dlg.showModal();
    dlg.querySelector('[data-act="yes"]').focus();
  }

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
    // requestSubmit fires the submit event, so the admin page check above still runs.
    sel.addEventListener("change", function () {
      if (sel.form.requestSubmit) sel.form.requestSubmit(); else sel.form.submit();
    });
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
  // which never reaches the server. Browsers drop the fragment when a form is posted (signing
  // in on the way, for instance), so park it in this tab's sessionStorage until the item form
  // picks it up. Also keep it on the list links on /add.
  var clip = location.hash.indexOf("#clip=") === 0 ? location.hash.slice(6) : null;
  if (clip) {
    try { sessionStorage.setItem("wt-clip", clip); } catch (err) { /* storage unavailable */ }
    document.querySelectorAll("a[data-keep-hash]").forEach(function (a) { a.href += location.hash; });
  }
  function takeClip() {
    var data = clip;
    try {
      data = data || sessionStorage.getItem("wt-clip");
      sessionStorage.removeItem("wt-clip");
    } catch (err) { /* storage unavailable */ }
    return data;
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
      var page = null, data = takeClip();
      try { page = data && JSON.parse(decodeURIComponent(data)); } catch (err) { page = null; }
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
      headers: pageUser ? { "Content-Type": "application/json", "X-WT-As": pageUser } : { "Content-Type": "application/json" },
      body: JSON.stringify(body),
      credentials: "same-origin"
    })
      .then(function (r) {
        if (r.status === 400 && pageUser) throw new Error("stale");
        return r.json();
      })
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
      .catch(function (err) {
        importForm.classList.remove("loading");
        status.innerHTML = "";
        addMsg("form-error", err && err.message === "stale"
          ? "This page was opened as someone other than the person you're acting as now. Reload the page and try again."
          : "Couldn't reach the server. Try again.");
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
