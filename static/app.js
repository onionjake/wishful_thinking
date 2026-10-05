// Progressive enhancements. Every feature also works without JavaScript.
(function () {
  "use strict";

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

  // Import without a page reload: fill the item form in place.
  var importForm = document.querySelector("form[data-import]");
  var itemForm = document.querySelector("form[data-item-form]");
  var status = document.querySelector("[data-import-status]");
  if (importForm && itemForm && window.fetch) {
    importForm.addEventListener("submit", function (e) {
      var url = importForm.querySelector("input[name=url]").value.trim();
      if (!url) return;
      e.preventDefault();
      importForm.classList.add("loading");
      status.innerHTML = '<p class="muted small">Reading the page…</p>';
      fetch("/api/import", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ url: url }),
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
