(function () {
    "use strict";

    let deckW = 1920;
    let deckH = 1080;

    let keyframesCss = "";

    let currentShadow = null;
    let inputInstalled = false;

    const assetBlobCache = Object.create(null);
    let assetVarStyleEl = null;
    const MAX_ASSET_ITER = 100000;

    function postControl(kind) {
        if (!window.ipc || typeof window.ipc.postMessage !== "function") {
            console.error("present: window.ipc.postMessage unavailable");
            return;
        }
        window.ipc.postMessage(JSON.stringify({ kind: kind }));
    }

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
            try { URL.revokeObjectURL(prior.url); } catch (_e) {  }
        }
        assetBlobCache[payload.asset_id] = { url: url, media_type: mediaType };
    }

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

    function refreshAssetVarStyle() {
        if (assetVarStyleEl) {
            assetVarStyleEl.textContent = buildAssetVarCss();
        }
    }

    let pendingSwap = null;

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

    function adoptHost(built) {
        currentShadow = built.shadow;
        assetVarStyleEl = built.shadow.getElementById("asset-vars");
        refreshAssetVarStyle();
    }

    function finalizeSwap() {
        if (!pendingSwap) {
            return;
        }
        const swap = pendingSwap;
        pendingSwap = null;
        window.clearTimeout(swap.timer);
        const stage = document.getElementById("stage");
        if (swap.wrapper) {

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

    function mountSlide(payload) {
        const stage = document.getElementById("stage");
        if (!stage || !payload) {
            return;
        }
        finalizeSwap();
        const oldShadow = currentShadow;
        const built = buildHost(payload);
        const kind = payload.transition && payload.transition.kind;
        if (kind && kind !== "None") {
            startSwap(stage, built, payload.transition);
            return;
        }
        const oldHost = oldShadow ? oldShadow.host : null;
        if (payload.transition && oldHost && window.run_morph) {
            stage.appendChild(built.host);
            adoptHost(built);
            window.run_morph(oldShadow, built.shadow, function () {
                if (oldHost.parentNode) {
                    oldHost.parentNode.removeChild(oldHost);
                }
            });
            return;
        }
        stage.replaceChildren(built.host);
        adoptHost(built);
    }

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

    function startPush(stage, oldHost, built, dur, ease) {
        built.host.style.transform = "translateX(100%)";
        stage.appendChild(built.host);
        adoptHost(built);
        void built.host.offsetWidth;
        built.host.style.transition = "transform " + dur + "ms " + ease;
        built.host.style.transform = "translateX(0)";
        scheduleSwapEnd(oldHost, built, built.host, "transform", dur);
    }

    function startFade(stage, oldHost, built, dur, ease) {
        stage.insertBefore(built.host, oldHost);
        adoptHost(built);
        void oldHost.offsetWidth;
        oldHost.style.transition = "opacity " + dur + "ms " + ease;
        oldHost.style.opacity = "0";
        scheduleSwapEnd(oldHost, built, oldHost, "opacity", dur);
    }

    const DISSOLVE_BLUR = "blur(12px)";

    function startDissolve(stage, oldHost, built, dur, ease) {
        built.host.style.filter = DISSOLVE_BLUR;
        stage.insertBefore(built.host, oldHost);
        adoptHost(built);
        void built.host.offsetWidth;
        built.host.style.transition = "filter " + dur + "ms " + ease;
        built.host.style.filter = "blur(0)";
        oldHost.style.transition =
            "opacity " + dur + "ms " + ease + ", filter " + dur + "ms " + ease;
        oldHost.style.opacity = "0";
        oldHost.style.filter = DISSOLVE_BLUR;
        scheduleSwapEnd(oldHost, built, oldHost, "opacity", dur);
    }

    function startWipe(stage, oldHost, built, dur, ease) {
        built.host.style.clipPath = "inset(0 0 0 100%)";
        stage.appendChild(built.host);
        adoptHost(built);
        void built.host.offsetWidth;
        built.host.style.transition = "clip-path " + dur + "ms " + ease;
        built.host.style.clipPath = "inset(0 0 0 0)";
        scheduleSwapEnd(oldHost, built, built.host, "clip-path", dur);
    }

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

    function findElement(id) {
        if (!currentShadow || !id) {
            return null;
        }
        const safe = String(id).replace(/"/g, "\\\"");
        return currentShadow.querySelector('[data-element-id="' + safe + '"]');
    }

    function setVisibility(id, visible) {
        const el = findElement(id);
        if (!el) {
            return;
        }
        el.style.animation = "none";
        el.style.opacity = visible ? "1" : "0";
    }

    function iterationsToCss(iters) {
        if (iters === "Infinite") {
            return "infinite";
        }
        if (iters && typeof iters.Count === "number") {
            return String(iters.Count);
        }
        return "1";
    }

    function playAnimation(a) {
        const el = findElement(a.element_id);
        if (!el) {
            return;
        }
        if (a.targets && a.targets.length > 0) {
            el.style.opacity = "1";
            el.style.transition =
                "all " + a.duration_ms + "ms " + a.easing + " " + a.delay_ms + "ms";

            window.requestAnimationFrame(function () {
                for (let i = 0; i < a.targets.length && i < 1000; i++) {
                    el.style.setProperty(a.targets[i].property, a.targets[i].value);
                }
            });
            return;
        }
        const iters = iterationsToCss(a.iterations);

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

    function applyReveal(payload) {
        if (!payload) {
            return;
        }
        (payload.hidden || []).forEach(function (id) { setVisibility(id, false); });
        (payload.shown || []).forEach(function (id) { setVisibility(id, true); });
        (payload.animate || []).forEach(function (a) { playAnimation(a); });
    }

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

            refreshAssetVarStyle();
        },
        PresentSlide: function (payload) {
            mountSlide(payload);
        },
        PresentReveal: function (payload) {
            applyReveal(payload);
        },
    };

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

    if (document.readyState === "loading") {
        document.addEventListener("DOMContentLoaded", function () { postControl("Ready"); });
    } else {
        postControl("Ready");
    }
})();
