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
    // An in-flight slide-transition swap, or null. Holds the old host to drop
    // and the new host to settle once the CSS transition finishes (or times out).
    let pendingSwap = null;

    // buildHost: create a present-host + shadow root for a payload, wired with
    // theme/globals/keyframes/asset-vars CSS. Does NOT attach to the stage.
    function buildHost(payload) {
        const host = document.createElement("div");
        host.className = "present-host";
        const shadow = host.attachShadow({ mode: "open" });
        shadow.innerHTML =
            "<style>" + (payload.theme_css || "") + "</style>"
            + "<style>" + (payload.globals_css || "") + "</style>"
            + "<style>" + keyframesCss + "</style>"
            + "<style id=\"asset-vars\"></style>"
            + (payload.slide_html || "");
        return { host: host, shadow: shadow };
    }

    // adoptHost: make a freshly-built host the current one (current-shadow +
    // asset-var bookkeeping), then resolve its image/media background URLs.
    function adoptHost(built) {
        currentShadow = built.shadow;
        assetVarStyleEl = built.shadow.getElementById("asset-vars");
        refreshAssetVarStyle();
    }

    // finalizeSwap: settle any in-flight transition immediately — drop the old
    // host (or, for Cube, re-parent the new host out of the 3D wrapper and drop
    // the wrapper), clear the new host's inline state and any inline 3D stage
    // props, and adopt the new host. Idempotent.
    function finalizeSwap() {
        if (!pendingSwap) {
            return;
        }
        const swap = pendingSwap;
        pendingSwap = null;
        window.clearTimeout(swap.timer);
        const stage = document.getElementById("stage");
        if (swap.wrapper) {
            // Re-parent the new host to the stage BEFORE removing the wrapper
            // (the new host is currently a child of the wrapper).
            if (stage) {
                stage.appendChild(swap.newBuilt.host);
            }
            if (swap.wrapper.parentNode) {
                swap.wrapper.parentNode.removeChild(swap.wrapper);
            }
        } else if (swap.oldHost && swap.oldHost.parentNode) {
            swap.oldHost.parentNode.removeChild(swap.oldHost);
        }
        swap.newBuilt.host.style.cssText = "";
        if (stage) {
            stage.style.perspective = "";
            stage.style.transformStyle = "";
        }
        adoptHost(swap.newBuilt);
    }

    // mountSlide
    // Inputs: a PresentSlidePayload.
    // Output: side-effect; mounts a fresh shadow-root slide. With no transition
    // (cut), replaces the stage contents instantly. With a Fade/Push transition
    // (forward cross-slide only), stacks the new host over the old and animates
    // the swap, dropping the old host when the CSS transition ends.
    function mountSlide(payload) {
        const stage = document.getElementById("stage");
        if (!stage || !payload) {
            return;
        }
        finalizeSwap();
        const built = buildHost(payload);
        const kind = payload.transition && payload.transition.kind;
        if (!kind || kind === "None") {
            stage.replaceChildren(built.host);
            adoptHost(built);
            return;
        }
        startSwap(stage, built, payload.transition);
    }

    // startSwap: animate the host swap per the transition (Fade | Push).
    //   Push — the new slide stacks ON TOP and slides in over the stationary old
    //     slide. The old fills the stage the whole time, so there is no black
    //     seam between the two panels.
    //   Fade — the new slide is mounted UNDERNEATH at full opacity and the old
    //     fades out on top. The opaque new slide always backs the frame (no
    //     black dip over transparent regions), and the old fully fades so no
    //     stray element on it lingers.
    // The host that actually animates (new for Push, old for Fade) drives the
    // transitionend that finalizes the swap.
    function startSwap(stage, built, transition) {
        const oldHost = currentShadow ? currentShadow.host : null;
        if (!oldHost) {
            stage.replaceChildren(built.host);
            adoptHost(built);
            return;
        }
        const dur = transition.duration_ms || 400;
        const ease = transition.easing || "ease";
        const starters = {
            Push: startPush, Wipe: startWipe, Flip: startFlip,
            Cube: startCube, Dissolve: startDissolve,
        };
        const start = starters[transition.kind] || startFade;
        start(stage, oldHost, built, dur, ease);
    }

    // startPush: new on top, translateX(100%) → 0; old stationary beneath.
    function startPush(stage, oldHost, built, dur, ease) {
        built.host.style.transform = "translateX(100%)";
        stage.appendChild(built.host);
        adoptHost(built);
        void built.host.offsetWidth; // commit the start state before transitioning
        built.host.style.transition = "transform " + dur + "ms " + ease;
        built.host.style.transform = "translateX(0)";
        scheduleSwapEnd(oldHost, built, built.host, "transform", dur);
    }

    // startFade: new beneath at opacity 1; old on top fades opacity 1 → 0.
    function startFade(stage, oldHost, built, dur, ease) {
        stage.insertBefore(built.host, oldHost);
        adoptHost(built);
        void oldHost.offsetWidth; // commit before transitioning the old host out
        oldHost.style.transition = "opacity " + dur + "ms " + ease;
        oldHost.style.opacity = "0";
        scheduleSwapEnd(oldHost, built, oldHost, "opacity", dur);
    }

    // Dissolve blur radius (the leaving slide blurs out, the entering one in).
    const DISSOLVE_BLUR = "blur(12px)";

    // startDissolve: crossfade + blur. Like Fade (new beneath at opacity 1, no
    // black dip) but both hosts also animate blur — old sharpens to blurred as
    // it fades, new starts blurred and sharpens into view as the old clears.
    function startDissolve(stage, oldHost, built, dur, ease) {
        built.host.style.filter = DISSOLVE_BLUR;
        stage.insertBefore(built.host, oldHost);
        adoptHost(built);
        void built.host.offsetWidth; // commit the new host's blurred start state
        built.host.style.transition = "filter " + dur + "ms " + ease;
        built.host.style.filter = "blur(0)";
        oldHost.style.transition =
            "opacity " + dur + "ms " + ease + ", filter " + dur + "ms " + ease;
        oldHost.style.opacity = "0";
        oldHost.style.filter = DISSOLVE_BLUR;
        scheduleSwapEnd(oldHost, built, oldHost, "opacity", dur);
    }

    // startWipe: new ON TOP, revealed left→right via clip-path inset; the old
    // stays put beneath the whole time (cover-style, no seam).
    function startWipe(stage, oldHost, built, dur, ease) {
        built.host.style.clipPath = "inset(0 0 0 100%)";
        stage.appendChild(built.host);
        adoptHost(built);
        void built.host.offsetWidth;
        built.host.style.transition = "clip-path " + dur + "ms " + ease;
        built.host.style.clipPath = "inset(0 0 0 0)";
        scheduleSwapEnd(oldHost, built, built.host, "clip-path", dur);
    }

    // startFlip: 3D Y-axis flip. Old rotates 0 → -90deg over the first half;
    // new rotates 90deg → 0 over the second half (delayed), both backface-hidden
    // so neither shows its reverse mid-flip.
    function startFlip(stage, oldHost, built, dur, ease) {
        stage.style.perspective = "1200px";
        const half = Math.round(dur / 2);
        oldHost.style.backfaceVisibility = "hidden";
        built.host.style.backfaceVisibility = "hidden";
        built.host.style.transform = "rotateY(90deg)";
        stage.appendChild(built.host);
        adoptHost(built);
        void built.host.offsetWidth;
        oldHost.style.transition = "transform " + half + "ms " + ease;
        oldHost.style.transform = "rotateY(-90deg)";
        built.host.style.transition = "transform " + half + "ms " + ease + " " + half + "ms";
        built.host.style.transform = "rotateY(0deg)";
        scheduleSwapEnd(oldHost, built, built.host, "transform", dur);
    }

    // startCube: 3D cube turn. Old is the front face, new the right face, both on
    // a transient preserve-3d wrapper; rotating the wrapper -90deg swings the new
    // face to front. finalizeSwap re-parents the new host out and drops the
    // wrapper (the one structural deviation from the sibling-hosts model).
    function startCube(stage, oldHost, built, dur, ease) {
        stage.style.perspective = "1200px";
        const halfW = (deckW / 2) + "px";
        const wrapper = document.createElement("div");
        wrapper.className = "present-cube";
        wrapper.style.position = "absolute";
        wrapper.style.inset = "0";
        wrapper.style.transformStyle = "preserve-3d";
        wrapper.style.transform = "translateZ(-" + halfW + ")";
        oldHost.style.transform = "rotateY(0deg) translateZ(" + halfW + ")";
        built.host.style.transform = "rotateY(90deg) translateZ(" + halfW + ")";
        stage.appendChild(wrapper);
        wrapper.appendChild(oldHost);
        wrapper.appendChild(built.host);
        adoptHost(built);
        void wrapper.offsetWidth;
        wrapper.style.transition = "transform " + dur + "ms " + ease;
        wrapper.style.transform = "translateZ(-" + halfW + ") rotateY(-90deg)";
        scheduleSwapEnd(oldHost, built, wrapper, "transform", dur, wrapper);
    }

    // scheduleSwapEnd: record the in-flight swap and arm both a transitionend
    // listener (on the animating element) and a safety timeout (duration + 50ms).
    // `wrapper` is the transient 3D container for Cube, else undefined.
    function scheduleSwapEnd(oldHost, built, animEl, prop, dur, wrapper) {
        const timer = window.setTimeout(finalizeSwap, dur + 50);
        pendingSwap = {
            oldHost: oldHost, newBuilt: built, timer: timer, wrapper: wrapper || null,
        };
        animEl.addEventListener("transitionend", function (e) {
            if (e.propertyName === prop) {
                finalizeSwap();
            }
        });
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
