// Landing window frontend.
//
// Reports Ready, receives one LandingData payload (recents + layout templates),
// renders two rows of cards, tracks a single selection across both rows, and
// posts the chosen open intent (or Cancel) back to Rust. Inbound controls are
// envelope-free flat objects ({ "kind": "OpenTemplate", ... }); the data
// payload arrives via window.__landing.receive(<json string>).
(function () {
    "use strict";

    // post
    // Inputs: a control kind and optional extra fields. Output: side-effect;
    // posts a flat LandingInbound object to Rust.
    function post(kind, extra) {
        if (!window.ipc || typeof window.ipc.postMessage !== "function") {
            console.error("landing: window.ipc.postMessage unavailable");
            return;
        }
        const msg = Object.assign({ kind: kind }, extra || {});
        window.ipc.postMessage(JSON.stringify(msg));
    }

    // Current single selection across both rows, or null.
    // { kind: "template", theme_id, layout_id } | { kind: "recent", path }
    let selection = null;

    // relativeDate
    // Inputs: unix seconds. Output: a short "Nm/h/d ago" string.
    function relativeDate(secs) {
        const delta = Math.max(0, Date.now() / 1000 - Number(secs || 0));
        if (delta < 3600) {
            return Math.floor(delta / 60) + "m ago";
        }
        if (delta < 86400) {
            return Math.floor(delta / 3600) + "h ago";
        }
        return Math.floor(delta / 86400) + "d ago";
    }

    // clearSelection
    // Drops the aria-selected flag from every card so only one stays lit.
    function clearSelection() {
        const cards = document.querySelectorAll(".landing__card[aria-selected=\"true\"]");
        for (let i = 0; i < cards.length; i++) {
            cards[i].setAttribute("aria-selected", "false");
        }
    }

    // bar
    // Inputs: a color plus left/top/width/height as percentages. Output: an
    // absolutely-positioned preview bar div.
    function bar(color, left, top, width, height) {
        const d = document.createElement("div");
        d.className = "landing__tile-bar";
        d.style.background = color;
        d.style.left = left + "%";
        d.style.top = top + "%";
        d.style.width = width + "%";
        d.style.height = height + "%";
        return d;
    }

    // layoutTile
    // Inputs: a template entry. Output: a 16:9 tile painted in the theme
    // background with bars echoing the layout (title / hero / text).
    function layoutTile(t) {
        const tile = document.createElement("div");
        tile.className = "landing__tile";
        tile.style.background = t.background;
        const fg = t.foreground;
        const accent = t.accent;
        if (t.layout_id === "title" || !t.layout_id) {
            tile.appendChild(bar(fg, 18, 42, 64, 14));
            tile.appendChild(bar(fg, 18, 62, 40, 8));
        } else if (t.layout_id === "hero") {
            tile.appendChild(bar(fg, 10, 30, 45, 16));
            tile.appendChild(bar(fg, 10, 60, 38, 8));
            tile.appendChild(bar(accent, 66, 20, 26, 60));
        } else {
            tile.appendChild(bar(fg, 10, 14, 55, 12));
            tile.appendChild(bar(fg, 10, 38, 80, 6));
            tile.appendChild(bar(fg, 10, 50, 80, 6));
            tile.appendChild(bar(fg, 10, 62, 70, 6));
            tile.appendChild(bar(t.accent, 10, 86, 30, 5));
        }
        return tile;
    }

    // makeCard
    // Inputs: a tile node, a title, a subtitle, the selection descriptor, and
    // an open callback. Output: a wired card (click selects, dblclick opens).
    function makeCard(tile, title, subtitle, descriptor, openFn) {
        const card = document.createElement("div");
        card.className = "landing__card";
        card.setAttribute("aria-selected", "false");
        card.appendChild(tile);
        const label = document.createElement("div");
        label.className = "landing__card-label";
        const t = document.createElement("span");
        t.className = "landing__card-title";
        t.textContent = title;
        const s = document.createElement("span");
        s.className = "landing__card-sub";
        s.textContent = subtitle;
        label.appendChild(t);
        label.appendChild(s);
        card.appendChild(label);
        card.addEventListener("click", function () {
            clearSelection();
            card.setAttribute("aria-selected", "true");
            selection = descriptor;
        });
        card.addEventListener("dblclick", openFn);
        return card;
    }

    // thumbScaler
    // One observer for every mounted thumbnail: whenever a stage's width
    // changes (window resize, row reflow) it rescales that stage's surface so
    // the native-pixel slide keeps filling the tile. A per-mount transform set
    // once would freeze at its first width and drift as the row reflows.
    const thumbScaler = new ResizeObserver(function (entries) {
        for (let i = 0; i < entries.length; i++) {
            const stage = entries[i].target;
            const surface = stage.firstChild;
            const native = Number(surface && surface.dataset.w) || 1;
            surface.style.transform = "scale(" + stage.clientWidth / native + ")";
        }
    });

    // mountThumb
    // Inputs: a tile element and a thumb payload { html, css, asset_vars_css,
    // width, height }. Output: side-effect; mounts the first-slide render in a
    // shadow root on the tile (so :host-scoped theme vars resolve) and scales
    // the native slide dimensions down to the tile's width via thumbScaler.
    // Inert: pointer events off, no interaction.
    function mountThumb(tile, thumb) {
        const w = Number(thumb.width) || 1920;
        const h = Number(thumb.height) || 1080;
        const stage = document.createElement("div");
        stage.className = "landing__thumb-stage";
        const surface = document.createElement("div");
        surface.className = "landing__thumb-surface";
        surface.style.width = w + "px";
        surface.style.height = h + "px";
        surface.dataset.w = String(w);
        const shadow = surface.attachShadow({ mode: "open" });
        shadow.innerHTML = "<style>" + thumb.css + "</style><style>"
            + (thumb.asset_vars_css || "") + "</style>" + thumb.html;
        stage.appendChild(surface);
        tile.appendChild(stage);
        thumbScaler.observe(stage);
    }

    // renderRecents / renderLayouts
    // Fill each row from the payload, or show an empty state.
    function renderRecents(recents) {
        const root = document.getElementById("recents");
        root.replaceChildren();
        if (!recents || recents.length === 0) {
            const e = document.createElement("div");
            e.className = "landing__empty";
            e.textContent = "No recent decks yet.";
            root.appendChild(e);
            return;
        }
        for (let i = 0; i < recents.length; i++) {
            const r = recents[i];
            const tile = document.createElement("div");
            tile.className = "landing__tile";
            if (r.thumb) {
                mountThumb(tile, r.thumb);
            }
            const card = makeCard(tile, r.title || "Untitled", relativeDate(r.modified),
                { kind: "recent", path: r.path }, function () {
                    post("OpenRecent", { path: r.path });
                });
            root.appendChild(card);
        }
    }

    function renderLayouts(templates) {
        const root = document.getElementById("layouts");
        root.replaceChildren();
        const list = templates || [];
        for (let i = 0; i < list.length; i++) {
            const t = list[i];
            const label = t.layout_name ? t.theme_name + " · " + t.layout_name : t.theme_name;
            const card = makeCard(layoutTile(t), label, "Starter deck",
                { kind: "template", theme_id: t.theme_id, layout_id: t.layout_id }, function () {
                    post("OpenTemplate", { theme_id: t.theme_id, layout_id: t.layout_id });
                });
            root.appendChild(card);
        }
    }

    // onOpen
    // Open the current selection, or a default light deck when nothing is
    // selected.
    function onOpen() {
        if (selection && selection.kind === "template") {
            post("OpenTemplate", { theme_id: selection.theme_id, layout_id: selection.layout_id });
        } else if (selection && selection.kind === "recent") {
            post("OpenRecent", { path: selection.path });
        } else {
            post("OpenDefault");
        }
    }

    // window.__landing.receive — the single inbound entry point. Rust calls it
    // with the JSON string of a LandingData payload.
    window.__landing = {
        receive: function (json) {
            let data;
            try {
                data = JSON.parse(json);
            } catch (e) {
                console.error("landing: bad payload", e);
                return;
            }
            renderRecents(data.recents);
            renderLayouts(data.templates);
        },
    };

    document.addEventListener("DOMContentLoaded", function () {
        const cancel = document.getElementById("landing-cancel");
        const open = document.getElementById("landing-open");
        if (cancel) {
            cancel.addEventListener("click", function () { post("Cancel"); });
        }
        if (open) {
            open.addEventListener("click", onOpen);
        }
        post("Ready");
    });
}());
