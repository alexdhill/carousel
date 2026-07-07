// player.js — standalone offline presentation player. Reads window.__DECK
// (baked by build_html_export) and walks (slideIdx, step) frames with arrow
// keys, applying precomputed reveal payloads exactly like presentation mode.
(function () {
    "use strict";
    var DECK = window.__DECK || { slides: [], width: 1920, height: 1080 };
    var slideIdx = 0;
    var step = 0;
    var currentShadow = null;
    // Decoded-image references held so the browser keeps them cached (prevents
    // the visible fetch/decode lag the first time each image is mounted).
    var preloaded = [];

    // preloadAssets: fetch + decode every asset up front into the browser cache
    // so mounting a slide that references it is instant.
    function preloadAssets() {
        var assets = DECK.assets || [];
        for (var i = 0; i < assets.length; i++) {
            var img = new Image();
            img.src = new URL(assets[i].path, document.baseURI).href;
            if (typeof img.decode === "function") {
                img.decode().catch(function () { /* ignore decode errors */ });
            }
            preloaded.push(img);
        }
    }

    // assetVarsCss: build :host { --asset-<id>: url("<absolute>"); } resolving
    // each asset's relative path to an ABSOLUTE URL against the document. A
    // relative url() inside a custom property in a shadow root does not reliably
    // resolve against the document base, so it must be absolutized here.
    function assetVarsCss() {
        var assets = DECK.assets || [];
        if (!assets.length) { return ""; }
        var parts = [":host {"];
        for (var i = 0; i < assets.length; i++) {
            var abs = new URL(assets[i].path, document.baseURI).href;
            parts.push("  --asset-" + assets[i].id + ": url(\"" + abs + "\");");
        }
        parts.push("}");
        return parts.join("\n");
    }

    // mountSlide: replace the stage with a fresh shadow root containing the
    // theme + globals + keyframes + asset-vars CSS and the slide HTML, then
    // size + scale the stage to fit the window.
    function mountSlide(i) {
        var stage = document.getElementById("stage");
        if (!stage || !DECK.slides[i]) { return; }
        var host = document.createElement("div");
        host.className = "present-host";
        var shadow = host.attachShadow({ mode: "open" });
        shadow.innerHTML =
            "<style>" + (DECK.theme_css || "") + "</style>"
            + "<style>" + (DECK.globals_css || "") + "</style>"
            + "<style>" + (DECK.keyframes_css || "") + "</style>"
            + "<style>" + assetVarsCss() + "</style>"
            // Mask content beyond the slide bounds (independent of the deck's
            // theme age, which may predate the .slide overflow rule).
            + "<style>.slide { overflow: hidden; }</style>"
            + (DECK.slides[i].html || "");
        stage.replaceChildren(host);
        currentShadow = shadow;
        applyStageScale();
    }

    function applyStageScale() {
        var stage = document.getElementById("stage");
        if (!stage) { return; }
        var w = DECK.width || 1920;
        var h = DECK.height || 1080;
        var s = Math.min(window.innerWidth / w, window.innerHeight / h);
        stage.style.width = w + "px";
        stage.style.height = h + "px";
        stage.style.transform = "scale(" + s + ")";
    }

    function findElement(id) {
        if (!currentShadow || !id) { return null; }
        var safe = String(id).replace(/"/g, "\\\"");
        return currentShadow.querySelector('[data-element-id="' + safe + '"]');
    }

    function setVisibility(id, visible) {
        var el = findElement(id);
        if (!el) { return; }
        el.style.animation = "none";
        el.style.opacity = visible ? "1" : "0";
    }

    function iterationsToCss(iters) {
        if (iters === "Infinite") { return "infinite"; }
        if (iters && typeof iters.Count === "number") { return String(iters.Count); }
        return "1";
    }

    function playAnimation(a) {
        var el = findElement(a.element_id);
        if (!el) { return; }
        el.style.opacity = "1";
        el.style.animation = a.keyframe + " " + a.duration_ms + "ms " + a.easing
            + " " + a.delay_ms + "ms " + iterationsToCss(a.iterations) + " both";
        var onEnd = function () {
            el.style.animation = "none";
            el.style.opacity = a.ends_hidden ? "0" : "1";
            el.removeEventListener("animationend", onEnd);
        };
        el.addEventListener("animationend", onEnd);
    }

    // applyReveal: snap hidden/shown, play the animate set (mirrors present.js).
    function applyReveal(payload) {
        if (!payload) { return; }
        (payload.hidden || []).forEach(function (id) { setVisibility(id, false); });
        (payload.shown || []).forEach(function (id) { setVisibility(id, true); });
        (payload.animate || []).forEach(function (a) { playAnimation(a); });
    }

    function slide() { return DECK.slides[slideIdx]; }
    function lastStep() { return Math.max(0, (slide().snaps || []).length - 1); }

    // mount_over: mount slide i layered over the current host, returning both roots.
    // Returns {old_root, new_root} where old_root is the current shadow, and
    // new_root is the newly-mounted shadow. Used for morph animations that need
    // both the old and new slide content visible simultaneously.
    function mount_over(i) {
        var stage = document.getElementById("stage");
        if (!stage || !DECK.slides[i]) { return null; }
        var old_shadow = currentShadow;
        var host = document.createElement("div");
        host.className = "present-host";
        var shadow = host.attachShadow({ mode: "open" });
        shadow.innerHTML =
            "<style>" + (DECK.theme_css || "") + "</style>"
            + "<style>" + (DECK.globals_css || "") + "</style>"
            + "<style>" + (DECK.keyframes_css || "") + "</style>"
            + "<style>" + assetVarsCss() + "</style>"
            + "<style>.slide { overflow: hidden; }</style>"
            + (DECK.slides[i].html || "");
        stage.appendChild(host);
        currentShadow = shadow;
        return { old_root: old_shadow, new_root: shadow };
    }

    // drop_old_root: remove the old host from the stage.
    function drop_old_root(old_root) {
        if (old_root && old_root.host && old_root.host.parentNode) {
            old_root.host.parentNode.removeChild(old_root.host);
        }
    }

    // goToSlide: mount slide i parked at parkStep, snapped (no animation).
    function goToSlide(i, parkStep) {
        slideIdx = i;
        mountSlide(i);
        step = parkStep;
        applyReveal(slide().snaps[step]);
    }

    function advance() {
        if (step < lastStep()) {
            step += 1;
            applyReveal(slide().forwards[step]); // animation plays
        } else if (slideIdx < DECK.slides.length - 1) {
            var nextIdx = slideIdx + 1;
            if (window.run_morph) {
                var roots = mount_over(nextIdx);
                if (roots) {
                    slideIdx = nextIdx;
                    step = 0;
                    applyReveal(slide().snaps[step]);
                    var old_root = roots.old_root;
                    window.run_morph(old_root, roots.new_root, function () {
                        drop_old_root(old_root);
                    });
                    return;
                }
            }
            goToSlide(nextIdx, 0);
        }
    }

    function back() {
        if (step > 0) {
            step -= 1;
            applyReveal(slide().snaps[step]);
        } else if (slideIdx > 0) {
            var prev = DECK.slides[slideIdx - 1];
            goToSlide(slideIdx - 1, Math.max(0, (prev.snaps || []).length - 1));
        }
    }

    function toggleFullscreen() {
        if (!document.fullscreenElement) {
            (document.documentElement.requestFullscreen || function () {}).call(document.documentElement);
        } else {
            (document.exitFullscreen || function () {}).call(document);
        }
    }

    window.addEventListener("keydown", function (e) {
        if (e.key === "ArrowRight" || e.key === " " || e.key === "Spacebar"
                || e.key === "Enter" || e.key === "PageDown") {
            e.preventDefault();
            advance();
        } else if (e.key === "ArrowLeft" || e.key === "PageUp") {
            e.preventDefault();
            back();
        } else if (e.key === "f" || e.key === "F") {
            e.preventDefault();
            toggleFullscreen();
        }
    });
    window.addEventListener("click", function () { advance(); });
    window.addEventListener("resize", applyStageScale);

    preloadAssets();
    if (DECK.slides.length > 0) {
        goToSlide(0, 0);
    }
}());
