// Presentation-mode frontend.
//
// A deliberately dumb renderer. Rust (the brain) owns the cursor and computes
// each step's resolved visual state; this script only:
//   1. reports Ready, then receives PresentInit / PresentSlide / PresentReveal,
//   2. mounts a slide into a scaled, letterboxed stage,
//   3. applies hidden / shown / animate to elements by data-element-id,
//   4. forwards key / click controls back to Rust.
//
// Inbound controls are envelope-free ({ "kind": "Advance" }); outbound payloads
// from Rust ride the standard IpcMessage envelope ({ id, timestamp, type,
// payload }), exactly like the editor's __deck bridge.
(function () {
    "use strict";

    // Deck authored pixel size; replaced by PresentInit. Drives stage scaling.
    let deckW = 1920;
    let deckH = 1080;
    // The built-in @keyframes library (from PresentInit), injected into every
    // mounted slide's shadow root alongside theme + globals CSS.
    let keyframesCss = "";
    // The current slide's shadow root (reveal targets live inside it).
    let currentShadow = null;
    let inputInstalled = false;
    // asset_id -> { url: blob URL, media_type }. Built from PresentAssets bytes
    // (a blob URL minted in the editor webview is invalid here, so we mint our
    // own). assetVarStyleEl is the <style> in the current shadow root mapping
    // :host { --asset-<id>: url(blob:…) } so image/media elements resolve.
    const assetBlobCache = Object.create(null);
    let assetVarStyleEl = null;
    const MAX_ASSET_ITER = 100000;

    // ---------- outbound controls ----------
    // postControl
    // Inputs: a control kind ("Ready" | "Advance" | "Back" | "Exit").
    // Output: side-effect; posts the envelope-free control to Rust.
    function postControl(kind) {
        if (!window.ipc || typeof window.ipc.postMessage !== "function") {
            console.error("present: window.ipc.postMessage unavailable");
            return;
        }
        window.ipc.postMessage(JSON.stringify({ kind: kind }));
    }

    // ---------- scaling ----------
    // computeScale
    // Output: side-effect; scales the stage to fit the window while preserving
    // the deck aspect ratio (contain). Letterbox bars are the black root.
    function computeScale() {
        const stage = document.getElementById("stage");
        if (!stage) {
            return;
        }
        const sw = window.innerWidth / deckW;
        const sh = window.innerHeight / deckH;
        const scale = Math.min(sw, sh);
        stage.style.width = deckW + "px";
        stage.style.height = deckH + "px";
        stage.style.transform = "scale(" + scale + ")";
    }

    // ---------- assets ----------
    // base64ToUint8Array: decode standard-alphabet base64 to bytes, or null.
    function base64ToUint8Array(b64) {
        try {
            const binary = window.atob(b64);
            const len = binary.length;
            const out = new Uint8Array(len);
            for (let i = 0; i < len; i++) {
                out[i] = binary.charCodeAt(i);
            }
            return out;
        } catch (e) {
            console.error("present: base64 decode failed", e);
            return null;
        }
    }

    // ingestAssetPayload: decode one { asset_id, media_type, content_base64 }
    // into a Blob + object URL, cached under asset_id (revoking any prior URL).
    function ingestAssetPayload(payload) {
        if (!payload || !payload.asset_id || !payload.content_base64) {
            return;
        }
        const bytes = base64ToUint8Array(payload.content_base64);
        if (!bytes) {
            return;
        }
        const mediaType = payload.media_type || "application/octet-stream";
        const url = URL.createObjectURL(new Blob([bytes], { type: mediaType }));
        const prior = assetBlobCache[payload.asset_id];
        if (prior && prior.url) {
            try { URL.revokeObjectURL(prior.url); } catch (_e) { /* noop */ }
        }
        assetBlobCache[payload.asset_id] = { url: url, media_type: mediaType };
    }

    // buildAssetVarCss: a :host { --asset-<id>: url(blob:…); } block for every
    // cached asset, or "" when empty. Mirrors the editor so image/media
    // elements (background-image: var(--asset-<id>)) resolve identically.
    function buildAssetVarCss() {
        const keys = Object.keys(assetBlobCache);
        if (keys.length === 0) {
            return "";
        }
        const parts = [":host {"];
        for (let i = 0; i < keys.length && i < MAX_ASSET_ITER; i++) {
            const id = keys[i];
            const entry = assetBlobCache[id];
            if (entry && entry.url) {
                parts.push("  --asset-" + id + ": url(" + entry.url + ");");
            }
        }
        parts.push("}");
        return parts.join("\n");
    }

    // refreshAssetVarStyle: rewrite the current shadow's asset-vars <style>.
    function refreshAssetVarStyle() {
        if (assetVarStyleEl) {
            assetVarStyleEl.textContent = buildAssetVarCss();
        }
    }

    // ---------- mounting ----------
    // mountSlide
    // Inputs: a PresentSlidePayload.
    // Output: side-effect; replaces the stage contents with a fresh shadow
    // root containing theme + globals + keyframes + asset-vars CSS, then the
    // slide HTML. The asset-vars block resolves image/media background URLs.
    function mountSlide(payload) {
        const stage = document.getElementById("stage");
        if (!stage || !payload) {
            return;
        }
        const host = document.createElement("div");
        host.className = "present-host";
        const shadow = host.attachShadow({ mode: "open" });
        shadow.innerHTML =
            "<style>" + (payload.theme_css || "") + "</style>"
            + "<style>" + (payload.globals_css || "") + "</style>"
            + "<style>" + keyframesCss + "</style>"
            + "<style id=\"asset-vars\"></style>"
            + (payload.slide_html || "");
        stage.replaceChildren(host);
        currentShadow = shadow;
        assetVarStyleEl = shadow.getElementById("asset-vars");
        refreshAssetVarStyle();
    }

    // ---------- reveal ----------
    // findElement: locate a slide element by its data-element-id.
    function findElement(id) {
        if (!currentShadow || !id) {
            return null;
        }
        const safe = String(id).replace(/"/g, "\\\"");
        return currentShadow.querySelector('[data-element-id="' + safe + '"]');
    }

    // setVisibility: snap an element visible/hidden with no animation.
    function setVisibility(id, visible) {
        const el = findElement(id);
        if (!el) {
            return;
        }
        el.style.animation = "none";
        el.style.opacity = visible ? "1" : "0";
    }

    // iterationsToCss: map the AnimationIterations serde shape to a CSS token.
    // Count(n) serializes as { "Count": n }; Infinite as the string "Infinite".
    function iterationsToCss(iters) {
        if (iters === "Infinite") {
            return "infinite";
        }
        if (iters && typeof iters.Count === "number") {
            return String(iters.Count);
        }
        return "1";
    }

    // playAnimation: run one keyframe now; resolve to its end state on finish.
    function playAnimation(a) {
        const el = findElement(a.element_id);
        if (!el) {
            return;
        }
        if (a.targets && a.targets.length > 0) {
            el.style.opacity = "1";
            el.style.transition =
                "all " + a.duration_ms + "ms " + a.easing + " " + a.delay_ms + "ms";
            // Apply targets next frame so the transition observes the change.
            window.requestAnimationFrame(function () {
                for (let i = 0; i < a.targets.length && i < 1000; i++) {
                    el.style.setProperty(a.targets[i].property, a.targets[i].value);
                }
            });
            return;
        }
        const iters = iterationsToCss(a.iterations);
        // Visible while the keyframe plays; the keyframe's own from/to controls
        // the opacity ramp (e.g. appear 0->1, disappear 1->0).
        el.style.opacity = "1";
        el.style.animation =
            a.keyframe + " " + a.duration_ms + "ms " + a.easing
            + " " + a.delay_ms + "ms " + iters + " both";
        const onEnd = function () {
            el.style.animation = "none";
            el.style.opacity = a.ends_hidden ? "0" : "1";
            el.removeEventListener("animationend", onEnd);
        };
        el.addEventListener("animationend", onEnd);
    }

    // applyReveal
    // Inputs: a RevealPayload.
    // Output: side-effect; snaps hidden/shown elements and plays the animate
    // set. Each managed element appears in exactly one bucket.
    function applyReveal(payload) {
        if (!payload) {
            return;
        }
        (payload.hidden || []).forEach(function (id) { setVisibility(id, false); });
        (payload.shown || []).forEach(function (id) { setVisibility(id, true); });
        (payload.animate || []).forEach(function (a) { playAnimation(a); });
    }

    // ---------- input ----------
    // installInput: wire key + click controls once (after PresentInit).
    function installInput() {
        if (inputInstalled) {
            return;
        }
        inputInstalled = true;
        document.addEventListener("keydown", function (e) {
            const k = e.key;
            if (k === "ArrowRight" || k === "ArrowDown" || k === " "
                || k === "Spacebar" || k === "Enter") {
                e.preventDefault();
                postControl("Advance");
            } else if (k === "ArrowLeft" || k === "ArrowUp") {
                e.preventDefault();
                postControl("Back");
            } else if (k === "Escape") {
                e.preventDefault();
                postControl("Exit");
            }
        });
        document.addEventListener("click", function () { postControl("Advance"); });
    }

    // ---------- inbound dispatch ----------
    const handlers = {
        PresentInit: function (payload) {
            if (payload) {
                keyframesCss = payload.animation_keyframes_css || "";
                if (payload.width > 0) { deckW = payload.width; }
                if (payload.height > 0) { deckH = payload.height; }
            }
            computeScale();
            installInput();
        },
        PresentAssets: function (payload) {
            const assets = (payload && payload.assets) || [];
            assets.forEach(function (a) { ingestAssetPayload(a); });
            // If a slide is already mounted, resolve its images now.
            refreshAssetVarStyle();
        },
        PresentSlide: function (payload) {
            mountSlide(payload);
        },
        PresentReveal: function (payload) {
            applyReveal(payload);
        },
    };

    // ---------- bridge ----------
    // Named __deck because the shared Rust-side WebviewSender invokes
    // `window.__deck.receive(...)` for every webview it owns (editor and
    // presentation alike); only the handled message types differ.
    window.__deck = {
        receive: function (envelopeJson) {
            let msg;
            try {
                msg = JSON.parse(envelopeJson);
            } catch (e) {
                console.error("present receive: invalid JSON", e);
                return;
            }
            const handler = handlers[msg.type];
            if (handler) {
                handler(msg.payload);
            } else {
                console.warn("present receive: unhandled type", msg.type);
            }
        },
    };

    window.addEventListener("resize", computeScale);
    // Announce readiness once the document is parsed.
    if (document.readyState === "loading") {
        document.addEventListener("DOMContentLoaded", function () { postControl("Ready"); });
    } else {
        postControl("Ready");
    }
})();
