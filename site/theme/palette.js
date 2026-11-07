// Haven Terminal — the site's command palette.
//
// The program has a palette: one modal, every command, narrowed by typing,
// each row carrying the key that runs it. The site has the same thing over
// its own content, because a documentation site is a list of places you can
// go and that is the same problem.
//
// It is built on mdBook's search index rather than beside it — the same
// elasticlunr index the built-in search uses, which already carries a title,
// a breadcrumb trail and a body for all 170-odd sections. No second index,
// and nothing to keep in step.
//
// Progressive enhancement throughout: mdBook's own search UI is left in the
// page and working, and is only hidden once this file has successfully put
// something better in front of it.

(function () {
    "use strict";

    var root = document.documentElement;
    var overlay = null;
    var input = null;
    var list = null;
    var empty = null;
    var rows = [];
    var cursor = 0;
    var index = null;
    var loading = false;

    // Static destinations, shown before anything is typed. This is the "menu"
    // half: the handful of places most people actually want, with a word
    // about each, rather than a wall of search results.
    var JUMPS = [
        { sym: "⤓", title: "Install it", meta: "Arch package, or a one-line installer", url: "install.html" },
        { sym: "⇄", title: "How it compares", meta: "Against Alacritty, kitty, WezTerm, Ghostty, tmux", url: "comparison.html" },
        { sym: "◫", title: "Keys and commands", meta: "The prefix table, copy mode, the mouse", url: "keys.html" },
        { sym: "▤", title: "Themes and configuration", meta: "config.toml, themes, bindings, fonts", url: "themes.html" },
        { sym: "✓", title: "Honest status", meta: "What works, and what is not written yet", url: "status.html" },
        { sym: "⌗", title: "Architecture", meta: "Thirteen crates and the layering gate", url: "architecture.html" },
        { sym: "⌕", title: "Design & implementation plan", meta: "The whole plan, included from docs/PLAN.md", url: "plan.html" }
    ];

    function base() {
        return (typeof path_to_root === "string" ? path_to_root : "");
    }

    // ---- the index, loaded the first time it is needed ------------------

    var waiting = [];

    function withIndex(then) {
        if (index) return then();

        // A second caller while the index is in flight must be queued, not
        // dropped: opening the palette warms the index, and the first search
        // arrives before that finishes. Returning early here lost it, and the
        // palette sat on its jump list however much you typed.
        waiting.push(then);
        if (loading) return;

        function build() {
            loading = false;
            if (window.search && window.search.index && window.elasticlunr) {
                try {
                    index = elasticlunr.Index.load(window.search.index);
                } catch (e) {
                    index = null;
                }
            }
            var queued = waiting;
            waiting = [];
            queued.forEach(function (fn) {
                fn();
            });
        }

        if (window.search && window.search.index) return build();

        loading = true;
        var src = window.path_to_searchindex_js;
        if (!src) {
            loading = false;
            return;
        }
        var script = document.createElement("script");
        script.src = src;
        script.onload = build;
        script.onerror = function () {
            loading = false;
            var queued = waiting;
            waiting = [];
            queued.forEach(function (fn) {
                fn();
            });
        };
        document.head.appendChild(script);
    }

    // ---- rendering ------------------------------------------------------

    function esc(text) {
        return String(text).replace(/[&<>"]/g, function (c) {
            return { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c];
        });
    }

    // A short run of the body around the first match, so a row says why it
    // matched rather than only that it did.
    function teaser(body, words) {
        if (!body) return "";
        var lower = body.toLowerCase();
        var at = -1;
        for (var i = 0; i < words.length && at < 0; i++) {
            if (words[i].length > 1) at = lower.indexOf(words[i]);
        }
        var from = at < 0 ? 0 : Math.max(0, at - 42);
        var cut = body.slice(from, from + 150).trim();
        return (from > 0 ? "…" : "") + cut + (from + 150 < body.length ? "…" : "");
    }

    function mark(text, words) {
        var out = esc(text);
        words.forEach(function (word) {
            if (word.length < 2) return;
            out = out.replace(
                new RegExp("(" + word.replace(/[.*+?^${}()|[\]\\]/g, "\\$&") + ")", "ig"),
                "<mark>$1</mark>"
            );
        });
        return out;
    }

    function draw(items, words) {
        rows = items;
        cursor = 0;
        if (!items.length) {
            list.innerHTML = "";
            empty.hidden = false;
            return;
        }
        empty.hidden = true;
        list.innerHTML = items
            .map(function (item, i) {
                return (
                    '<li role="option" aria-selected="' + (i === 0) + '" class="hv-row' +
                    (i === 0 ? " is-on" : "") + '" data-i="' + i + '">' +
                    '<span class="hv-row-sym">' + esc(item.sym || "›") + "</span>" +
                    '<span class="hv-row-main">' +
                    '<span class="hv-row-title">' + (words ? mark(item.title, words) : esc(item.title)) + "</span>" +
                    (item.meta ? '<span class="hv-row-meta">' + (words ? mark(item.meta, words) : esc(item.meta)) + "</span>" : "") +
                    "</span>" +
                    (item.crumb ? '<span class="hv-row-crumb">' + esc(item.crumb) + "</span>" : "") +
                    "</li>"
                );
            })
            .join("");
    }

    function move(delta) {
        if (!rows.length) return;
        var nodes = list.children;
        nodes[cursor].classList.remove("is-on");
        nodes[cursor].setAttribute("aria-selected", "false");
        cursor = (cursor + delta + rows.length) % rows.length;
        var node = nodes[cursor];
        node.classList.add("is-on");
        node.setAttribute("aria-selected", "true");
        node.scrollIntoView({ block: "nearest" });
    }

    function go(i) {
        var item = rows[i];
        if (!item) return;
        close();
        window.location.href = base() + item.url;
    }

    // ---- searching ------------------------------------------------------

    function run(query) {
        query = query.trim();
        if (!query) {
            draw(JUMPS, null);
            return;
        }

        withIndex(function () {
            if (!index) {
                draw([], null);
                return;
            }
            var words = query.toLowerCase().split(/\s+/).filter(Boolean);
            var hits = index.search(query, window.search.search_options).slice(0, 24);
            var docs = window.search.index.documentStore.docs;
            var urls = window.search.doc_urls;

            draw(
                hits.map(function (hit) {
                    var doc = docs[hit.ref] || {};
                    // The breadcrumb's last element is the section itself,
                    // which the title already says.
                    var crumbs = (doc.breadcrumbs || "").split(" » ");
                    crumbs.pop();
                    return {
                        sym: "⌕",
                        title: doc.title || "Untitled",
                        meta: teaser(doc.body, words),
                        crumb: crumbs.join(" › "),
                        url: urls[hit.ref]
                    };
                }),
                words
            );
        });
    }

    // ---- the modal ------------------------------------------------------

    function build() {
        overlay = document.createElement("div");
        overlay.className = "hv-palette";
        overlay.hidden = true;
        overlay.innerHTML =
            '<div class="hv-palette-scrim" data-close></div>' +
            '<div class="hv-palette-box" role="dialog" aria-modal="true" aria-label="Search the documentation">' +
            '<div class="hv-palette-head">' +
            '<span class="hv-palette-icon">⌕</span>' +
            '<input class="hv-palette-input" type="text" autocomplete="off" spellcheck="false" ' +
            'placeholder="Search the docs, or jump somewhere…" aria-label="Search" ' +
            'role="combobox" aria-expanded="true" aria-controls="hv-palette-list">' +
            '<kbd class="hv-palette-esc">Esc</kbd>' +
            "</div>" +
            '<ul class="hv-palette-list" id="hv-palette-list" role="listbox"></ul>' +
            '<p class="hv-palette-empty" hidden>Nothing matches that.</p>' +
            '<div class="hv-palette-foot">' +
            "<span><kbd>↑</kbd><kbd>↓</kbd> move</span>" +
            "<span><kbd>↵</kbd> open</span>" +
            "<span><kbd>Esc</kbd> close</span>" +
            '<span class="hv-palette-foot-end">search by mdBook</span>' +
            "</div>" +
            "</div>";
        document.body.appendChild(overlay);

        input = overlay.querySelector(".hv-palette-input");
        list = overlay.querySelector(".hv-palette-list");
        empty = overlay.querySelector(".hv-palette-empty");

        input.addEventListener("input", function () {
            run(input.value);
        });

        overlay.addEventListener("click", function (event) {
            if (event.target.hasAttribute("data-close")) return close();
            var row = event.target.closest(".hv-row");
            if (row) go(Number(row.dataset.i));
        });

        overlay.addEventListener("mousemove", function (event) {
            var row = event.target.closest(".hv-row");
            if (!row) return;
            var i = Number(row.dataset.i);
            if (i !== cursor) move(i - cursor);
        });

        input.addEventListener("keydown", function (event) {
            if (event.key === "ArrowDown") {
                event.preventDefault();
                move(1);
            } else if (event.key === "ArrowUp") {
                event.preventDefault();
                move(-1);
            } else if (event.key === "Enter") {
                event.preventDefault();
                go(cursor);
            } else if (event.key === "Escape") {
                event.preventDefault();
                close();
            }
        });
    }

    function open() {
        if (!overlay) build();
        if (!overlay.hidden) return;
        overlay.hidden = false;
        root.classList.add("hv-palette-open");
        input.value = "";
        run("");
        // Warm the index while the first keystroke is still being thought
        // about, so the first search does not wait on a download.
        withIndex(function () {});
        input.focus();
    }

    function close() {
        if (!overlay || overlay.hidden) return;
        overlay.hidden = true;
        root.classList.remove("hv-palette-open");
    }

    function start() {
        // Take over mdBook's search button, and its keyboard shortcuts.
        var toggle = document.getElementById("mdbook-search-toggle");
        if (toggle) {
            toggle.addEventListener(
                "click",
                function (event) {
                    event.preventDefault();
                    event.stopImmediatePropagation();
                    open();
                },
                true
            );
            toggle.setAttribute("title", "Search (Ctrl+K)");
            toggle.setAttribute("aria-keyshortcuts", "Control+K /");
        }

        document.addEventListener(
            "keydown",
            function (event) {
                var k = event.key;
                if ((event.ctrlKey || event.metaKey) && (k === "k" || k === "K")) {
                    event.preventDefault();
                    event.stopImmediatePropagation();
                    return overlay && !overlay.hidden ? close() : open();
                }
                if (overlay && !overlay.hidden) return;
                // `/` and `s` are mdBook's own; take them only when the reader
                // is not typing into something.
                var tag = (event.target.tagName || "").toLowerCase();
                if (tag === "input" || tag === "textarea" || event.target.isContentEditable) return;
                if (k === "/" || k === "s" || k === "S") {
                    event.preventDefault();
                    event.stopImmediatePropagation();
                    open();
                }
            },
            true
        );

        // Only now, with a working replacement attached, retire mdBook's own
        // search panel.
        root.classList.add("hv-has-palette");

        // `#search` opens it on arrival, so a link can point at the search
        // itself — and `#search=cells` at a query.
        var hash = decodeURIComponent(window.location.hash || "");
        if (hash === "#search" || hash.indexOf("#search=") === 0) {
            open();
            var q = hash.slice("#search=".length);
            if (hash.indexOf("=") > 0 && q) {
                input.value = q;
                run(q);
            }
        }
    }

    if (document.readyState === "loading") {
        document.addEventListener("DOMContentLoaded", start);
    } else {
        start();
    }
})();
