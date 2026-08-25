(function () {
    "use strict";

    let deckW = 1920;
    let deckH = 1080;

    const assetBlobCache = Object.create(null);
    const MAX_ASSET_ITER = 100000;
    const MAX_HIDDEN_ITER = 100000;

    const mounted = { current: null, next: null };
    let lastPayload = null;
    let updateCount = 0;

    let timerRunning = false;
    let timerStartedAt = 0;
    let timerBankedMs = 0;

    function postControl(kind) {
        if (!window.ipc || typeof window.ipc.postMessage !== "function") {
            console.error("presenter: window.ipc.postMessage unavailable");
            return;
        }
        window.ipc.postMessage(JSON.stringify({ kind: kind }));
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
            console.error("presenter: base64 decode failed", e);
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
            const entry = assetBlobCache[keys[i]];
            if (entry && entry.url) {
                parts.push("  --asset-" + keys[i] + ": url(" + entry.url + ");");
            }
        }
        parts.push("}");
        return parts.join("\n");
    }

    function hideElements(shadow, ids) {
        const list = ids || [];
        for (let i = 0; i < list.length && i < MAX_HIDDEN_ITER; i++) {
            const safe = String(list[i]).replace(/"/g, "\\\"");
            const el = shadow.querySelector('[data-element-id="' + safe + '"]');
            if (el) {
                el.style.opacity = "0";
            }
        }
    }

    function scalePreview(entry) {
        if (!entry || !entry.slot || !entry.host) {
            return;
        }
        const sw = entry.slot.clientWidth / deckW;
        const sh = entry.slot.clientHeight / deckH;
        const scale = Math.min(sw, sh);
        entry.host.style.transform = "scale(" + (scale > 0 ? scale : 0) + ")";
    }

    function mountPreview(slotId, html, hidden, payload) {
        const slot = document.getElementById(slotId);
        if (!slot) {
            return null;
        }
        if (!html) {
            slot.replaceChildren();
            return null;
        }
        const host = document.createElement("div");
        host.className = "preview-host";
        host.style.width = deckW + "px";
        host.style.height = deckH + "px";
        const shadow = host.attachShadow({ mode: "open" });
        shadow.innerHTML =
            "<style>" + (payload.theme_css || "") + "</style>"
            + "<style>" + (payload.globals_css || "") + "</style>"
            + "<style>" + buildAssetVarCss() + "</style>"
            + html;
        hideElements(shadow, hidden);
        slot.replaceChildren(host);
        const entry = { slot: slot, host: host };
        scalePreview(entry);
        return entry;
    }

    function renderUpdate(payload) {
        if (!payload) {
            return;
        }
        lastPayload = payload;
        if (payload.width > 0) { deckW = payload.width; }
        if (payload.height > 0) { deckH = payload.height; }
        mounted.current = mountPreview(
            "current-slot", payload.current_html, payload.current_hidden, payload
        );
        mounted.next = mountPreview(
            "next-slot", payload.next_html, payload.next_hidden, payload
        );
        const counter = document.getElementById("counter");
        if (counter) {
            counter.textContent = (payload.index + 1) + " / " + payload.count;
        }
        const notes = document.getElementById("notes");
        if (notes) {
            notes.textContent = payload.notes || "";
        }
    }

    function elapsedMs() {
        if (!timerRunning) {
            return timerBankedMs;
        }
        return timerBankedMs + (Date.now() - timerStartedAt);
    }

    function formatElapsed(ms) {
        const total = Math.floor(ms / 1000);
        const hours = Math.floor(total / 3600);
        const mins = Math.floor((total % 3600) / 60);
        const secs = total % 60;
        const mm = String(mins).padStart(2, "0");
        const ss = String(secs).padStart(2, "0");
        return hours > 0 ? hours + ":" + mm + ":" + ss : mm + ":" + ss;
    }

    function renderTimer() {
        const el = document.getElementById("timer");
        if (el) {
            el.textContent = formatElapsed(elapsedMs());
        }
    }

    function startTimer() {
        timerRunning = true;
        timerStartedAt = Date.now();
        setToggleLabel();
    }

    function toggleTimer() {
        if (timerRunning) {
            timerBankedMs = elapsedMs();
            timerRunning = false;
            setToggleLabel();
            renderTimer();
            return;
        }
        startTimer();
    }

    function resetTimer() {
        timerBankedMs = 0;
        timerStartedAt = Date.now();
        renderTimer();
    }

    function setToggleLabel() {
        const btn = document.getElementById("timer-toggle");
        if (btn) {
            btn.textContent = timerRunning ? "pause" : "start";
        }
    }

    function onKey(e) {
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
        } else if (k === "r" || k === "R") {
            resetTimer();
        } else if (k === "p" || k === "P") {
            toggleTimer();
        }
    }

    function bindButton(id, fn) {
        const el = document.getElementById(id);
        if (el) {
            el.addEventListener("click", fn);
        }
    }

    function installInput() {
        document.addEventListener("keydown", onKey);
        bindButton("prev-btn", function () { postControl("Back"); });
        bindButton("next-btn", function () { postControl("Advance"); });
        bindButton("exit-btn", function () { postControl("Exit"); });
        bindButton("timer-toggle", toggleTimer);
        bindButton("timer-reset", resetTimer);
        setToggleLabel();
    }

    const handlers = {
        PresentAssets: function (payload) {
            const assets = (payload && payload.assets) || [];
            assets.forEach(function (a) { ingestAssetPayload(a); });
            if (lastPayload) {
                renderUpdate(lastPayload);
            }
        },
        PresenterUpdate: function (payload) {
            renderUpdate(payload);
            updateCount += 1;

            if (updateCount > 1 && !timerRunning && timerBankedMs === 0) {
                startTimer();
            }
        },
    };

    window.__deck = {
        receive: function (envelopeJson) {
            let msg;
            try {
                msg = JSON.parse(envelopeJson);
            } catch (e) {
                console.error("presenter receive: invalid JSON", e);
                return;
            }
            const handler = handlers[msg.type];
            if (handler) {
                handler(msg.payload);
            } else {
                console.warn("presenter receive: unhandled type", msg.type);
            }
        },
    };

    window.addEventListener("resize", function () {
        scalePreview(mounted.current);
        scalePreview(mounted.next);
    });

    window.setInterval(renderTimer, 250);
    installInput();

    if (document.readyState === "loading") {
        document.addEventListener("DOMContentLoaded", function () {
            postControl("PresenterReady");
        });
    } else {
        postControl("PresenterReady");
    }
})();
