// host.js
//
// Webview-side of the IPC bridge plus the editor's interaction loop.
//
// Stage 5 wires:
//   - mousedown/mousemove/mouseup on the viewport, with shadow-DOM
//     composedPath to identify the element under the cursor.
//   - A 3px drag threshold separating click from drag.
//   - Optimistic CSS transform during drag (no layout thrash).
//   - rAF-throttled ElementDragged IPC (~one message per frame).
//   - SetSelection handler that draws selection boxes in the host's
//     #selection-overlay container (not inside the shadow root).
//   - Selection overlay reposition during drag.

(function () {
    "use strict";

    // ---------- state ----------
    let currentShadow = null;
    let currentSlideHost = null;
    // Pixel-grid snapping: session-only, never persisted or sent to Rust.
    // Read into the snap engine's opts.gridEnabled each gesture move.
    let gridEnabled = false;
    // Which editor region last received interaction; drives delete/copy/cut
    // targeting. One of "objects" | "preview" | "navigator". Default preview.
    let focusRegion = "preview";
    const FOCUS_CONTAINERS = {
        objects: "object-panel",
        preview: "viewport-container",
        navigator: "thumbnail-row",
    };
    // Crop mode: holds { elementId, assetId, mask, natural, state, preStyle }
    // while an image is being cropped. null outside crop mode. The committed
    // element is never mutated during the session — cancel is a clean teardown.
    let cropState = null;
    let cropPan = null;
    let cropResize = null;
    let dragState = null;
    let pendingDrag = null;
    let dragRafScheduled = false;
    // Marquee (drag-to-select) session, null when idle. Armed on a background
    // press; becomes active once the pointer crosses DRAG_THRESHOLD.
    let marquee = null;
    let currentSelectionIds = [];
    // slideSelected — true only when the slide itself is the selection (an
    // explicit thumbnail click), distinct from "nothing selected". Clicking
    // negative space (slide background, around the thumbnails) leaves both this
    // and the element selection empty, so nothing is highlighted. Managed by the
    // click handlers, NOT inferred from an empty element selection.
    let slideSelected = false;
    // Slide zoom. "fit" recomputes a width-fit scale on every layout change;
    // "manual" pins zoomManualPct (50–250, stepped by 10). The viewport's CSS
    // transform is the single source of truth read back by getViewportScale.
    let zoomMode = "fit";
    let zoomManualPct = 100;
    const ZOOM_MIN = 50;
    const ZOOM_MAX = 250;
    const ZOOM_STEP = 10;
    // Pan offset (screen px) applied as a translate alongside the zoom scale.
    // Clamped so the slide edge never pulls past the pane edge; always 0 when
    // the scaled slide fits the pane (so panning has no effect when fitted).
    let panX = 0;
    let panY = 0;
    // Active canvas tool: "select" (default) or "hand" (drag pans, no select).
    let activeTool = "select";
    let panSession = null;
    // Rulers & guides. Guides are saveable (persisted per slide/layout on the
    // Rust side) and editor-only — they live outside the element tree, so they
    // never appear in presentation/export. The active canvas's guides arrive via
    // GuidesUpdate: `guideOwn` are this canvas's editable guides, `guideInherited`
    // are its layout's guides (read-only on a slide; empty when editing a layout).
    // A guide is { id, index, orient: "h"|"v", pos } where pos is in slide pixels
    // (0 at the slide's top-left). `id` is "g"+index (own) / "gi"+index
    // (inherited): stable across the post-commit re-hydration so a selection
    // survives a move. Horizontal guides come from the top ruler (move in Y),
    // vertical from the left ruler (move in X).
    let rulersOn = false;
    const RULER = 18;
    let guideOwn = [];
    let guideInherited = [];
    let selectedGuideId = null;
    let guideDragSession = null;
    let guideSeq = 0;
    // focusChain — group ids the editor has entered (empty = top level). A
    // click resolves to the deepest focused group's child; double-click drills.
    let focusChain = [];

    // elementChain — the data-element-id ancestry of a node, innermost→outermost,
    // bounded by .slide-host.
    function elementChain(node) {
        const out = [];
        let n = node;
        let guard = 0;
        while (n && guard < 1000) {
            guard += 1;
            if (n.classList && n.classList.contains("slide-host")) { break; }
            if (n.dataset && n.dataset.elementId) { out.push(n); }
            n = n.parentElement || (n.getRootNode && n.getRootNode().host);
        }
        return out;
    }
    // pendingDragEnds: id -> DOM node. When mouseup fires we keep the
    // optimistic transform on each dragged element so there is no visible flash
    // between the transform clearing and the absolute-position patch landing.
    // Each entry's transform is removed inside applyOnePatch the moment a
    // SetStyle(left|top) patch for that id arrives. A safety timeout clears any
    // stragglers after PENDING_TRANSFORM_TIMEOUT_MS. A map (not a single slot)
    // so a multi-select drag can hold every moved element at once.
    const pendingDragEnds = Object.create(null);
    // textEditState: non-null while a text element is being edited inline
    // (double-click). Holds the element id, the contenteditable DOM node,
    // its text at edit-start (for cancel), and the keydown/blur listeners
    // so they can be detached on finish. See beginTextEdit / finishTextEdit.
    let textEditState = null;

    const DRAG_THRESHOLD = 3;
    const MAX_BATCH_ITER = 100000;
    const PENDING_TRANSFORM_TIMEOUT_MS = 200;
    // Resizable panes (session-only). Canvas floor captured once at launch =
    // its size in the default spawn window; panes may grow only into the spare
    // room a larger window provides. Mins are the CSS defaults; fixed maxes are
    // 750 (side panes) / 500 (thumbs). See resizable-panes spec.
    let canvasMinW = 0;
    let canvasMinH = 0;
    const PANE_MIN = { objects: 240, inspector: 300, thumbs: 160 };
    const PANE_MAX = { objects: 750, inspector: 750, thumbs: 500 };
    let paneDragSession = null;
    // assetBlobCache: asset_id -> { url: blob URL, media_type } so the
    // slide's CSS custom properties can resolve to image URLs.
    // assetVarStyleEl: the <style> node injected into the active shadow
    // root that maps :host { --asset-<id>: url(<blob-url>); }.
    const assetBlobCache = Object.create(null);
    let assetVarStyleEl = null;
    // Deck-wide globals CSS (Stage 11). Injected into every shadow root —
    // the viewport mount and every thumbnail — between theme CSS and the
    // asset-vars block. Refreshed by MountSlide / LayoutListUpdate.
    let currentGlobalsCss = "";
    // The active editor mode ("slide" | "layout"), echoed by the Rust side
    // via SetMode. Drives body[data-mode] and which list the row shows.
    let currentMode = "slide";
    // The immutable built-in @keyframes library (delivered once via Configure)
    // injected into every shadow root for forthcoming playback.
    let builtinKeyframesCss = "";
    // The effect catalog (from Configure): the single source for the add-menu
    // and per-bar effect picker.
    let animationCatalog = [];
    // The active slide's animation timeline (from SlideAnimationsUpdate); the
    // animations panel filters this by the selected id and renders a bar stack.
    let slideAnimations = [];
    // animation_id -> true while its bar is expanded (survives refreshes).
    const animExpanded = {};
    // Guards the editor build preview so it cannot re-enter / double-restore.
    let animPreviewActive = false;
    // The active slide's inspector data (from SlideInspectorUpdate); rendered in
    // the Slide box when nothing is selected in slide mode.
    let slideInspectorData = null;
    // The active layout's background (from LayoutListUpdate); feeds the Slide
    // box's Fill/Image controls when editing a layout in layout mode.
    let layoutBgData = null;

    // ---------- envelope id ----------
    function newId() {
        if (window.crypto && typeof window.crypto.randomUUID === "function") {
            return window.crypto.randomUUID();
        }
        return "js_" + Math.random().toString(36).slice(2) + Date.now().toString(36);
    }

    // ---------- mounting ----------
    // mountSlide
    // Inputs: slideId, slideHtml, themeCss.
    // Output: side-effect; replaces #viewport's slide-host with a fresh
    // div whose shadow root contains theme CSS + slide HTML. Caches the
    // shadow root and host for the selection overlay + patch applier.
    function mountSlide(slideId, slideHtml, themeCss, globalsCss) {
        if (typeof globalsCss === "string") {
            currentGlobalsCss = globalsCss;
        }
        const viewport = document.getElementById("viewport");
        if (!viewport) {
            console.error("mountSlide: #viewport not found");
            return;
        }
        // A remount replaces the shadow DOM, so any in-progress text edit
        // is referencing a node that is about to be discarded. Abandon the
        // session silently (the node is gone; there is nothing to commit).
        textEditState = null;
        const host = document.createElement("div");
        host.className = "slide-host";
        host.dataset.slideId = slideId;
        const shadow = host.attachShadow({ mode: "open" });
        // Three top-level children inside the shadow root:
        //   <style id="theme-css">...</style>     theme tokens
        //   <style id="asset-vars">...</style>    --asset-* → url(blob:)
        //   ...slide HTML...
        // The asset-vars block is rebuilt by refreshAssetVarStyle() so
        // image elements (whose inline_styles set
        //   background-image: var(--asset-<id>);
        // ) resolve to actual blob URLs.
        shadow.innerHTML = "<style>" + themeCss + "</style>"
            + "<style id=\"globals-css\">" + currentGlobalsCss + "</style>"
            + "<style id=\"anim-kf\">" + builtinKeyframesCss + "</style>"
            + "<style id=\"asset-vars\"></style>"
            // Edit-mode only: reveal content positioned beyond the slide bounds
            // (the canvas scrim greys it). Present/export/thumbnails omit this,
            // so the theme's .slide overflow:hidden crops them.
            + "<style id=\"edit-overflow\">.slide{overflow:visible}</style>"
            + slideHtml;
        viewport.replaceChildren(host);
        currentShadow = shadow;
        currentSlideHost = host;
        assetVarStyleEl = shadow.getElementById("asset-vars");
        refreshAssetVarStyle();
        // Selection from the previous slide does not transfer.
        currentSelectionIds = [];
        clearSelectionOverlay();
        // Re-apply the current zoom to the fresh host (fit recomputes for the
        // new slide's dimensions).
        applyZoom();
    }

    // ingestAssetPayload
    // Inputs: an AssetPayload-shaped object { asset_id, media_type,
    // content_base64 }.
    // Output: side-effect; decodes the base64 bytes into a Blob,
    // creates a URL.createObjectURL handle, caches under asset_id.
    // Replacing an existing entry revokes the prior blob URL so we
    // don't leak.
    function ingestAssetPayload(payload) {
        if (!payload || !payload.asset_id || !payload.content_base64) {
            return;
        }
        const bytes = base64ToUint8Array(payload.content_base64);
        if (!bytes) {
            return;
        }
        const mediaType = payload.media_type || "application/octet-stream";
        const blob = new Blob([bytes], { type: mediaType });
        const url = URL.createObjectURL(blob);
        const prior = assetBlobCache[payload.asset_id];
        if (prior && prior.url) {
            try { URL.revokeObjectURL(prior.url); } catch (_e) { /* noop */ }
        }
        assetBlobCache[payload.asset_id] = {
            url: url,
            media_type: mediaType,
            original_filename: payload.original_filename || "",
        };
    }

    // assetFilename
    // Inputs: an asset id. Output: its original filename, or "" when unknown.
    function assetFilename(assetId) {
        const entry = assetBlobCache[assetId];
        return (entry && entry.original_filename) || "";
    }

    // base64ToUint8Array
    // Inputs: a standard-alphabet base64 string.
    // Output: a Uint8Array of the decoded bytes, or null on failure.
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
            console.error("base64 decode failed:", e);
            return null;
        }
    }

    // refreshAssetVarStyle
    // Inputs: none (reads assetBlobCache + currentShadow).
    // Output: side-effect; rewrites the asset-vars <style> tag's text
    // content so every cached asset id maps to its current blob URL.
    // Dataflow: build a single :host { ... } block listing every entry
    // in assetBlobCache. Re-runs whenever the cache changes OR a new
    // shadow root is mounted.
    function refreshAssetVarStyle() {
        if (!assetVarStyleEl) {
            return;
        }
        assetVarStyleEl.textContent = buildAssetVarCss();
    }

    // buildAssetVarCss
    // Inputs: none (reads assetBlobCache).
    // Output: a :host { --asset-<id>: url(blob:…); … } CSS string, or
    // "" when no assets are cached. Shared by the viewport's
    // asset-vars <style> and every thumbnail's shadow root so image
    // elements resolve identically everywhere.
    function buildAssetVarCss() {
        const keys = Object.keys(assetBlobCache);
        if (keys.length === 0) {
            return "";
        }
        const parts = [":host {"];
        let iter = 0;
        for (let i = 0; i < keys.length; i++) {
            if (iter >= MAX_BATCH_ITER) {
                break;
            }
            const id = keys[i];
            const entry = assetBlobCache[id];
            if (!entry || !entry.url) {
                continue;
            }
            // CSS custom property names allow alphanumeric + hyphen +
            // underscore. asset_ids are produced by the Rust side as
            // "asset_<hex>" so they pass cleanly without escaping.
            parts.push("  --asset-" + id + ": url(" + entry.url + ");");
            iter += 1;
        }
        parts.push("}");
        return parts.join("\n");
    }

    // getViewportScale
    // Inputs: none (reads #viewport's computed transform).
    // Output: the horizontal scale factor of the viewport's CSS transform
    // (1.0 when no transform is set). Used to convert window-CSS-pixel
    // drag deltas into slide-coordinate-pixel deltas so the optimistic
    // transform and the absolute-position commit agree.
    // Dataflow: parses `matrix(a, b, c, d, e, f)` from getComputedStyle;
    // `a` is scaleX.
    function getViewportScale() {
        const viewport = document.getElementById("viewport");
        if (!viewport) {
            return 1;
        }
        const computed = window.getComputedStyle(viewport);
        const t = computed.transform;
        if (!t || t === "none") {
            return 1;
        }
        const m = t.match(/matrix\(([^)]+)\)/);
        if (!m) {
            return 1;
        }
        const parts = m[1].split(",").map(function (s) { return parseFloat(s); });
        if (parts.length < 4) {
            return 1;
        }
        const a = parts[0];
        if (!isFinite(a) || a === 0) {
            return 1;
        }
        return a;
    }

    // ---------- slide zoom ----------
    // computeFitScale
    // Output: scale that fits the slide width inside the canvas pane (with a
    // little breathing room), or null when the slide/pane is not measurable.
    // The slide-host's offsetWidth is its UNSCALED layout width (the CSS
    // transform does not affect layout boxes), so it is the true slide width.
    function computeFitScale() {
        const stage = document.getElementById("viewport-container");
        const host = currentSlideHost;
        if (!stage || !host) {
            return null;
        }
        const w = host.offsetWidth;
        const avail = stage.clientWidth - 32;
        if (w <= 0 || avail <= 0) {
            return null;
        }
        return avail / w;
    }

    // effectiveZoomScale
    // Output: the scale to apply — the fit scale in "fit" mode (falling back to
    // the manual pct if unmeasurable), else the manual pct as a fraction.
    function effectiveZoomScale() {
        if (zoomMode === "fit") {
            const f = computeFitScale();
            if (f && isFinite(f) && f > 0) {
                return f;
            }
        }
        return zoomManualPct / 100;
    }

    // applyZoom
    // Output: side-effect; writes the viewport transform, updates the readout
    // ("Fit" or "NN%"), and re-syncs the selection overlay (which is measured
    // in screen pixels and so must follow the scale).
    // panBounds — max |pan| on each axis = half the overflow of the scaled
    // slide past the pane (the viewport is centred, so it can shift each way by
    // that much). Zero when the slide fits → panning is a no-op when fitted.
    function panBounds() {
        const stage = document.getElementById("viewport-container");
        const host = currentSlideHost;
        if (!stage || !host) {
            return { x: 0, y: 0 };
        }
        const s = effectiveZoomScale();
        const sw = (host.offsetWidth || 1920) * s;
        const sh = (host.offsetHeight || 1080) * s;
        return {
            x: Math.max(0, (sw - stage.clientWidth) / 2),
            y: Math.max(0, (sh - stage.clientHeight) / 2),
        };
    }

    function clampPan() {
        const b = panBounds();
        panX = Math.max(-b.x, Math.min(b.x, panX));
        panY = Math.max(-b.y, Math.min(b.y, panY));
    }

    function applyZoom() {
        clampPan();
        const viewport = document.getElementById("viewport");
        if (viewport) {
            viewport.style.transform =
                "translate(" + panX + "px," + panY + "px) scale(" + effectiveZoomScale() + ")";
        }
        const pct = document.getElementById("zoom-pct");
        if (pct) {
            pct.textContent = (zoomMode === "fit")
                ? "Fit"
                : (Math.round(zoomManualPct) + "%");
        }
        if (currentSelectionIds.length > 0) {
            updateSelectionOverlay();
        }
        refreshRulers();
        renderRulerGuides();
        renderCanvasScrim();
    }

    // setZoomFit / zoomStep
    // setZoomFit returns to width-fit. zoomStep leaves fit (snapping the fit
    // percentage to the nearest 10 first so steps stay round) and nudges the
    // manual zoom by ±ZOOM_STEP, clamped to [ZOOM_MIN, ZOOM_MAX].
    function setZoomFit() {
        zoomMode = "fit";
        panX = 0;
        panY = 0;
        applyZoom();
    }

    function zoomStep(delta) {
        let base = zoomManualPct;
        if (zoomMode === "fit") {
            const f = computeFitScale();
            base = Math.round(((f || 1) * 100) / ZOOM_STEP) * ZOOM_STEP;
        }
        let next = base + delta;
        if (next < ZOOM_MIN) { next = ZOOM_MIN; }
        if (next > ZOOM_MAX) { next = ZOOM_MAX; }
        zoomMode = "manual";
        zoomManualPct = next;
        applyZoom();
    }

    // ---------- hand / pan tool ----------
    // setTool — switch between "select" and "hand". Updates the toolbar pressed
    // state and the canvas cursor (grab in hand mode).
    function setTool(name) {
        activeTool = (name === "hand") ? "hand" : "select";
        const sel = document.getElementById("tool-select");
        const hand = document.getElementById("tool-hand");
        if (sel) { sel.classList.toggle("is-on", activeTool === "select"); }
        if (hand) { hand.classList.toggle("is-on", activeTool === "hand"); }
        const stage = document.getElementById("viewport-container");
        if (stage) {
            stage.style.cursor = activeTool === "hand" ? "grab" : "";
        }
    }

    function onPanMouseMove(e) {
        if (!panSession) {
            return;
        }
        panX = panSession.basePanX + (e.clientX - panSession.startX);
        panY = panSession.basePanY + (e.clientY - panSession.startY);
        applyZoom();
    }

    function onPanMouseUp() {
        panSession = null;
        document.body.style.userSelect = "";
        const stage = document.getElementById("viewport-container");
        if (stage && activeTool === "hand") { stage.style.cursor = "grab"; }
        window.removeEventListener("mousemove", onPanMouseMove);
        window.removeEventListener("mouseup", onPanMouseUp);
    }

    // ---------- patch applier ----------
    // findElement
    // Inputs: an element id.
    // Output: the matching DOM Element inside currentShadow, or null.
    function findElement(id) {
        if (!currentShadow) {
            return null;
        }
        const safe = (window.CSS && window.CSS.escape) ? window.CSS.escape(id) : id;
        return currentShadow.querySelector('[data-element-id="' + safe + '"]');
    }

    // applyOnePatch
    // Inputs: a single (non-Batch) patch object.
    // Output: side-effect on the DOM.
    function applyOnePatch(patch) {
        if (patch.op === "InsertElement") {
            const parent = findElement(patch.parent_id);
            if (!parent) {
                console.warn("InsertElement: parent not found", patch.parent_id);
                return;
            }
            const tmp = document.createElement("div");
            tmp.innerHTML = patch.html;
            const newEl = tmp.firstElementChild;
            if (!newEl) {
                console.warn("InsertElement: html produced no element");
                return;
            }
            const refNode = parent.children[patch.position] || null;
            parent.insertBefore(newEl, refNode);
            return;
        }
        const el = findElement(patch.element_id);
        if (!el) {
            console.warn("patch target not found:", patch.element_id, "op:", patch.op);
            return;
        }
        switch (patch.op) {
            case "SetAttribute":
                el.setAttribute(patch.attribute, patch.value);
                break;
            case "RemoveAttribute":
                el.removeAttribute(patch.attribute);
                break;
            case "SetStyle":
                el.style.setProperty(patch.property, patch.value);
                // Clear the optimistic drag transform the moment the
                // authoritative absolute position arrives from Rust.
                if (pendingDragEnds[patch.element_id] &&
                        (patch.property === "left" || patch.property === "top")) {
                    pendingDragEnds[patch.element_id].style.removeProperty("transform");
                    delete pendingDragEnds[patch.element_id];
                }
                break;
            case "RemoveStyle":
                el.style.removeProperty(patch.property);
                break;
            case "SetText":
                el.textContent = patch.text;
                if (typeof patch.src === "string") {
                    el.dataset.src = patch.src;
                } else if (el.dataset) {
                    delete el.dataset.src;
                }
                break;
            case "SetInnerHtml":
                el.innerHTML = patch.html;
                break;
            case "ReplaceElement": {
                const tmp = document.createElement("div");
                tmp.innerHTML = patch.new_html;
                const newEl = tmp.firstElementChild;
                if (newEl && el.parentNode) {
                    el.parentNode.replaceChild(newEl, el);
                }
                break;
            }
            case "RemoveElement":
                if (el.parentNode) {
                    el.parentNode.removeChild(el);
                }
                break;
            default:
                console.warn("unknown patch op:", patch.op);
        }
    }

    // applyPatch
    // Inputs: a top-level patch (possibly a Batch wrapping more patches).
    // Output: side-effect; applies every patch in source order using an
    // explicit stack — no recursion — so a deep Batch cannot blow the
    // JS stack.
    function applyPatch(rootPatch) {
        const stack = [rootPatch];
        let iter = 0;
        while (stack.length > 0 && iter < MAX_BATCH_ITER) {
            iter++;
            const p = stack.pop();
            if (p && p.op === "Batch" && Array.isArray(p.patches)) {
                for (let i = p.patches.length - 1; i >= 0; i--) {
                    stack.push(p.patches[i]);
                }
                continue;
            }
            if (p) {
                applyOnePatch(p);
            }
        }
        if (iter >= MAX_BATCH_ITER) {
            console.warn("applyPatch hit MAX_BATCH_ITER; truncating");
        }
        // After any patch, reposition the selection overlay because
        // element geometry may have changed (e.g., MoveElement → SetStyle).
        if (currentSelectionIds.length > 0) {
            updateSelectionOverlay();
        }
    }

    // ---------- selection overlay ----------
    // clearSelectionOverlay
    // Inputs: none.
    // Output: side-effect; removes all box children from #selection-overlay.
    function clearSelectionOverlay() {
        const overlay = document.getElementById("selection-overlay");
        if (overlay) {
            overlay.replaceChildren();
        }
    }

    // updateSelectionOverlay
    // Inputs: none (reads currentSelectionIds + currentSlideHost).
    // Output: side-effect; redraws one absolutely-positioned box per
    // selected element using getBoundingClientRect coordinates. Positions
    // are computed relative to #viewport-container so the boxes track the
    // slide host's transform (e.g., scale).
    // Selection box outline offset (px) so the blue rectangle sits a
    // hair outside the element rather than clipping its edge.
    const SELECTION_OUTSET_PX = 0;
    // Handle order matches CSS [data-handle="…"]. The (dx, dy) pair is
    // the handle's offset within the selection box, expressed as
    // fractions (0..1) of width/height.
    const SELECTION_HANDLES = [
        { name: "nw", fx: 0,   fy: 0   },
        { name: "n",  fx: 0.5, fy: 0   },
        { name: "ne", fx: 1,   fy: 0   },
        { name: "e",  fx: 1,   fy: 0.5 },
        { name: "se", fx: 1,   fy: 1   },
        { name: "s",  fx: 0.5, fy: 1   },
        { name: "sw", fx: 0,   fy: 1   },
        { name: "w",  fx: 0,   fy: 0.5 },
    ];

    function updateSelectionOverlay() {
        const overlay = document.getElementById("selection-overlay");
        if (!overlay) {
            return;
        }
        overlay.replaceChildren();
        // While cropping, the crop overlay owns the element's chrome; drawing
        // selection handles here would overlap and steal resize gestures.
        if (cropState) {
            return;
        }
        if (!currentShadow || !currentSlideHost) {
            return;
        }
        // Table focus mode: draw the cell selection instead of the element box.
        if (tableCellSel && focusedTableId() === tableCellSel.elementId) {
            renderCellSelection(overlay);
            return;
        }
        if (currentSelectionIds.length === 0) {
            return;
        }
        const overlayRect = overlay.getBoundingClientRect();
        const showHandles = currentSelectionIds.length === 1;
        const multi = currentSelectionIds.length > 1;
        let unionL = Infinity, unionT = Infinity, unionR = -Infinity, unionB = -Infinity;
        for (let i = 0; i < currentSelectionIds.length; i++) {
            const id = currentSelectionIds[i];
            const safe = (window.CSS && window.CSS.escape) ? window.CSS.escape(id) : id;
            const el = currentShadow.querySelector('[data-element-id="' + safe + '"]');
            if (!el) {
                continue;
            }
            const rect = el.getBoundingClientRect();
            if (multi) {
                unionL = Math.min(unionL, rect.left);
                unionT = Math.min(unionT, rect.top);
                unionR = Math.max(unionR, rect.right);
                unionB = Math.max(unionB, rect.bottom);
            }
            const outset = SELECTION_OUTSET_PX;
            const boxLeft = rect.left - overlayRect.left - outset;
            const boxTop = rect.top - overlayRect.top - outset;
            const boxWidth = rect.width + 2 * outset;
            const boxHeight = rect.height + 2 * outset;
            const box = document.createElement("div");
            box.className = "selection-box";
            box.style.position = "absolute";
            box.style.left = boxLeft + "px";
            box.style.top = boxTop + "px";
            box.style.width = boxWidth + "px";
            box.style.height = boxHeight + "px";
            box.style.border = "1.5px dashed var(--acc)";
            box.style.pointerEvents = "none";
            box.style.boxSizing = "border-box";
            overlay.appendChild(box);

            if (showHandles) {
                const isGroup = el.dataset.elementType === "group";
                for (let h = 0; h < SELECTION_HANDLES.length; h++) {
                    const spec = SELECTION_HANDLES[h];
                    if (isGroup && spec.name.length === 1) { continue; } // skip edges n/e/s/w
                    const handle = document.createElement("div");
                    handle.className = "selection-handle";
                    handle.dataset.handle = spec.name;
                    handle.dataset.elementId = id;
                    handle.style.left = (boxLeft + spec.fx * boxWidth) + "px";
                    handle.style.top = (boxTop + spec.fy * boxHeight) + "px";
                    handle.addEventListener("mousedown", onResizeHandleMouseDown);
                    overlay.appendChild(handle);
                }
            }
        }
        // Multi-selection: a union bounding box with corner-only handles for
        // proportional scaling of the whole set.
        if (multi && unionR > unionL && unionB > unionT) {
            const bx = unionL - overlayRect.left;
            const by = unionT - overlayRect.top;
            const bw = unionR - unionL;
            const bh = unionB - unionT;
            const box = document.createElement("div");
            box.className = "selection-box selection-box--multi";
            box.style.position = "absolute";
            box.style.left = bx + "px";
            box.style.top = by + "px";
            box.style.width = bw + "px";
            box.style.height = bh + "px";
            box.style.pointerEvents = "none";
            box.style.boxSizing = "border-box";
            overlay.appendChild(box);
            const corners = [
                { name: "nw", fx: 0, fy: 0 },
                { name: "ne", fx: 1, fy: 0 },
                { name: "se", fx: 1, fy: 1 },
                { name: "sw", fx: 0, fy: 1 },
            ];
            for (let h = 0; h < corners.length; h++) {
                const c = corners[h];
                const handle = document.createElement("div");
                handle.className = "selection-handle";
                handle.dataset.handle = c.name;
                handle.dataset.multiScale = "1";
                handle.style.left = (bx + c.fx * bw) + "px";
                handle.style.top = (by + c.fy * bh) + "px";
                handle.addEventListener("mousedown", onMultiScaleMouseDown);
                overlay.appendChild(handle);
            }
        }
    }

    // ===================== table cell editing (focus mode) =====================
    // When a table is the deepest focus (entered via double-click, like a
    // group), clicks select cells by (row, col) instead of dragging the element.
    // The selected-cell set lives only here; it is serialized into the command
    // messages so Rust stays stateless about cell selection.
    // tableCellSel: { elementId, anchor: [r,c], cells: [[r,c], ...] } | null
    let tableCellSel = null;

    // focusedTableId — the deepest focused element id if it is a table, else
    // null. Used to gate cell-selection behavior in the pointer pipeline.
    function focusedTableId() {
        if (focusChain.length === 0 || !currentShadow) {
            return null;
        }
        const top = focusChain[focusChain.length - 1];
        const safe = (window.CSS && window.CSS.escape) ? window.CSS.escape(top) : top;
        const el = currentShadow.querySelector('[data-element-id="' + safe + '"]');
        return (el && el.dataset.elementType === "table") ? top : null;
    }

    // tableCellGrid — the rendered cells of a table as grid[r][c] = { r, c, td }.
    // Row index is the <tr> index; column index is the cell index within the row
    // (v1 has no merged cells, so this is a plain rectangle).
    function tableCellGrid(tableId) {
        const safe = (window.CSS && window.CSS.escape) ? window.CSS.escape(tableId) : tableId;
        const wrap = currentShadow && currentShadow.querySelector('[data-element-id="' + safe + '"]');
        const table = wrap && wrap.querySelector("table");
        if (!table) {
            return [];
        }
        const trs = table.querySelectorAll("tr");
        const grid = [];
        for (let r = 0; r < trs.length; r++) {
            const cellEls = trs[r].querySelectorAll("td, th");
            const row = [];
            for (let c = 0; c < cellEls.length; c++) {
                row.push({ r: r, c: c, td: cellEls[c] });
            }
            grid.push(row);
        }
        return grid;
    }

    // cellAtPoint — the [r, c] of the cell under a client point, or null.
    function cellAtPoint(tableId, clientX, clientY) {
        const grid = tableCellGrid(tableId);
        for (let r = 0; r < grid.length; r++) {
            for (let c = 0; c < grid[r].length; c++) {
                const rect = grid[r][c].td.getBoundingClientRect();
                if (clientX >= rect.left && clientX <= rect.right
                        && clientY >= rect.top && clientY <= rect.bottom) {
                    return [r, c];
                }
            }
        }
        return null;
    }

    function cellKey(rc) {
        return rc[0] + "," + rc[1];
    }

    // rangeCells — every [r,c] in the rectangle spanned by two corners.
    function rangeCells(a, b) {
        const r0 = Math.min(a[0], b[0]), r1 = Math.max(a[0], b[0]);
        const c0 = Math.min(a[1], b[1]), c1 = Math.max(a[1], b[1]);
        const out = [];
        for (let r = r0; r <= r1; r++) {
            for (let c = c0; c <= c1; c++) {
                out.push([r, c]);
            }
        }
        return out;
    }

    // selectCell — update the cell selection from a click: plain click selects
    // one (new anchor); Shift extends a rectangular range from the anchor;
    // Cmd/Ctrl toggles an individual cell.
    function selectCell(tableId, rc, e) {
        const sameTable = tableCellSel && tableCellSel.elementId === tableId;
        if (sameTable && e && e.shiftKey) {
            tableCellSel.cells = rangeCells(tableCellSel.anchor, rc);
        } else if (sameTable && e && (e.metaKey || e.ctrlKey)) {
            const k = cellKey(rc);
            const idx = tableCellSel.cells.findIndex(function (x) { return cellKey(x) === k; });
            if (idx >= 0) {
                tableCellSel.cells.splice(idx, 1);
            } else {
                tableCellSel.cells.push(rc);
            }
            tableCellSel.anchor = rc;
        } else {
            tableCellSel = { elementId: tableId, anchor: rc, cells: [rc] };
        }
        updateSelectionOverlay();
        refreshInspector();
    }

    // selectAllCells — select every cell (whole-table styling affordance).
    function selectAllCells(tableId) {
        const grid = tableCellGrid(tableId);
        const all = [];
        for (let r = 0; r < grid.length; r++) {
            for (let c = 0; c < grid[r].length; c++) {
                all.push([r, c]);
            }
        }
        tableCellSel = { elementId: tableId, anchor: [0, 0], cells: all };
        updateSelectionOverlay();
        refreshInspector();
    }

    function clearTableCellSel() {
        if (tableCellSel) {
            tableCellSel = null;
            updateSelectionOverlay();
            refreshInspector();
        }
    }

    // renderCellSelection — accent outline over each selected cell, drawn into
    // the selection overlay in place of the element box/handles.
    function renderCellSelection(overlay) {
        const overlayRect = overlay.getBoundingClientRect();
        const grid = tableCellGrid(tableCellSel.elementId);
        for (let i = 0; i < tableCellSel.cells.length; i++) {
            const rc = tableCellSel.cells[i];
            const cellObj = grid[rc[0]] && grid[rc[0]][rc[1]];
            if (!cellObj) {
                continue;
            }
            const rect = cellObj.td.getBoundingClientRect();
            const box = document.createElement("div");
            box.className = "selection-box selection-box--cell";
            box.style.position = "absolute";
            box.style.left = (rect.left - overlayRect.left) + "px";
            box.style.top = (rect.top - overlayRect.top) + "px";
            box.style.width = rect.width + "px";
            box.style.height = rect.height + "px";
            box.style.pointerEvents = "none";
            box.style.boxSizing = "border-box";
            overlay.appendChild(box);
        }
    }

    // beginCellEdit — inline-edit one cell's text (contenteditable on the <td>).
    // Enter / blur commit via CellTextEditRequested; Escape cancels. The slide
    // remounts on commit (ReplaceElement), replacing the editable node.
    function beginCellEdit(tableId, rc) {
        const grid = tableCellGrid(tableId);
        const cellObj = grid[rc[0]] && grid[rc[0]][rc[1]];
        if (!cellObj) {
            return;
        }
        const td = cellObj.td;
        td.setAttribute("contenteditable", "true");
        td.focus();
        const range = document.createRange();
        range.selectNodeContents(td);
        const sel = window.getSelection();
        sel.removeAllRanges();
        sel.addRange(range);
        function finish(commit) {
            td.removeEventListener("blur", onBlur);
            td.removeEventListener("keydown", onKey);
            td.removeAttribute("contenteditable");
            if (commit) {
                window.__deck.send("Interaction", {
                    kind: "CellTextEditRequested",
                    element_id: tableId,
                    row: rc[0],
                    col: rc[1],
                    text: td.textContent,
                });
            }
        }
        function onBlur() { finish(true); }
        function onKey(ev) {
            ev.stopPropagation();
            if (ev.key === "Enter" && !ev.shiftKey) {
                ev.preventDefault();
                td.blur();
            } else if (ev.key === "Escape") {
                ev.preventDefault();
                finish(false);
            }
        }
        td.addEventListener("blur", onBlur);
        td.addEventListener("keydown", onKey);
    }

    // tableContextId — the table id the inspector's Table section acts on: the
    // focused table (cell mode) or a singly-selected table element, else null.
    function tableContextId() {
        if (tableCellSel) {
            return tableCellSel.elementId;
        }
        if (currentSelectionIds.length === 1 && currentShadow) {
            const id = currentSelectionIds[0];
            const safe = (window.CSS && window.CSS.escape) ? window.CSS.escape(id) : id;
            const el = currentShadow.querySelector('[data-element-id="' + safe + '"]');
            if (el && el.dataset.elementType === "table") {
                return id;
            }
        }
        return null;
    }

    function tableAnchor() {
        return (tableCellSel && tableCellSel.anchor) ? tableCellSel.anchor : [0, 0];
    }

    function renderedTable(tableId) {
        const safe = (window.CSS && window.CSS.escape) ? window.CSS.escape(tableId) : tableId;
        return currentShadow && currentShadow.querySelector('[data-element-id="' + safe + '"] table');
    }

    // refreshTableBox — show the Table inspector section for a table context and
    // sync the header toggles from the rendered table's data-* attrs.
    function refreshTableBox() {
        const box = document.getElementById("table-box");
        if (!box) {
            return;
        }
        const tid = tableContextId();
        box.hidden = !tid;
        if (!tid) {
            return;
        }
        const table = renderedTable(tid);
        const hr = table ? parseInt(table.getAttribute("data-header-rows") || "0", 10) : 0;
        const hc = table ? parseInt(table.getAttribute("data-header-columns") || "0", 10) : 0;
        const rowChk = document.getElementById("table-header-row");
        const colChk = document.getElementById("table-header-col");
        if (rowChk) { rowChk.checked = hr > 0; }
        if (colChk) { colChk.checked = hc > 0; }
    }

    function tableSend(kind, extra) {
        const tid = tableContextId();
        if (!tid) {
            return;
        }
        window.__deck.send("Interaction", Object.assign({ kind: kind, element_id: tid }, extra || {}));
    }

    // wireTableBox — bind the Table section's structural buttons + header
    // toggles. Insert lands after the anchor cell; delete removes the anchor's
    // row/column.
    function wireTableBox() {
        const bind = function (id, fn) {
            const el = document.getElementById(id);
            if (el) { el.addEventListener(id.indexOf("header") >= 0 ? "change" : "click", fn); }
        };
        bind("table-add-row", function () { tableSend("TableInsertRow", { at: tableAnchor()[0] + 1 }); });
        bind("table-del-row", function () { tableSend("TableDeleteRow", { at: tableAnchor()[0] }); });
        bind("table-add-col", function () { tableSend("TableInsertColumn", { at: tableAnchor()[1] + 1 }); });
        bind("table-del-col", function () { tableSend("TableDeleteColumn", { at: tableAnchor()[1] }); });
        bind("table-header-row", function () {
            const c = document.getElementById("table-header-row");
            tableSend("TableSetHeaderRows", { count: c && c.checked ? 1 : 0 });
        });
        bind("table-header-col", function () {
            const c = document.getElementById("table-header-col");
            tableSend("TableSetHeaderColumns", { count: c && c.checked ? 1 : 0 });
        });
    }

    // ---------- snap guides ----------
    // ensureGuideLayer
    // Inputs: none. Output: the #snap-guides element, created once as a
    // SIBLING of #selection-overlay inside #viewport-container. It must not
    // live inside #selection-overlay because updateSelectionOverlay() calls
    // overlay.replaceChildren() each frame, which would wipe the guides.
    function ensureGuideLayer() {
        const container = document.getElementById("viewport-container");
        if (!container) {
            return null;
        }
        let layer = document.getElementById("snap-guides");
        if (!layer) {
            layer = document.createElement("div");
            layer.id = "snap-guides";
            container.appendChild(layer);
        }
        return layer;
    }

    // slideToScreen
    // Inputs: the guide layer element. Output: { ox, oy, scale } mapping slide
    // coordinates to layer-local px: screen = origin + coord*scale. Reads the
    // slide-host rect once; returns null when unavailable. The slide-host is
    // the shadow HOST (light-DOM div), cached as currentSlideHost — it is not
    // a descendant of its own shadow root, so a shadow-internal query would
    // never find it.
    function slideToScreen(layer) {
        const host = currentSlideHost;
        if (!host || !layer) {
            return null;
        }
        const hr = host.getBoundingClientRect();
        const lr = layer.getBoundingClientRect();
        return { ox: hr.left - lr.left, oy: hr.top - lr.top, scale: hr.width / 1920 };
    }

    // ---------- rulers & guides ----------
    // canvasMetrics — map slide pixels to viewport-container-local px, plus the
    // slide's natural size. screen = origin + slidePx * scale. Null when no
    // slide is mounted.
    function canvasMetrics() {
        const host = currentSlideHost;
        const stage = document.getElementById("viewport-container");
        if (!host || !stage) {
            return null;
        }
        const hr = host.getBoundingClientRect();
        const sr = stage.getBoundingClientRect();
        const w = host.offsetWidth || 1920;
        const h = host.offsetHeight || 1080;
        return {
            ox: hr.left - sr.left, oy: hr.top - sr.top,
            scale: hr.width / w, slideW: w, slideH: h,
            stageW: sr.width, stageH: sr.height,
        };
    }

    // renderCanvasScrim — grey the area outside the slide bounds in edit mode.
    // Four translucent canvas-colored rects fill the viewport minus the slide
    // rect, so content positioned off-slide (shown via the edit-overflow style)
    // fades toward the canvas colour. Pointer-events:none → interaction passes
    // through. Present/export crop instead (no scrim, .slide overflow:hidden).
    function renderCanvasScrim() {
        const stage = document.getElementById("viewport-container");
        if (!stage) {
            return;
        }
        let layer = document.getElementById("canvas-scrim");
        if (!layer) {
            layer = document.createElement("div");
            layer.id = "canvas-scrim";
            for (let i = 0; i < 4; i++) {
                const r = document.createElement("div");
                r.className = "canvas-scrim__rect";
                layer.appendChild(r);
            }
            stage.appendChild(layer);
        }
        const m = canvasMetrics();
        if (!m) {
            layer.style.display = "none";
            return;
        }
        layer.style.display = "block";
        const sw = m.slideW * m.scale;
        const sh = m.slideH * m.scale;
        const r = layer.children;
        // top, bottom, left, right of the slide rect (clamped to >= 0).
        const set = function (el, x, y, w, h) {
            el.style.left = x + "px";
            el.style.top = y + "px";
            el.style.width = Math.max(0, w) + "px";
            el.style.height = Math.max(0, h) + "px";
        };
        set(r[0], 0, 0, m.stageW, m.oy);
        set(r[1], 0, m.oy + sh, m.stageW, m.stageH - (m.oy + sh));
        set(r[2], 0, m.oy, m.ox, sh);
        set(r[3], m.ox + sw, m.oy, m.stageW - (m.ox + sw), sh);
    }

    // ensureRulers — create the two ruler canvases + corner once, wiring the
    // drag-out-a-guide gesture on each ruler.
    function ensureRulers() {
        const stage = document.getElementById("viewport-container");
        if (!stage || document.getElementById("ruler-top")) {
            return;
        }
        const top = document.createElement("canvas");
        top.id = "ruler-top";
        top.className = "ruler ruler--top";
        top.addEventListener("mousedown", function (e) { startGuideCreate(e, "h"); });
        const left = document.createElement("canvas");
        left.id = "ruler-left";
        left.className = "ruler ruler--left";
        left.addEventListener("mousedown", function (e) { startGuideCreate(e, "v"); });
        const corner = document.createElement("div");
        corner.id = "ruler-corner";
        corner.className = "ruler-corner";
        stage.append(top, left, corner);
    }

    // toggleRulers — Cmd+R.
    function toggleRulers() {
        rulersOn = !rulersOn;
        ensureRulers();
        refreshRulers();
        renderRulerGuides();
    }

    // refreshRulers — show/draw or hide the rulers for the current zoom/slide.
    function refreshRulers() {
        const top = document.getElementById("ruler-top");
        const left = document.getElementById("ruler-left");
        const corner = document.getElementById("ruler-corner");
        if (!top || !left || !corner) {
            return;
        }
        const show = rulersOn;
        top.style.display = show ? "block" : "none";
        left.style.display = show ? "block" : "none";
        corner.style.display = show ? "block" : "none";
        if (!show) {
            return;
        }
        const m = canvasMetrics();
        if (m) {
            drawRuler(top, m, "h");
            drawRuler(left, m, "v");
        }
    }

    // rulerStep — slide-px between labelled ticks so labels stay ~64px apart.
    function rulerStep(scale) {
        const cands = [1, 2, 5, 10, 20, 25, 50, 100, 200, 250, 500, 1000, 2000, 5000];
        for (let i = 0; i < cands.length; i++) {
            if (cands[i] * scale >= 64) {
                return cands[i];
            }
        }
        return cands[cands.length - 1];
    }

    // drawRuler — paint ticks/labels in slide pixels onto a ruler canvas.
    // Only the span that lies over the slide (0..slideDim) gets ticks; the rest
    // (e.g. when zoomed out) stays blank.
    function drawRuler(cv, m, orient) {
        const horiz = orient === "h";
        const cssW = horiz ? m.stageW : RULER;
        const cssH = horiz ? RULER : m.stageH;
        const dpr = window.devicePixelRatio || 1;
        cv.width = Math.max(1, Math.round(cssW * dpr));
        cv.height = Math.max(1, Math.round(cssH * dpr));
        cv.style.width = cssW + "px";
        cv.style.height = cssH + "px";
        const ctx = cv.getContext("2d");
        ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
        ctx.clearRect(0, 0, cssW, cssH);
        const css = getComputedStyle(document.body);
        ctx.fillStyle = css.getPropertyValue("--panel") || "#f4f1ea";
        ctx.fillRect(0, 0, cssW, cssH);
        const ink = (css.getPropertyValue("--ink3") || "#9a9384").trim();
        ctx.strokeStyle = ink;
        ctx.fillStyle = ink;
        ctx.font = "9px ui-monospace, Menlo, monospace";
        const step = rulerStep(m.scale);
        const minor = step / 5;
        const origin = horiz ? m.ox : m.oy;
        const slideDim = horiz ? m.slideW : m.slideH;
        const limit = horiz ? m.stageW : m.stageH;
        ctx.beginPath();
        for (let p = 0; p <= slideDim + 0.5; p += minor) {
            const s = origin + p * m.scale;
            if (s < RULER - 0.5 || s > limit) {
                continue;
            }
            const major = Math.abs(p % step) < 0.001;
            const len = major ? RULER : (RULER * 0.4);
            if (horiz) {
                ctx.moveTo(s + 0.5, RULER); ctx.lineTo(s + 0.5, RULER - len);
            } else {
                ctx.moveTo(RULER, s + 0.5); ctx.lineTo(RULER - len, s + 0.5);
            }
            if (major) {
                drawRulerLabel(ctx, Math.round(p), s, horiz);
            }
        }
        ctx.stroke();
    }

    function drawRulerLabel(ctx, value, s, horiz) {
        const txt = String(value);
        if (horiz) {
            ctx.fillText(txt, s + 2, 8);
        } else {
            ctx.save();
            ctx.translate(8, s - 2);
            ctx.rotate(-Math.PI / 2);
            ctx.fillText(txt, 0, 0);
            ctx.restore();
        }
    }

    // ---- guides ----
    function ensureGuideOverlay() {
        const stage = document.getElementById("viewport-container");
        if (!stage) {
            return null;
        }
        let layer = document.getElementById("guide-layer");
        if (!layer) {
            layer = document.createElement("div");
            layer.id = "guide-layer";
            stage.appendChild(layer);
        }
        return layer;
    }

    // currentGuides — the active canvas's own (editable) guides.
    function currentGuides() {
        return guideOwn;
    }

    // sendGuideEvent — dispatch one guide interaction to Rust (which owns the
    // saveable guide state and echoes a GuidesUpdate that re-hydrates the
    // overlay). `kind` is GuideAdded / GuideMoved / GuideRemoved.
    function sendGuideEvent(payload) {
        window.__deck.send("Interaction", payload);
    }

    // renderRulerGuides — redraw the active canvas's guides at the current
    // zoom: inherited (layout) guides first, read-only and beneath the editable
    // own guides. (Distinct from the snap engine's renderGuides.)
    function renderRulerGuides() {
        const layer = ensureGuideOverlay();
        if (!layer) {
            return;
        }
        layer.replaceChildren();
        if (!rulersOn) {
            return;
        }
        const m = canvasMetrics();
        if (!m) {
            return;
        }
        for (let i = 0; i < guideInherited.length && i < 512; i++) {
            layer.appendChild(buildGuideLine(guideInherited[i], m, true));
        }
        for (let i = 0; i < guideOwn.length && i < 512; i++) {
            layer.appendChild(buildGuideLine(guideOwn[i], m, false));
        }
    }

    function buildGuideLine(g, m, readOnly) {
        const line = document.createElement("div");
        line.className = "guide";
        if (readOnly) {
            line.classList.add("guide--inherited");
        } else if (g.id === selectedGuideId) {
            line.classList.add("guide--selected");
        }
        line.dataset.guideId = g.id;
        if (g.orient === "h") {
            line.classList.add("guide--h");
            line.style.top = (m.oy + g.pos * m.scale) + "px";
            line.style.left = m.ox + "px";
            line.style.width = (m.slideW * m.scale) + "px";
        } else {
            line.classList.add("guide--v");
            line.style.left = (m.ox + g.pos * m.scale) + "px";
            line.style.top = m.oy + "px";
            line.style.height = (m.slideH * m.scale) + "px";
        }
        // Inherited guides are read-only (edit them on the layout); only own
        // guides take the drag/select gesture.
        if (!readOnly) {
            line.addEventListener("mousedown", function (e) { startGuideDrag(e, g); });
        }
        return line;
    }

    // pointerToSlide — slide-pixel coordinate of a pointer event along an axis.
    function pointerToSlide(e, orient, m) {
        const sr = document.getElementById("viewport-container").getBoundingClientRect();
        if (orient === "h") {
            return Math.round((e.clientY - sr.top - m.oy) / m.scale);
        }
        return Math.round((e.clientX - sr.left - m.ox) / m.scale);
    }

    function clampGuidePos(orient, pos, m) {
        const max = (orient === "h") ? m.slideH : m.slideW;
        return Math.max(0, Math.min(max, pos));
    }

    // overRuler — is the pointer over the ruler the given orientation drags from
    // (top ruler for h-guides, left ruler for v-guides)? Used to delete-on-drop.
    function overRuler(e, orient) {
        const sr = document.getElementById("viewport-container").getBoundingClientRect();
        if (orient === "h") {
            return (e.clientY - sr.top) < RULER;
        }
        return (e.clientX - sr.left) < RULER;
    }

    // startGuideCreate — drag a new guide out of a ruler. It lives only once the
    // pointer leaves the ruler band; releasing back on the ruler discards it.
    function startGuideCreate(e, orient) {
        if (!rulersOn || e.button !== 0) {
            return;
        }
        const m = canvasMetrics();
        if (!m) {
            return;
        }
        e.preventDefault();
        e.stopPropagation();
        // A local, server-less temp guide rendered live during the drag; on
        // drop GuideAdded is dispatched and the GuidesUpdate echo replaces it
        // with the authoritative guide. Released back on the ruler: discarded.
        const g = { id: "gtmp", index: -1, orient: orient, pos: clampGuidePos(orient, pointerToSlide(e, orient, m), m) };
        guideOwn.push(g);
        selectedGuideId = g.id;
        renderRulerGuides();
        showGuideInspector();
        beginGuideSession(g, orient, true);
    }

    // startGuideDrag — move (or delete) an existing guide.
    function startGuideDrag(e, g) {
        if (e.button !== 0) {
            return;
        }
        e.preventDefault();
        e.stopPropagation();
        selectGuide(g.id);
        beginGuideSession(g, g.orient, false);
    }

    // beginGuideSession — shared move loop for create + drag, with its own
    // listeners so it never tangles with element dragging. Positions update
    // locally for a smooth drag; the authoritative change is dispatched to Rust
    // once on drop (GuideAdded / GuideMoved), or the guide is removed
    // (GuideRemoved / discarded) when dropped back over its ruler.
    function beginGuideSession(g, orient, isCreate) {
        guideDragSession = { g: g, orient: orient, isCreate: isCreate };
        const move = function (ev) {
            const m = canvasMetrics();
            if (!m) {
                return;
            }
            g.pos = clampGuidePos(orient, pointerToSlide(ev, orient, m), m);
            renderRulerGuides();
            showGuideInspector();
        };
        const up = function (ev) {
            window.removeEventListener("mousemove", move);
            window.removeEventListener("mouseup", up);
            guideDragSession = null;
            const onRuler = overRuler(ev, orient);
            if (isCreate) {
                // Temp guide: commit it as a new guide, or drop it silently.
                guideOwn = guideOwn.filter(function (x) { return x !== g; });
                if (selectedGuideId === g.id) {
                    selectedGuideId = null;
                }
                if (!onRuler) {
                    sendGuideEvent({ kind: "GuideAdded", axis: orient, pos: g.pos });
                } else {
                    renderRulerGuides();
                    hideGuideInspector();
                }
            } else if (onRuler) {
                sendGuideEvent({ kind: "GuideRemoved", index: g.index });
            } else {
                sendGuideEvent({ kind: "GuideMoved", index: g.index, pos: g.pos });
            }
        };
        window.addEventListener("mousemove", move);
        window.addEventListener("mouseup", up);
    }

    function selectGuide(id) {
        selectedGuideId = id;
        // A guide selection is not an element selection — clear any element
        // selection (and slide focus) so only the guide reads as selected.
        slideSelected = false;
        if (currentSelectionIds.length > 0) {
            window.__deck.send("Interaction", { kind: "SetSelectionFromPanel", element_ids: [] });
        }
        renderRulerGuides();
        refreshInspector();
    }

    function deselectGuide() {
        if (selectedGuideId === null) {
            return;
        }
        selectedGuideId = null;
        renderRulerGuides();
        hideGuideInspector();
    }

    // deleteGuide — dispatch a removal for the own guide with this id. Rust
    // applies it and echoes GuidesUpdate, which re-hydrates the overlay (so the
    // local splice and re-render happen there, not here).
    function deleteGuide(id) {
        const g = guideOwn.find(function (x) { return x.id === id; });
        if (!g || g.index < 0) {
            return;
        }
        if (selectedGuideId === id) {
            selectedGuideId = null;
            hideGuideInspector();
        }
        sendGuideEvent({ kind: "GuideRemoved", index: g.index });
    }

    // showGuideInspector / hideGuideInspector — the selected guide's only
    // editable property is its position (px along its axis).
    function showGuideInspector() {
        const box = document.getElementById("guide-box");
        if (!box) {
            return;
        }
        const g = currentGuides().find(function (x) { return x.id === selectedGuideId; });
        if (!g) {
            hideGuideInspector();
            return;
        }
        setSlideBoxVisible(false);
        setElementInspectorVisible(false, null);
        box.style.display = "block";
        const sub = document.getElementById("inspector-target");
        if (sub) {
            sub.textContent = (g.orient === "h" ? "Horizontal" : "Vertical") + " guide";
        }
        const lbl = document.getElementById("guide-pos-label");
        if (lbl) {
            lbl.textContent = (g.orient === "h") ? "Y" : "X";
        }
        const input = document.getElementById("guide-pos");
        if (input && document.activeElement !== input) {
            input.value = String(g.pos);
        }
    }

    function hideGuideInspector() {
        const box = document.getElementById("guide-box");
        if (box) {
            box.style.display = "none";
        }
    }

    // wireGuideInspector — commit the position field to the selected guide.
    function wireGuideInspector() {
        const input = document.getElementById("guide-pos");
        if (!input) {
            return;
        }
        input.addEventListener("change", function () {
            const g = currentGuides().find(function (x) { return x.id === selectedGuideId; });
            if (!g || g.index < 0) {
                return;
            }
            const m = canvasMetrics();
            let v = parseInt(input.value, 10);
            if (!isFinite(v)) {
                v = g.pos;
            }
            const pos = m ? clampGuidePos(g.orient, v, m) : Math.max(0, v);
            input.value = String(pos);
            // Commit via Rust; the GuidesUpdate echo re-renders the overlay.
            sendGuideEvent({ kind: "GuideMoved", index: g.index, pos: pos });
        });
    }

    // ---------- resizable panes ----------
    // captureCanvasMin — record the canvas content size at launch (the default
    // spawn window). This is the floor pane growth may not push the canvas
    // below. Captured once.
    function captureCanvasMin() {
        if (canvasMinW > 0) {
            return;
        }
        const canvas = document.querySelector(".panel--canvas");
        if (!canvas) {
            return;
        }
        const r = canvas.getBoundingClientRect();
        if (r.width > 0 && r.height > 0) {
            canvasMinW = r.width;
            canvasMinH = r.height;
        }
    }

    // positionDividers — lay the three hit strips over the inter-pane gutters,
    // tracking the live pane rects. Coordinates are body-relative (body is the
    // fixed positioning context).
    function positionDividers() {
        const objects = document.getElementById("object-panel");
        const inspector = document.getElementById("inspector-panel");
        const thumbs = document.querySelector(".panel--thumbs");
        const dObj = document.getElementById("divider-objects");
        const dIns = document.getElementById("divider-inspector");
        const dThu = document.getElementById("divider-thumbs");
        if (!objects || !inspector || !thumbs || !dObj || !dIns || !dThu) {
            return;
        }
        const gut = 11;
        const place = function (el, left, top, w, h) {
            el.style.display = "block";
            el.style.left = left + "px";
            el.style.top = top + "px";
            el.style.width = w + "px";
            el.style.height = h + "px";
        };
        const o = objects.getBoundingClientRect();
        const ins = inspector.getBoundingClientRect();
        const th = thumbs.getBoundingClientRect();
        place(dObj, o.right - gut / 2, o.top, gut, o.height);
        place(dIns, ins.left - gut / 2, ins.top, gut, ins.height);
        place(dThu, th.left, th.top - gut / 2, th.width, gut);
    }

    // refitThumbnails — size every thumbnail preview to fill the thumbs pane
    // height (margins unchanged), then refit each slide mount to its new box.
    function refitThumbnails() {
        const strip = document.getElementById("thumbnail-row");
        if (!strip) {
            return;
        }
        const cs = getComputedStyle(strip);
        const padV = parseFloat(cs.paddingTop) + parseFloat(cs.paddingBottom);
        const cap = strip.querySelector(".thumb__caption");
        const capH = cap ? cap.offsetHeight : 16;
        const gap = 6; // .thumb column gap (preview ↔ caption)
        let ph = strip.clientHeight - padV - capH - gap;
        if (!(ph > 0)) {
            return;
        }
        const aspect = (thumbnailDims.width || 1920) / (thumbnailDims.height || 1080);
        const pw = Math.round(ph * aspect);
        ph = Math.round(ph);
        const boxes = strip.querySelectorAll(".thumb__preview, .thumb__add-glyph");
        for (let i = 0; i < boxes.length; i++) {
            boxes[i].style.width = pw + "px";
            boxes[i].style.height = ph + "px";
        }
        const previews = strip.querySelectorAll(".thumb__preview");
        for (let i = 0; i < previews.length; i++) {
            const mount = previews[i].querySelector(".thumb__mount");
            if (mount) {
                applyThumbnailScale(previews[i], mount);
            }
        }
    }

    // wirePaneResizers — bind the three dividers.
    function wirePaneResizers() {
        const map = {
            "divider-objects": "objects",
            "divider-inspector": "inspector",
            "divider-thumbs": "thumbs",
        };
        Object.keys(map).forEach(function (id) {
            const el = document.getElementById(id);
            if (el) {
                el.addEventListener("mousedown", function (e) { beginPaneDrag(e, map[id], el); });
            }
        });
        positionDividers();
    }

    // beginPaneDrag — drag one divider. Recomputes the clamp from live rects on
    // every move so a pane grows only into the canvas's spare room (zero at the
    // spawn window), capped at its fixed max, and never below its default min.
    function beginPaneDrag(e, kind, el) {
        if (e.button !== 0) {
            return;
        }
        e.preventDefault();
        e.stopPropagation();
        captureCanvasMin();
        el.classList.add("is-dragging");
        const move = function (ev) {
            applyPaneSize(kind, ev);
            positionDividers();
        };
        const up = function () {
            window.removeEventListener("mousemove", move);
            window.removeEventListener("mouseup", up);
            el.classList.remove("is-dragging");
        };
        window.addEventListener("mousemove", move);
        window.addEventListener("mouseup", up);
    }

    function applyPaneSize(kind, ev) {
        const canvas = document.querySelector(".panel--canvas");
        if (!canvas) {
            return;
        }
        const cr = canvas.getBoundingClientRect();
        if (kind === "thumbs") {
            const thumbs = document.querySelector(".panel--thumbs");
            const cur = thumbs.getBoundingClientRect();
            const desired = cur.bottom - ev.clientY;
            const max = Math.min(PANE_MAX.thumbs, cur.height + (cr.height - canvasMinH));
            const h = Math.max(PANE_MIN.thumbs, Math.min(desired, max));
            thumbs.style.height = h + "px";
            refitThumbnails();
        } else {
            const isObj = kind === "objects";
            const pane = document.getElementById(isObj ? "object-panel" : "inspector-panel");
            const cur = pane.getBoundingClientRect();
            const desired = isObj ? (ev.clientX - cur.left) : (cur.right - ev.clientX);
            const max = Math.min(PANE_MAX[kind], cur.width + (cr.width - canvasMinW));
            const w = Math.max(PANE_MIN[kind], Math.min(desired, max));
            pane.style.width = w + "px";
        }
        // Keep overlays aligned with the reflowed canvas.
        if (zoomMode === "fit") {
            applyZoom();
        } else {
            refreshRulers();
            renderRulerGuides();
            renderCanvasScrim();
            updateSelectionOverlay();
        }
    }

    // drawAlignLine
    // Inputs: the layer, an align/center guide { axis, pos }, the mapping.
    // Output: side-effect; one full-length 1px line over the slide surface.
    function drawAlignLine(layer, g, m) {
        const line = document.createElement("div");
        line.className = "snap-guide snap-guide--line";
        if (g.axis === "x") {
            line.style.left = (m.ox + g.pos * m.scale) + "px";
            line.style.top = m.oy + "px";
            line.style.width = "1px";
            line.style.height = (1080 * m.scale) + "px";
        } else {
            line.style.top = (m.oy + g.pos * m.scale) + "px";
            line.style.left = m.ox + "px";
            line.style.height = "1px";
            line.style.width = (1920 * m.scale) + "px";
        }
        layer.appendChild(line);
    }

    // drawSpacing
    // Inputs: the layer, a spacing guide { axis, gaps }, the mapping. Output:
    // side-effect; a thin bar with end ticks for each equal gap.
    function drawSpacing(layer, g, m) {
        let i = 0;
        for (i = 0; i < g.gaps.length; i = i + 1) {
            const gap = g.gaps[i];
            const bar = document.createElement("div");
            bar.className = "snap-guide snap-guide--space snap-guide--space-"
                + (g.axis === "x" ? "h" : "v");
            if (g.axis === "x") {
                bar.style.left = (m.ox + gap.start * m.scale) + "px";
                bar.style.width = ((gap.end - gap.start) * m.scale) + "px";
                bar.style.top = (m.oy + gap.perp * m.scale) + "px";
            } else {
                bar.style.top = (m.oy + gap.start * m.scale) + "px";
                bar.style.height = ((gap.end - gap.start) * m.scale) + "px";
                bar.style.left = (m.ox + gap.perp * m.scale) + "px";
            }
            layer.appendChild(bar);
        }
    }

    // renderGuides
    // Inputs: guide descriptors from the snap engine. Output: side-effect;
    // draws magenta lines/ticks into #snap-guides. Clears first each frame.
    function renderGuides(guides) {
        const layer = ensureGuideLayer();
        if (!layer) {
            return;
        }
        layer.replaceChildren();
        const m = slideToScreen(layer);
        if (!m || !guides) {
            return;
        }
        let i = 0;
        for (i = 0; i < guides.length; i = i + 1) {
            if (guides[i].kind === "spacing") {
                drawSpacing(layer, guides[i], m);
            } else {
                drawAlignLine(layer, guides[i], m);
            }
        }
    }

    // clearGuides
    // Inputs: none. Output: empties the #snap-guides layer.
    function clearGuides() {
        const layer = document.getElementById("snap-guides");
        if (layer) {
            layer.replaceChildren();
        }
    }

    // buildSnapTargets
    // Inputs: the element id to EXCLUDE (the one being manipulated). Output:
    // { xLines, yLines, rects } from __snap.__build_targets, built from every
    // other element's inline rect plus the slide pseudo-rect. Read once per
    // gesture (siblings do not move mid-gesture).
    function buildSnapTargets(excludeId) {
        const rects = [{ x: 0, y: 0, w: 1920, h: 1080 }];
        if (currentShadow) {
            const nodes = currentShadow.querySelectorAll("[data-element-id]");
            let i = 0;
            for (i = 0; i < nodes.length && rects.length < 256; i = i + 1) {
                if (nodes[i].dataset.elementId === excludeId) {
                    continue;
                }
                const r = movingRectFromStyle(nodes[i]);
                if (r.w > 0 && r.h > 0) {
                    rects.push(r);
                }
            }
        }
        const targets = window.__snap.__build_targets(rects);
        // Ruler guides are snap targets too — both the canvas's own and the
        // inherited layout guides: vertical guides add an x line, horizontal a
        // y line.
        const guides = guideOwn.concat(guideInherited);
        for (let g = 0; g < guides.length; g++) {
            if (guides[g].orient === "v") {
                targets.xLines.push({ pos: guides[g].pos, source: "guide" });
            } else {
                targets.yLines.push({ pos: guides[g].pos, source: "guide" });
            }
        }
        return targets;
    }

    // movingRectFromStyle
    // Inputs: a target element. Output: its current inline rect in slide px.
    function movingRectFromStyle(el) {
        const d = parseStyleAttr(el.getAttribute("style") || "");
        return {
            x: parseFloat(stripPx(d.left)) || 0,
            y: parseFloat(stripPx(d.top)) || 0,
            w: parseFloat(stripPx(d.width)) || 0,
            h: parseFloat(stripPx(d.height)) || 0,
        };
    }

    // ---------- crop mode ----------
    // ensureCropLayer
    // Inputs: none. Output: the #crop-overlay element, created once as a
    // SIBLING of #selection-overlay inside #viewport-container (NOT inside it,
    // which updateSelectionOverlay wipes via replaceChildren).
    function ensureCropLayer() {
        const container = document.getElementById("viewport-container");
        if (!container) {
            return null;
        }
        let layer = document.getElementById("crop-overlay");
        if (!layer) {
            layer = document.createElement("div");
            layer.id = "crop-overlay";
            container.appendChild(layer);
        }
        return layer;
    }

    // cropImageUrl
    // Inputs: an asset id. Output: the cached blob URL, or "".
    function cropImageUrl(assetId) {
        const entry = assetBlobCache[assetId];
        return (entry && entry.url) ? entry.url : "";
    }

    // clearCropOverlay
    // Inputs: none. Output: empties #crop-overlay.
    function clearCropOverlay() {
        const layer = document.getElementById("crop-overlay");
        if (layer) {
            layer.replaceChildren();
        }
    }

    // cropPlaceImg
    // Inputs: a div, blob url, and a screen rect. Output: side-effect; styles
    // it as a background image filling that rect.
    function cropPlaceImg(el, url, x, y, w, h) {
        el.style.position = "absolute";
        el.style.left = x + "px";
        el.style.top = y + "px";
        el.style.width = w + "px";
        el.style.height = h + "px";
        el.style.backgroundImage = "url(" + url + ")";
        el.style.backgroundSize = "100% 100%";
        el.style.backgroundRepeat = "no-repeat";
    }

    // cropDrawMaskFrame
    // Inputs: layer + mask screen rect. Output: side-effect; outline div + the
    // 8 resize handles (reusing the SELECTION_HANDLES fraction table).
    function cropDrawMaskFrame(layer, x, y, w, h) {
        const box = document.createElement("div");
        box.className = "crop-mask-box";
        box.style.left = x + "px";
        box.style.top = y + "px";
        box.style.width = w + "px";
        box.style.height = h + "px";
        layer.appendChild(box);
        let i = 0;
        for (i = 0; i < SELECTION_HANDLES.length; i = i + 1) {
            const s = SELECTION_HANDLES[i];
            const handle = document.createElement("div");
            handle.className = "crop-handle";
            handle.dataset.handle = s.name;
            handle.style.left = (x + s.fx * w) + "px";
            handle.style.top = (y + s.fy * h) + "px";
            handle.addEventListener("mousedown", onCropHandleMouseDown);
            layer.appendChild(handle);
        }
    }

    // cropDrawToolbar
    // Inputs: layer + the mask's top-right screen point. Output: side-effect;
    // the floating toolbar (zoom slider, %, Reset, ✕ cancel, ✓ confirm).
    function cropDrawToolbar(layer, rightX, topY) {
        const bar = document.createElement("div");
        bar.className = "crop-toolbar";
        bar.style.left = rightX + "px";
        bar.style.top = topY + "px";
        const pct = Math.round(window.__crop.zoomPercent(
            cropState.state, cropState.mask, cropState.natural));
        bar.innerHTML =
            '<input type="range" class="crop-zoom" min="100" max="400" value="' + pct + '">'
            + '<span class="crop-zoom-pct">' + pct + '%</span>'
            + '<button type="button" class="crop-btn crop-reset" title="Reset crop">Reset</button>'
            + '<button type="button" class="crop-btn crop-cancel" title="Cancel (Esc)">✕</button>'
            + '<button type="button" class="crop-btn crop-confirm" title="Done (Enter)">✓</button>';
        bar.querySelector(".crop-zoom").addEventListener("input", onCropZoomInput);
        bar.querySelector(".crop-reset").addEventListener("click", resetCrop);
        bar.querySelector(".crop-cancel").addEventListener("click", cancelCrop);
        bar.querySelector(".crop-confirm").addEventListener("click", commitCrop);
        layer.appendChild(bar);
    }

    // renderCropOverlay
    // Inputs: none (reads cropState). Output: side-effect; draws the dimmed
    // full image, the bright in-mask region, a pan catcher, the mask frame +
    // handles, and the toolbar. Cleared and redrawn each interaction.
    function renderCropOverlay() {
        const layer = ensureCropLayer();
        if (!layer || !cropState) {
            return;
        }
        layer.replaceChildren();
        const m = slideToScreen(layer);
        if (!m) {
            return;
        }
        const mask = cropState.mask;
        const st = cropState.state;
        const url = cropImageUrl(cropState.assetId);
        const imgX = m.ox + (mask.x + st.dx) * m.scale;
        const imgY = m.oy + (mask.y + st.dy) * m.scale;
        const imgW = st.iw * m.scale;
        const imgH = st.ih * m.scale;
        const mX = m.ox + mask.x * m.scale;
        const mY = m.oy + mask.y * m.scale;
        const mW = mask.w * m.scale;
        const mH = mask.h * m.scale;
        // (1) dimmed full image
        const dim = document.createElement("div");
        dim.className = "crop-img crop-img--dim";
        cropPlaceImg(dim, url, imgX, imgY, imgW, imgH);
        layer.appendChild(dim);
        // (2) bright in-mask region: same image, clipped to the mask box
        const bright = document.createElement("div");
        bright.className = "crop-img crop-img--bright";
        cropPlaceImg(bright, url, imgX, imgY, imgW, imgH);
        bright.style.clipPath = "inset(" + (mY - imgY) + "px "
            + (imgX + imgW - (mX + mW)) + "px "
            + (imgY + imgH - (mY + mH)) + "px " + (mX - imgX) + "px)";
        layer.appendChild(bright);
        // (3) transparent catcher for pan + scroll-zoom over the mask
        const catcher = document.createElement("div");
        catcher.className = "crop-catcher";
        catcher.style.position = "absolute";
        catcher.style.left = mX + "px";
        catcher.style.top = mY + "px";
        catcher.style.width = mW + "px";
        catcher.style.height = mH + "px";
        catcher.style.pointerEvents = "auto";
        catcher.style.cursor = "move";
        catcher.addEventListener("mousedown", onCropPanMouseDown);
        catcher.addEventListener("wheel", onCropWheel, { passive: false });
        layer.appendChild(catcher);
        // (4) mask outline + handles, then (5) toolbar pinned top-right
        cropDrawMaskFrame(layer, mX, mY, mW, mH);
        cropDrawToolbar(layer, mX + mW, mY);
    }

    // enterCropMode
    // Inputs: an image element id. Output: side-effect; loads natural dims,
    // seeds cropState from existing crop styles or the cover baseline, and
    // renders the overlay. No IPC (fully optimistic until commit).
    function enterCropMode(elementId) {
        const el = findElement(elementId);
        if (!el || el.dataset.elementType !== "image") {
            return;
        }
        const assetId = el.dataset.assetId || "";
        const url = cropImageUrl(assetId);
        if (!url) {
            return;
        }
        const rect = movingRectFromStyle(el);
        const mask = { x: rect.x, y: rect.y, w: rect.w, h: rect.h };
        const decls = parseStyleAttr(el.getAttribute("style") || "");
        const img = new Image();
        img.onload = function () {
            const natural = { w: img.naturalWidth, h: img.naturalHeight };
            if (!(natural.w > 0 && natural.h > 0)) {
                return;
            }
            let state = window.__crop.fromStyles(
                decls["background-size"], decls["background-position"]);
            if (!state) {
                state = window.__crop.fromCover(mask, natural);
            }
            cropState = {
                elementId: elementId,
                assetId: assetId,
                el: el,
                mask: mask,
                natural: natural,
                state: state,
                preStyle: el.getAttribute("style") || "",
            };
            // Hide the real element so the overlay is the sole image source —
            // otherwise its full-opacity render defeats the dim preview.
            el.style.visibility = "hidden";
            document.body.dataset.crop = "1";
            updateSelectionOverlay();
            renderCropOverlay();
        };
        img.src = url;
    }

    // onCropPanMouseDown / Move / Up — drag inside the mask pans the image.
    function onCropPanMouseDown(e) {
        if (!cropState || e.button !== 0) {
            return;
        }
        e.preventDefault();
        e.stopPropagation();
        cropPan = { x: e.clientX, y: e.clientY };
        window.addEventListener("mousemove", onCropPanMouseMove);
        window.addEventListener("mouseup", onCropPanMouseUp);
    }
    function onCropPanMouseMove(e) {
        if (!cropPan || !cropState) {
            return;
        }
        const scale = getViewportScale();
        const ddx = (e.clientX - cropPan.x) / scale;
        const ddy = (e.clientY - cropPan.y) / scale;
        cropPan = { x: e.clientX, y: e.clientY };
        cropState.state = window.__crop.pan(cropState.state, cropState.mask, ddx, ddy);
        renderCropOverlay();
    }
    function onCropPanMouseUp() {
        cropPan = null;
        window.removeEventListener("mousemove", onCropPanMouseMove);
        window.removeEventListener("mouseup", onCropPanMouseUp);
    }

    // onCropWheel — scroll zooms about the mask center.
    function onCropWheel(e) {
        if (!cropState) {
            return;
        }
        e.preventDefault();
        const factor = e.deltaY < 0 ? 1.05 : (1 / 1.05);
        cropState.state = window.__crop.zoom(
            cropState.state, cropState.mask, cropState.natural, factor);
        renderCropOverlay();
    }

    // onCropZoomInput — slider sets an absolute zoom percent.
    function onCropZoomInput(e) {
        if (!cropState) {
            return;
        }
        const pct = parseFloat(e.currentTarget.value) || 100;
        cropState.state = window.__crop.setZoomPercent(
            pct, cropState.state, cropState.mask, cropState.natural);
        renderCropOverlay();
    }

    // onCropHandleMouseDown / Move / Up — resize the mask window (reveal/clip),
    // reusing the snap engine for the box and re-clamping the image to cover.
    function onCropHandleMouseDown(e) {
        if (!cropState || e.button !== 0) {
            return;
        }
        e.preventDefault();
        e.stopPropagation();
        cropResize = {
            handle: e.currentTarget.dataset.handle,
            startMouse: { x: e.clientX, y: e.clientY },
            startMask: {
                x: cropState.mask.x, y: cropState.mask.y,
                w: cropState.mask.w, h: cropState.mask.h,
            },
            // The image's top-left in canvas/slide coords, captured so the
            // image stays put while the mask window is resized around it.
            imgOrigin: {
                x: cropState.mask.x + cropState.state.dx,
                y: cropState.mask.y + cropState.state.dy,
            },
            snapTargets: buildSnapTargets(cropState.elementId),
        };
        window.addEventListener("mousemove", onCropHandleMouseMove);
        window.addEventListener("mouseup", onCropHandleMouseUp);
    }
    function onCropHandleMouseMove(e) {
        if (!cropResize || !cropState) {
            return;
        }
        const scale = getViewportScale();
        const dx = (e.clientX - cropResize.startMouse.x) / scale;
        const dy = (e.clientY - cropResize.startMouse.y) / scale;
        // Reuse the element resize math so the mask box honors Shift
        // (proportional) and Alt (from-center) exactly like a normal resize.
        const raw = computeResizeRect(
            { handle: cropResize.handle, startRect: cropResize.startMask },
            dx, dy, !!e.shiftKey, !!e.altKey);
        const snapped = window.__snap.forResize(
            raw, handleEdges(cropResize.handle), cropResize.snapTargets,
            {
                threshold: 3 / scale, gridEnabled: gridEnabled, suppress: !!e.metaKey,
                shift: !!e.shiftKey, alt: !!e.altKey,
                aspect: cropResize.startMask.w / cropResize.startMask.h,
            });
        cropState.mask = {
            x: snapped.rect.x, y: snapped.rect.y, w: snapped.rect.w, h: snapped.rect.h,
        };
        // Hold the image fixed in canvas space — only the mask window moves.
        cropState.state = window.__crop.placeImage(
            cropState.state, cropState.mask,
            cropResize.imgOrigin.x, cropResize.imgOrigin.y, cropState.natural);
        renderCropOverlay();
        renderGuides(snapped.guides);
    }
    function onCropHandleMouseUp() {
        cropResize = null;
        clearGuides();
        window.removeEventListener("mousemove", onCropHandleMouseMove);
        window.removeEventListener("mouseup", onCropHandleMouseUp);
    }

    // resetCrop — back to the seamless cover baseline (live, in crop mode).
    function resetCrop() {
        if (!cropState) {
            return;
        }
        cropState.state = window.__crop.fromCover(cropState.mask, cropState.natural);
        renderCropOverlay();
    }

    // commitCrop — send ElementCropCommitted and tear down the overlay.
    function commitCrop() {
        if (!cropState) {
            return;
        }
        const css = window.__crop.toStyles(cropState.state);
        window.__deck.send("Interaction", {
            kind: "ElementCropCommitted",
            element_id: cropState.elementId,
            new_position: { x: cropState.mask.x, y: cropState.mask.y },
            new_size: { width: cropState.mask.w, height: cropState.mask.h },
            background_size: css.backgroundSize,
            background_position: css.backgroundPosition,
        });
        exitCropMode();
    }

    // cancelCrop — discard the session; no IPC (element was never mutated).
    function cancelCrop() {
        exitCropMode();
    }

    // exitCropMode — restore the hidden element, clear crop state, guides,
    // and the overlay.
    function exitCropMode() {
        if (cropState && cropState.el) {
            cropState.el.style.removeProperty("visibility");
        }
        cropState = null;
        cropPan = null;
        cropResize = null;
        delete document.body.dataset.crop;
        clearGuides();
        clearCropOverlay();
        updateSelectionOverlay();
    }

    // refreshCropBox
    // Inputs: none (reads currentSelectionIds + shadow). Output: side-effect;
    // shows the Inspector crop section and syncs Offset X/Y for a single
    // selected image, else hides it. Zoom % is left for the user to type (it
    // needs natural dims, loaded on edit).
    function refreshCropBox() {
        const box = document.getElementById("crop-box");
        if (!box) {
            return;
        }
        const el = (currentSelectionIds.length === 1)
            ? findElement(currentSelectionIds[0]) : null;
        if (!el || el.dataset.elementType !== "image") {
            box.hidden = true;
            return;
        }
        box.hidden = false;
        const decls = parseStyleAttr(el.getAttribute("style") || "");
        const state = window.__crop.fromStyles(
            decls["background-size"], decls["background-position"]);
        const x = document.getElementById("crop-offset-x");
        const y = document.getElementById("crop-offset-y");
        if (state) {
            x.value = Math.round(state.dx);
            y.value = Math.round(state.dy);
        } else {
            x.value = "";
            y.value = "";
        }
        document.getElementById("crop-zoom-pct").value = "";
    }

    // withImageNatural
    // Inputs: an image element id and a callback (el, mask, natural, decls).
    // Output: loads the asset's natural dims via an Image, then invokes the
    // callback. No-op when the element is not a loadable image.
    function withImageNatural(id, cb) {
        const el = findElement(id);
        if (!el || el.dataset.elementType !== "image") {
            return;
        }
        const url = cropImageUrl(el.dataset.assetId || "");
        if (!url) {
            return;
        }
        const rect = movingRectFromStyle(el);
        const mask = { x: rect.x, y: rect.y, w: rect.w, h: rect.h };
        const decls = parseStyleAttr(el.getAttribute("style") || "");
        const img = new Image();
        img.onload = function () {
            if (img.naturalWidth > 0 && img.naturalHeight > 0) {
                cb(el, mask, { w: img.naturalWidth, h: img.naturalHeight }, decls);
            }
        };
        img.src = url;
    }

    // onCropInspectorEdit — recompute crop from the edited fields and commit
    // background-size + background-position via PropertyChanged.
    function onCropInspectorEdit() {
        if (currentSelectionIds.length !== 1) {
            return;
        }
        const id = currentSelectionIds[0];
        withImageNatural(id, function (el, mask, natural, decls) {
            let state = window.__crop.fromStyles(
                decls["background-size"], decls["background-position"])
                || window.__crop.fromCover(mask, natural);
            const pct = parseFloat(document.getElementById("crop-zoom-pct").value);
            if (isFinite(pct) && pct >= 100) {
                state = window.__crop.setZoomPercent(pct, state, mask, natural);
            }
            const ox = parseFloat(document.getElementById("crop-offset-x").value);
            const oy = parseFloat(document.getElementById("crop-offset-y").value);
            const tx = isFinite(ox) ? ox - state.dx : 0;
            const ty = isFinite(oy) ? oy - state.dy : 0;
            if (tx !== 0 || ty !== 0) {
                state = window.__crop.pan(state, mask, tx, ty);
            }
            sendCropStyleEdits(id, window.__crop.toStyles(state));
        });
    }

    // inspectorResetCrop — reset to the cover baseline and commit.
    function inspectorResetCrop(id) {
        withImageNatural(id, function (el, mask, natural) {
            sendCropStyleEdits(id, window.__crop.toStyles(
                window.__crop.fromCover(mask, natural)));
        });
    }

    // sendCropStyleEdits — commit background-size + background-position via the
    // existing PropertyChanged → SetInlineStyle path.
    function sendCropStyleEdits(id, css) {
        window.__deck.send("Interaction", {
            kind: "PropertyChanged", element_id: id,
            property: "background-size", value: css.backgroundSize,
        });
        window.__deck.send("Interaction", {
            kind: "PropertyChanged", element_id: id,
            property: "background-position", value: css.backgroundPosition,
        });
    }

    // bindCropInspectorControls — wire the crop section's buttons + fields.
    function bindCropInspectorControls() {
        const enterBtn = document.getElementById("crop-enter");
        if (enterBtn) {
            enterBtn.addEventListener("click", function () {
                if (currentSelectionIds.length === 1) {
                    enterCropMode(currentSelectionIds[0]);
                }
            });
        }
        const resetBtn = document.getElementById("crop-reset");
        if (resetBtn) {
            resetBtn.addEventListener("click", function () {
                if (currentSelectionIds.length === 1) {
                    inspectorResetCrop(currentSelectionIds[0]);
                }
            });
        }
        const ids = ["crop-offset-x", "crop-offset-y", "crop-zoom-pct"];
        let i = 0;
        for (i = 0; i < ids.length; i = i + 1) {
            const input = document.getElementById(ids[i]);
            if (input) {
                input.addEventListener("change", onCropInspectorEdit);
            }
        }
    }

    // ---------- interaction capture ----------
    // findInteractionTarget
    // Inputs: a DOM Event.
    // Output: the first ancestor along composedPath carrying
    // data-element-id, or null. Skips elements without the attribute and
    // stops at the slide host (so background clicks return null).
    function findInteractionTarget(e) {
        const path = (typeof e.composedPath === "function") ? e.composedPath() : [];
        let hit = null;
        for (let i = 0; i < path.length; i++) {
            const node = path[i];
            if (!node || !node.dataset) {
                continue;
            }
            if (node.classList && node.classList.contains("slide-host")) {
                break;
            }
            if (node.dataset.elementId) {
                hit = node;
                break;
            }
        }
        if (!hit) { return null; }
        const chain = elementChain(hit); // innermost..outermost
        if (focusChain.length === 0) {
            return chain[chain.length - 1]; // outermost element under the slide
        }
        // Focused: return the child of the deepest focused group in the chain.
        const deep = focusChain[focusChain.length - 1];
        for (let i = 0; i < chain.length; i++) {
            const parent = chain[i].parentElement;
            if (parent && parent.dataset && parent.dataset.elementId === deep) {
                return chain[i];
            }
        }
        return chain[chain.length - 1];
    }

    // readModifiers
    // Inputs: an Event with modifier-key flags.
    // Output: a Modifiers object matching the Rust struct shape.
    function readModifiers(e) {
        return {
            shift: !!e.shiftKey,
            ctrl: !!e.ctrlKey,
            alt: !!e.altKey,
            meta: !!e.metaKey,
        };
    }

    // ---------- inline text editing ----------
    // onViewportDblClick
    // Inputs: a dblclick MouseEvent on the viewport container.
    // Output: side-effect; if the double-clicked element is a Text
    // element, enters inline editing on it. Other element types are
    // ignored (double-click has no meaning for them yet).
    function onViewportDblClick(e) {
        const target = findInteractionTarget(e);
        if (!target) {
            return;
        }
        if (target.dataset.elementType === "group") {
            e.preventDefault();
            focusChain.push(target.dataset.elementId);
            // Select the child under the cursor at the new level.
            const inner = findInteractionTarget(e);
            if (inner && inner.dataset.elementId) {
                window.__deck.send("Interaction", {
                    kind: "ElementClicked", element_id: inner.dataset.elementId,
                    modifiers: readModifiers(e), position: { x: e.clientX, y: e.clientY },
                });
            }
            return;
        }
        if (target.dataset.elementType === "image") {
            e.preventDefault();
            enterCropMode(target.dataset.elementId);
            return;
        }
        if (target.dataset.elementType === "embed") {
            e.preventDefault();
            openEmbedEditor(target.dataset.elementId, target.innerHTML);
            return;
        }
        if (target.dataset.elementType === "table") {
            e.preventDefault();
            const tid = target.dataset.elementId;
            const already = focusedTableId() === tid;
            if (!already) {
                focusChain = [tid];
            }
            const rc = cellAtPoint(tid, e.clientX, e.clientY);
            if (!rc) {
                return;
            }
            // First double-click enters cell-focus and selects the cell; a
            // second double-click (already focused) edits the cell text.
            if (already) {
                beginCellEdit(tid, rc);
            } else {
                selectCell(tid, rc, null);
            }
            return;
        }
        if (target.dataset.elementType !== "text") {
            return;
        }
        e.preventDefault();
        beginTextEdit(target);
    }

    // openEmbedEditor
    // Inputs: an embed element id and its current raw inner HTML.
    // Output: side-effect; pops a modal textarea to edit the block's HTML.
    // Save commits via EmbedHtmlEditRequested (Rust dispatches SetEmbedHtml);
    // Cancel / Esc / backdrop click dismisses without changes.
    function openEmbedEditor(elementId, currentHtml) {
        const existing = document.getElementById("embed-editor");
        if (existing) {
            existing.remove();
        }
        const overlay = document.createElement("div");
        overlay.id = "embed-editor";
        overlay.className = "embed-editor";
        const panel = document.createElement("div");
        panel.className = "embed-editor__panel";
        const title = document.createElement("h2");
        title.className = "embed-editor__title";
        title.textContent = "Edit code block";
        const area = document.createElement("textarea");
        area.className = "embed-editor__area";
        area.spellcheck = false;
        area.value = currentHtml || "";
        const actions = document.createElement("div");
        actions.className = "embed-editor__actions";
        const cancel = document.createElement("button");
        cancel.type = "button";
        cancel.className = "embed-editor__btn";
        cancel.textContent = "Cancel";
        const save = document.createElement("button");
        save.type = "button";
        save.className = "embed-editor__btn embed-editor__btn--primary";
        save.textContent = "Save";
        function close() {
            overlay.remove();
            document.removeEventListener("keydown", onKey, true);
        }
        function commit() {
            window.__deck.send("Interaction", {
                kind: "EmbedHtmlEditRequested",
                element_id: elementId,
                html: area.value,
            });
            close();
        }
        function onKey(ev) {
            if (ev.key === "Escape") {
                ev.preventDefault();
                ev.stopPropagation();
                close();
            } else if (ev.key === "Enter" && (ev.metaKey || ev.ctrlKey)) {
                ev.preventDefault();
                ev.stopPropagation();
                commit();
            }
        }
        cancel.addEventListener("click", close);
        save.addEventListener("click", commit);
        overlay.addEventListener("mousedown", function (ev) {
            if (ev.target === overlay) {
                close();
            }
        });
        actions.appendChild(cancel);
        actions.appendChild(save);
        panel.appendChild(title);
        panel.appendChild(area);
        panel.appendChild(actions);
        overlay.appendChild(panel);
        document.body.appendChild(overlay);
        document.addEventListener("keydown", onKey, true);
        area.focus();
    }

    // beginTextEdit
    // Inputs: the Text element's DOM node (inside the slide shadow root).
    // Output: side-effect; makes the node contenteditable, focuses it,
    // selects its text, records textEditState, and notifies Rust with
    // TextEditStarted. Enter inserts a newline in the box (default
    // contenteditable behavior); the edit commits on blur / clicking away
    // and cancels on Escape. The keydown listener stopPropagation()s so
    // the global hotkey dispatcher never sees — or crash-guards — edit
    // keystrokes, while leaving that dispatcher (and its Enter handling)
    // intact for use outside edit mode.
    function beginTextEdit(target) {
        const elementId = target.dataset.elementId;
        if (!elementId) {
            return;
        }
        if (textEditState) {
            if (textEditState.elementId === elementId) {
                return;
            }
            commitTextEdit();
        }
        const onKeydown = function (ev) {
            // Keep edit keystrokes out of the global shortcut dispatcher.
            // Enter is deliberately NOT handled here: it falls through to
            // the contenteditable default and types a newline. Escape
            // cancels the edit.
            ev.stopPropagation();
            if (ev.key === "Escape") {
                ev.preventDefault();
                cancelTextEdit();
            }
        };
        const onBlur = function () {
            commitTextEdit();
        };
        textEditState = {
            elementId: elementId,
            target: target,
            original: target.innerText,
            onKeydown: onKeydown,
            onBlur: onBlur,
        };
        // Tokened text renders its resolved value; edit the raw ${…} source.
        if (target.dataset && typeof target.dataset.src === "string") {
            target.textContent = target.dataset.src;
        }
        target.setAttribute("contenteditable", "true");
        target.spellcheck = false;
        target.addEventListener("keydown", onKeydown);
        target.addEventListener("blur", onBlur);
        target.focus();
        selectAllText(target);
        window.__deck.send("Interaction", {
            kind: "TextEditStarted",
            element_id: elementId,
        });
    }

    // finishTextEdit
    // Inputs: commit — true to keep the edited text (send TextEditEnded so
    // Rust dispatches SetTextContent), false to revert to the original.
    // Output: side-effect; tears down the contenteditable session exactly
    // once. textEditState is cleared FIRST so the blur fired by our own
    // .blur() call re-enters as a no-op.
    function finishTextEdit(commit) {
        const state = textEditState;
        if (!state) {
            return;
        }
        textEditState = null;
        const target = state.target;
        target.removeEventListener("keydown", state.onKeydown);
        target.removeEventListener("blur", state.onBlur);
        target.removeAttribute("contenteditable");
        if (commit) {
            // innerText (not textContent) so the line breaks the user
            // typed with Enter survive as "\n" characters in the committed
            // text rather than being flattened away.
            window.__deck.send("Interaction", {
                kind: "TextEditEnded",
                element_id: state.elementId,
                text: target.innerText,
            });
        } else {
            target.textContent = state.original;
        }
        if (typeof target.blur === "function") {
            target.blur();
        }
    }

    function commitTextEdit() {
        finishTextEdit(true);
    }

    function cancelTextEdit() {
        finishTextEdit(false);
    }

    // selectAllText
    // Inputs: a DOM element. Output: side-effect; selects all of its text
    // so the user can type over it immediately. Best-effort: selection
    // across shadow boundaries is inconsistent between engines, so any
    // failure is swallowed (focus alone still allows editing).
    function selectAllText(el) {
        try {
            const sel = window.getSelection();
            if (!sel) {
                return;
            }
            const range = document.createRange();
            range.selectNodeContents(el);
            sel.removeAllRanges();
            sel.addRange(range);
        } catch (err) {
            // No selection available; editing still works via the caret.
        }
    }

    // onMouseDown
    // Inputs: a MouseEvent on #viewport (the host of slide-host).
    // Output: side-effect; sends ElementClicked or BackgroundClicked and
    // arms dragState. The drag only "starts" once mousemove crosses
    // DRAG_THRESHOLD pixels (see onMouseMove).
    function onMouseDown(e) {
        // Only react to primary button.
        if (e.button !== 0) {
            return;
        }
        // While cropping, a press on the overlay (catcher / handles / toolbar)
        // is handled by the overlay's own listeners; a press anywhere else
        // commits the crop. Either way we stop here so no drag/select arms.
        if (cropState) {
            const inOverlay = e.target && e.target.closest
                && e.target.closest("#crop-overlay");
            if (!inOverlay) {
                commitCrop();
            }
            return;
        }
        // While a text element is being edited, let the contenteditable
        // own pointer interactions (caret placement, text selection). A
        // click inside the editor is left alone; a click anywhere else
        // commits the edit and then proceeds with normal handling.
        if (textEditState) {
            const path = (e.composedPath && e.composedPath()) || [];
            if (path.indexOf(textEditState.target) >= 0) {
                return;
            }
            commitTextEdit();
        }
        // Hand tool: a press starts a pan instead of any selection/drag. No
        // effect when the slide already fits (panBounds is then 0,0).
        if (activeTool === "hand") {
            e.preventDefault();
            panSession = { startX: e.clientX, startY: e.clientY, basePanX: panX, basePanY: panY };
            document.body.style.userSelect = "none";
            const stage = document.getElementById("viewport-container");
            if (stage) { stage.style.cursor = "grabbing"; }
            window.addEventListener("mousemove", onPanMouseMove);
            window.addEventListener("mouseup", onPanMouseUp);
            return;
        }
        // Any canvas press is element-level focus, never a slide selection —
        // including a background click, which deselects everything (guides too).
        slideSelected = false;
        deselectGuide();
        // No element under the cursor (gray margin OR slide background) → arm a
        // marquee. A no-drag release falls back to a deselect click; a drag
        // selects overlapped elements. focusChain is snapshotted now and left
        // untouched during the marquee (only the no-drag click drops focus).
        const focusSnapshot = focusChain.slice();
        const slideHost = e.target.closest && e.target.closest(".slide-host");
        const target = slideHost ? findInteractionTarget(e) : null;
        if (!target) {
            armMarquee(e, focusSnapshot);
            return;
        }
        // Table focus mode: a press inside the focused table selects cells
        // (plain / Shift range / Cmd toggle) instead of dragging the element.
        const ftid = focusedTableId();
        if (ftid && elementChain(target).some(function (n) { return n.dataset.elementId === ftid; })) {
            const rc = cellAtPoint(ftid, e.clientX, e.clientY);
            if (rc) {
                selectCell(ftid, rc, e);
                document.body.style.userSelect = "none";
                return;
            }
        }
        // Element press. Leaving the deepest focused group (clicking an element
        // outside it) drops back to top-level selection before sending.
        if (focusChain.length > 0) {
            const deep = focusChain[focusChain.length - 1];
            const insideFocus = elementChain(target).some(function (n) {
                return n.dataset.elementId === deep;
            });
            if (!insideFocus) { focusChain = []; tableCellSel = null; }
        }
        const elementId = target.dataset.elementId;
        // Pressing an already-selected element while several are selected (no
        // Shift) starts a MULTI drag: keep the selection and drag them all. A
        // no-drag release collapses to just this element (handled in mouseup).
        const inSelection = currentSelectionIds.indexOf(elementId) >= 0;
        const multi = inSelection && currentSelectionIds.length > 1 && !e.shiftKey;
        if (multi) {
            const targets = [];
            for (let i = 0; i < currentSelectionIds.length; i++) {
                const node = findElement(currentSelectionIds[i]);
                if (node) {
                    targets.push({ id: currentSelectionIds[i], node: node });
                }
            }
            dragState = {
                element_id: elementId,
                start: { x: e.clientX, y: e.clientY },
                started: false,
                target: target,
                multi: true,
                targets: targets,
                collapseId: elementId,
            };
        } else {
            window.__deck.send("Interaction", {
                kind: "ElementClicked",
                element_id: elementId,
                modifiers: readModifiers(e),
                position: { x: e.clientX, y: e.clientY },
            });
            dragState = {
                element_id: elementId,
                start: { x: e.clientX, y: e.clientY },
                started: false,
                target: target,
            };
        }
        // Disable browser text selection for the duration of this gesture.
        // Cleared unconditionally in onMouseUp regardless of whether a drag started.
        document.body.style.userSelect = "none";
    }

    // onMouseMove
    // Inputs: a MouseEvent on window (so the drag continues even outside
    // the viewport).
    // Output: side-effect; if dragState is armed and the cursor crossed
    // DRAG_THRESHOLD, sends ElementDragStarted once, then optimistically
    // transforms the element and throttles an ElementDragged IPC via rAF.
    // Deltas are divided by the viewport scale so translate values are in
    // slide coordinates (1920px space), not screen pixels.
    // ---------- marquee (drag-to-select) ----------
    // armMarquee — start a marquee session on a background press. focusSnapshot
    // is the level whose elements the marquee will select within.
    function armMarquee(e, focusSnapshot) {
        marquee = {
            startX: e.clientX,
            startY: e.clientY,
            shift: !!e.shiftKey,
            baseline: currentSelectionIds.slice(),
            focusSnapshot: focusSnapshot,
            active: false,
        };
        document.body.style.userSelect = "none";
    }

    function ensureMarqueeBox() {
        const stage = document.getElementById("viewport-container");
        if (!stage) {
            return null;
        }
        let box = document.getElementById("marquee-box");
        if (!box) {
            box = document.createElement("div");
            box.id = "marquee-box";
            stage.appendChild(box);
        }
        return box;
    }

    function updateMarqueeBox(cx, cy) {
        const stage = document.getElementById("viewport-container");
        const box = ensureMarqueeBox();
        if (!stage || !box) {
            return;
        }
        const sr = stage.getBoundingClientRect();
        box.style.display = "block";
        box.style.left = (Math.min(marquee.startX, cx) - sr.left) + "px";
        box.style.top = (Math.min(marquee.startY, cy) - sr.top) + "px";
        box.style.width = Math.abs(cx - marquee.startX) + "px";
        box.style.height = Math.abs(cy - marquee.startY) + "px";
    }

    function clearMarqueeBox() {
        const box = document.getElementById("marquee-box");
        if (box) {
            box.style.display = "none";
        }
    }

    function rectsIntersect(a, b) {
        return !(b.right < a.left || b.left > a.right || b.bottom < a.top || b.top > a.bottom);
    }

    // marqueeCandidates — elements at the given focus level: top-level elements
    // (no element ancestor) when the snapshot is empty, else the direct children
    // of the deepest snapshot group.
    function marqueeCandidates(focusSnapshot) {
        if (!currentShadow) {
            return [];
        }
        const levelParent = focusSnapshot.length
            ? focusSnapshot[focusSnapshot.length - 1] : null;
        const out = [];
        const nodes = currentShadow.querySelectorAll("[data-element-id]");
        for (let i = 0; i < nodes.length; i++) {
            const node = nodes[i];
            let p = node.parentElement;
            let pid = null;
            while (p && p !== currentShadow) {
                if (p.classList && p.classList.contains("slide-host")) {
                    break;
                }
                if (p.dataset && p.dataset.elementId) {
                    pid = p.dataset.elementId;
                    break;
                }
                p = p.parentElement;
            }
            if (pid === levelParent) {
                out.push(node);
            }
        }
        return out;
    }

    // marqueeIds — selection ids for the current box (Shift unions with the
    // baseline). Pure read of the DOM + the marquee session.
    function marqueeIds(cx, cy) {
        const rect = {
            left: Math.min(marquee.startX, cx),
            top: Math.min(marquee.startY, cy),
            right: Math.max(marquee.startX, cx),
            bottom: Math.max(marquee.startY, cy),
        };
        const hits = [];
        const cands = marqueeCandidates(marquee.focusSnapshot);
        for (let i = 0; i < cands.length; i++) {
            if (rectsIntersect(rect, cands[i].getBoundingClientRect())) {
                hits.push(cands[i].dataset.elementId);
            }
        }
        if (!marquee.shift) {
            return hits;
        }
        const ids = marquee.baseline.slice();
        for (let i = 0; i < hits.length; i++) {
            if (ids.indexOf(hits[i]) < 0) {
                ids.push(hits[i]);
            }
        }
        return ids;
    }

    // sendMarqueeSelection — push the selection only when the id set changed,
    // so a live-updating marquee does not flood the same selection every frame.
    function sendMarqueeSelection(ids) {
        const key = ids.join(",");
        if (key === marquee.lastSentKey) {
            return;
        }
        marquee.lastSentKey = key;
        window.__deck.send("Interaction", {
            kind: "SetSelectionFromPanel", element_ids: ids,
        });
    }

    // finalizeMarquee — on release: a no-drag click deselects (as before); a
    // drag commits the final overlapped selection.
    function finalizeMarquee(e) {
        const m = marquee;
        const active = m.active;
        if (active) {
            sendMarqueeSelection(marqueeIds(e.clientX, e.clientY));
        }
        marquee = null;
        document.body.style.userSelect = "";
        clearMarqueeBox();
        if (!active) {
            focusChain = [];
            tableCellSel = null;
            window.__deck.send("Interaction", {
                kind: "BackgroundClicked",
                position: { x: e.clientX, y: e.clientY },
            });
        }
    }

    function onMouseMove(e) {
        if (marquee) {
            const mdx = e.clientX - marquee.startX;
            const mdy = e.clientY - marquee.startY;
            if (!marquee.active) {
                if (Math.hypot(mdx, mdy) < DRAG_THRESHOLD) {
                    return;
                }
                marquee.active = true;
            }
            updateMarqueeBox(e.clientX, e.clientY);
            sendMarqueeSelection(marqueeIds(e.clientX, e.clientY));
            return;
        }
        if (!dragState) {
            return;
        }
        const dx = e.clientX - dragState.start.x;
        const dy = e.clientY - dragState.start.y;
        if (!dragState.started) {
            if (Math.hypot(dx, dy) < DRAG_THRESHOLD) {
                return;
            }
            dragState.started = true;
            dragState.snapTargets = buildSnapTargets(dragState.element_id);
            dragState.baseRect = movingRectFromStyle(dragState.target);
            // Track Shift press/release during the drag so axis-lock toggles
            // live even when the mouse is stationary.
            window.addEventListener("keydown", onDragKeyChange);
            window.addEventListener("keyup", onDragKeyChange);
            window.__deck.send("Interaction", {
                kind: "ElementDragStarted",
                element_id: dragState.element_id,
                position: { x: dragState.start.x, y: dragState.start.y },
            });
        }
        renderDrag(e.clientX, e.clientY, e.shiftKey, e.metaKey);
    }

    // snappedDragDelta
    // Inputs: the raw slide-space delta (dxSlide, dySlide), the viewport scale,
    // the Cmd suppress flag, and whether to draw guides. Output: { x, y }
    // snapped slide-space delta. Feeds the raw target rect through the snap
    // engine and returns the corrected delta; renders guides as a side-effect
    // when draw is true. Falls back to the raw delta when no snapshot exists.
    function snappedDragDelta(dxSlide, dySlide, scale, suppress, draw) {
        if (!dragState || !dragState.snapTargets || !dragState.baseRect) {
            return { x: dxSlide, y: dySlide };
        }
        const want = {
            x: dragState.baseRect.x + dxSlide,
            y: dragState.baseRect.y + dySlide,
            w: dragState.baseRect.w,
            h: dragState.baseRect.h,
        };
        const out = window.__snap.forDrag(want, dragState.snapTargets, {
            threshold: 3 / scale,
            gridEnabled: gridEnabled,
            suppress: !!suppress,
        });
        if (draw) {
            renderGuides(out.guides);
        }
        return {
            x: out.rect.x - dragState.baseRect.x,
            y: out.rect.y - dragState.baseRect.y,
        };
    }

    // computeDragDelta
    // Inputs: the pointer position (screen px), viewport scale, and the live
    // Shift/Cmd state, plus whether to draw guides. Output: the final
    // slide-space { x, y } delta after axis-lock (Shift), snapping, and a
    // re-zero of the locked axis so snapping can't nudge it off the line.
    function computeDragDelta(clientX, clientY, scale, shiftHeld, metaHeld, draw) {
        const dxSlide = (clientX - dragState.start.x) / scale;
        const dySlide = (clientY - dragState.start.y) / scale;
        const locked = window.__snap.axisLock(dxSlide, dySlide, shiftHeld);
        const snapped = snappedDragDelta(locked.dx, locked.dy, scale, metaHeld, draw);
        if (locked.lockedAxis === "x") {
            snapped.x = 0;
        } else if (locked.lockedAxis === "y") {
            snapped.y = 0;
        }
        return snapped;
    }

    // renderDrag
    // Inputs: the pointer position and live Shift/Cmd state. Output: side-effect;
    // records lastMouse, applies the optimistic transform for the computed
    // delta, and posts a throttled ElementDragged. Shared by onMouseMove and the
    // drag-scoped Shift key handler (so a Shift press/release with a stationary
    // mouse still updates the preview).
    function renderDrag(clientX, clientY, shiftHeld, metaHeld) {
        if (!dragState || !dragState.started) {
            return;
        }
        const scale = getViewportScale();
        dragState.lastMouse = { x: clientX, y: clientY };
        // Delta is computed once from the primary element (snapping uses its
        // rect); in a multi drag every selected element gets the same delta.
        const d = computeDragDelta(clientX, clientY, scale, shiftHeld, metaHeld, true);
        if (dragState.multi) {
            for (let i = 0; i < dragState.targets.length; i++) {
                optimisticTransform(dragState.targets[i].node, d.x, d.y);
            }
        } else {
            optimisticTransform(dragState.target, d.x, d.y);
            reportDragThrottled(dragState.element_id, { x: d.x, y: d.y }, { x: clientX, y: clientY });
        }
    }

    // onDragKeyChange
    // Inputs: a Shift keydown/keyup during a drag. Output: side-effect; re-runs
    // the drag render at the last mouse position so the element snaps to / leaves
    // the locked axis the instant Shift changes, without any mouse movement.
    function onDragKeyChange(e) {
        if (e.key !== "Shift" || !dragState || !dragState.started || !dragState.lastMouse) {
            return;
        }
        renderDrag(dragState.lastMouse.x, dragState.lastMouse.y, e.shiftKey, e.metaKey);
    }

    // onMouseUp
    // Inputs: a MouseEvent on window.
    // Output: side-effect; if a drag was in progress, sends
    // ElementDragEnded with the final delta (in slide coordinates) and
    // registers pendingDragEnd so the optimistic transform is held until
    // the SetStyle(left|top) patch arrives — avoiding a visible flash.
    // A safety timeout removes the transform if no patch arrives within
    // PENDING_TRANSFORM_TIMEOUT_MS. If no drag was in progress (click
    // only), clears dragState.
    function onMouseUp(e) {
        if (marquee) {
            finalizeMarquee(e);
            return;
        }
        if (!dragState) {
            return;
        }
        window.removeEventListener("keydown", onDragKeyChange);
        window.removeEventListener("keyup", onDragKeyChange);
        if (dragState.started) {
            const scale = getViewportScale();
            const snapped = computeDragDelta(
                e.clientX, e.clientY, scale, e.shiftKey, e.metaKey, false);
            // Hold each moved element's optimistic transform until its
            // SetStyle(left|top) patch lands (applyOnePatch clears it); a safety
            // timeout clears any straggler.
            const held = dragState.multi
                ? dragState.targets.slice()
                : [{ id: dragState.element_id, node: dragState.target }];
            for (let i = 0; i < held.length; i++) {
                pendingDragEnds[held[i].id] = held[i].node;
            }
            (function (ids) {
                setTimeout(function () {
                    for (let i = 0; i < ids.length; i++) {
                        if (pendingDragEnds[ids[i]]) {
                            pendingDragEnds[ids[i]].style.removeProperty("transform");
                            delete pendingDragEnds[ids[i]];
                        }
                    }
                }, PENDING_TRANSFORM_TIMEOUT_MS);
            }(held.map(function (h) { return h.id; })));
            if (dragState.multi) {
                window.__deck.send("Interaction", {
                    kind: "ElementsDragEnded",
                    element_ids: dragState.targets.map(function (t) { return t.id; }),
                    delta: { x: snapped.x, y: snapped.y },
                });
            } else {
                window.__deck.send("Interaction", {
                    kind: "ElementDragEnded",
                    element_id: dragState.element_id,
                    delta: { x: snapped.x, y: snapped.y },
                });
            }
        } else if (dragState.multi && !e.shiftKey) {
            // No-drag click on one of several selected items → collapse to it.
            window.__deck.send("Interaction", {
                kind: "SetSelectionFromPanel", element_ids: [dragState.collapseId],
            });
        }
        // Restore text selectability now that the gesture is over.
        document.body.style.userSelect = "";
        clearGuides();
        dragState = null;
    }

    // optimisticTransform
    // Inputs: the DOM element to transform, the cumulative dx/dy in
    // viewport CSS pixels.
    // Output: side-effect; sets `transform: translate(dx, dy)` while
    // dragging, removes the property when dx == dy == 0. Also nudges
    // the selection overlay to track the optimistic position.
    function optimisticTransform(el, dx, dy) {
        if (!el) {
            return;
        }
        if (dx === 0 && dy === 0) {
            el.style.removeProperty("transform");
        } else {
            el.style.transform = "translate(" + dx + "px, " + dy + "px)";
        }
        if (currentSelectionIds.length > 0) {
            updateSelectionOverlay();
        }
    }

    // reportDragThrottled
    // Inputs: element id, cumulative delta from drag start, current
    // pointer position.
    // Output: side-effect; coalesces multiple mouse events per frame
    // into one Interaction message via requestAnimationFrame.
    function reportDragThrottled(elementId, delta, position) {
        pendingDrag = { element_id: elementId, delta: delta, position: position };
        if (dragRafScheduled) {
            return;
        }
        dragRafScheduled = true;
        requestAnimationFrame(function () {
            if (pendingDrag) {
                window.__deck.send("Interaction", {
                    kind: "ElementDragged",
                    element_id: pendingDrag.element_id,
                    delta: pendingDrag.delta,
                    position: pendingDrag.position,
                });
                pendingDrag = null;
            }
            dragRafScheduled = false;
        });
    }

    // ---------- IPC handlers ----------
    const handlers = {
        MountSlide: function (payload) {
            mountSlide(
                payload.slide_id,
                payload.slide_html,
                payload.theme_css,
                payload.globals_css
            );
            refreshInspector();
            // Keep the cached HTML for this slide fresh so its thumbnail
            // reflects the latest mount. Theme CSS may also have changed
            // (theme editor mode, future). Each thumbnail re-renders the
            // affected slide only.
            updateThumbnailHtml(payload.slide_id, payload.slide_html, payload.theme_css);
            highlightActiveThumbnail(payload.slide_id);
            // Guides belong to a slide; a switch deselects any guide and redraws
            // for the newly active slide.
            selectedGuideId = null;
            refreshRulers();
            renderRulerGuides();
            renderCanvasScrim();
        },
        ApplyPatch: function (payload) {
            applyPatch(payload);
            // Any patch may have moved or restyled the selected element.
            // Inspector reads from the shadow DOM, so it stays the
            // single source of truth visible to the user.
            refreshInspector();
        },
        SetSelection: function (payload) {
            const ids = (payload && Array.isArray(payload.element_ids))
                ? payload.element_ids
                : [];
            currentSelectionIds = ids.slice();
            // An element selection is never also a slide or guide selection.
            if (currentSelectionIds.length > 0) {
                slideSelected = false;
                if (selectedGuideId !== null) {
                    selectedGuideId = null;
                    renderRulerGuides();
                }
            }
            updateSelectionOverlay();
            refreshInspector();
            updateObjectPanelSelection();
            updateSlideFocusState();
            refreshAnimationsSection();
        },
        ObjectTreeUpdate: function (payload) {
            renderObjectPanel(payload);
        },
        SlideListUpdate: function (payload) {
            renderThumbnailRow(payload, "slide");
        },
        LayoutListUpdate: function (payload) {
            renderThumbnailRow(payload, "layout");
            // Cache the active layout's background so the Slide box can show its
            // Fill/Image controls in layout mode.
            layoutBgData = null;
            if (payload && Array.isArray(payload.layouts)) {
                for (let i = 0; i < payload.layouts.length; i++) {
                    if (payload.layouts[i].layout_id === payload.active_layout_id) {
                        layoutBgData = payload.layouts[i];
                        break;
                    }
                }
            }
            if (currentMode === "layout" && currentSelectionIds.length === 0) {
                refreshInspector();
            }
            // Keep the globals textarea in sync with the committed value.
            if (payload && typeof payload.globals_css === "string") {
                currentGlobalsCss = payload.globals_css;
                const ta = document.getElementById("globals-css");
                if (ta && document.activeElement !== ta) {
                    ta.value = payload.globals_css;
                }
            }
        },
        SlideLayoutPickerData: function (payload) {
            openLayoutPicker(payload);
        },
        ChromiumDownloadProgress: function (payload) {
            showChromiumDownload(payload && payload.received, payload && payload.total);
        },
        ChromiumDownloadDone: function (payload) {
            finishChromiumDownload(payload && payload.ok, payload && payload.message);
        },
        SetMode: function (payload) {
            const mode = (payload && payload.mode) || "slide";
            currentMode = mode;
            document.body.dataset.mode = mode;
            // The no-selection pane differs by mode (Slide box vs globals), so
            // re-evaluate inspector visibility on a mode switch.
            refreshInspector();
        },
        Configure: function (payload) {
            builtinKeyframesCss = (payload && payload.animation_keyframes_css) || "";
            animationCatalog = (payload && payload.animation_catalog) || [];
            initDeckTitle(payload && payload.deck_title, payload && payload.focus_title);
        },
        SlideAnimationsUpdate: function (payload) {
            slideAnimations = (payload && payload.entries) || [];
            refreshAnimationsSection();
            renderSlideAnimations();
        },
        GuidesUpdate: function (payload) {
            // Re-hydrate the active canvas's guides from authoritative state.
            // ids are index-based so a selection survives the post-commit echo
            // (a moved guide keeps its index). Inherited guides are read-only.
            const own = (payload && payload.own) || [];
            const inh = (payload && payload.inherited) || [];
            guideOwn = own.map(function (g, i) {
                return { id: "g" + i, index: i, orient: g.axis, pos: g.pos };
            });
            guideInherited = inh.map(function (g, i) {
                return { id: "gi" + i, index: i, orient: g.axis, pos: g.pos };
            });
            if (selectedGuideId !== null
                    && !guideOwn.some(function (x) { return x.id === selectedGuideId; })) {
                selectedGuideId = null;
                hideGuideInspector();
            }
            renderRulerGuides();
            showGuideInspector();
        },
        SaveStateUpdate: function (payload) {
            const meta = document.querySelector(".doc-meta");
            if (meta) {
                meta.classList.toggle("doc-meta--dirty", payload === true);
            }
        },
        ShowQuitDialog: function () {
            showQuitDialog();
        },
        SlideInspectorUpdate: function (payload) {
            slideInspectorData = payload || null;
            // Refresh the Slide box if it is the visible state (no selection,
            // slide mode). refreshInspector decides whether to render it.
            if (currentSelectionIds.length === 0) {
                refreshInspector();
            }
        },
        Notice: function (payload) {
            showToast((payload && payload.message) || "", payload && payload.detail);
        },
        AssetsUpdate: function (payload) {
            const assets = (payload && Array.isArray(payload.assets))
                ? payload.assets
                : [];
            for (let i = 0; i < assets.length; i++) {
                ingestAssetPayload(assets[i]);
            }
            refreshAssetVarStyle();
            refreshThumbnailAssetVars();
        },
        AssetAdded: function (payload) {
            ingestAssetPayload(payload);
            refreshAssetVarStyle();
            refreshThumbnailAssetVars();
        },
        FontList: function (payload) {
            availableFonts = (payload && Array.isArray(payload.families))
                ? payload.families : [];
        },
    };

    // ---------- __deck bridge ----------
    window.__deck = {
        send: function (type, payload) {
            const envelope = {
                id: newId(),
                timestamp: Date.now(),
                type: type,
            };
            if (payload !== null && payload !== undefined) {
                envelope.payload = payload;
            }
            if (!window.ipc || typeof window.ipc.postMessage !== "function") {
                console.error("window.ipc.postMessage unavailable");
                return;
            }
            window.ipc.postMessage(JSON.stringify(envelope));
        },
        receive: function (envelopeJson) {
            let msg;
            try {
                msg = JSON.parse(envelopeJson);
            } catch (e) {
                console.error("receive: invalid JSON", e);
                return;
            }
            const handler = handlers[msg.type];
            if (handler) {
                handler(msg.payload);
            } else {
                console.warn("receive: unhandled message type:", msg.type);
            }
        },
    };

    // ---------- resize handles ----------
    // resizeState lives for the duration of one resize gesture:
    //   target           – the DOM element being resized (in the shadow)
    //   elementId        – the slide-coordinate id
    //   handle           – "nw" / "n" / "ne" / ... matching the dot
    //   startMouse       – pointer position at mousedown (screen px)
    //   startRect        – element rect at mousedown in SLIDE coords
    //                      { x, y, w, h }; used as the source of truth
    //                      for every mousemove geometry calculation
    //   aspect           – startRect.w / startRect.h, cached so shift-
    //                      constrained drags don't drift
    //   savedTransform   – the element's prior transform style, restored
    //                      after the resize commits (we set 'none' to
    //                      avoid stacking with the new size).
    let resizeState = null;
    let resizeRafScheduled = false;
    let pendingResize = null;
    // Multi-select proportional scale session (null when idle).
    let multiScaleState = null;
    // Minimum visual size in slide pixels — mirrors the Rust-side
    // MIN_DIMENSION_PX safety clamp so the user can't drag an element
    // to a degenerate state mid-drag either.
    const RESIZE_MIN_PX = 1;
    const RESIZE_THROTTLE_KEY = "kind";  // future: switch between rAF / immediate

    // onResizeHandleMouseDown
    // Inputs: a mousedown MouseEvent on a .selection-handle.
    // Output: side-effect; reads the target element's slide-space rect
    // from its inline style, arms resizeState, sends ElementResizeStarted,
    // and binds window-level move/up handlers so the gesture survives
    // the cursor leaving the handle.
    function onResizeHandleMouseDown(e) {
        if (e.button !== 0) {
            return;
        }
        const handle = e.currentTarget;
        const elementId = handle.dataset.elementId;
        if (!elementId || !currentShadow) {
            return;
        }
        const target = findElement(elementId);
        if (!target) {
            return;
        }
        const decls = parseStyleAttr(target.getAttribute("style") || "");
        const startRect = {
            x: parseFloat(stripPx(decls.left)) || 0,
            y: parseFloat(stripPx(decls.top)) || 0,
            w: parseFloat(stripPx(decls.width)) || 0,
            h: parseFloat(stripPx(decls.height)) || 0,
        };
        if (startRect.w <= 0 || startRect.h <= 0) {
            return;
        }
        // Stop propagation so the viewport mousedown handler does not
        // also fire and arm a drag (that would race with the resize).
        e.stopPropagation();
        e.preventDefault();

        // A cropped image (explicit px background-size) scales its picture
        // proportionally with the box (B-proportional); capture its crop
        // state so each move can rescale the background.
        const cropStart = (target.dataset.elementType === "image")
            ? window.__crop.fromStyles(decls["background-size"], decls["background-position"])
            : null;
        // Groups resize by uniform scale (transform), not by box geometry, and
        // commit via SetGroupScale on drop — entirely client-side. They must NOT
        // open a resize transaction (ElementResizeStarted) since no
        // ElementResizeEnded ever closes it; a leftover open transaction would
        // panic the next undo.
        const isGroup = target.dataset.elementType === "group";
        const priorScale = isGroup
            ? (parseFloat(target.dataset.flexScale || "1") || 1) : 1;
        resizeState = {
            target: target,
            elementId: elementId,
            handle: handle.dataset.handle,
            startMouse: { x: e.clientX, y: e.clientY },
            startRect: startRect,
            aspect: startRect.w / startRect.h,
            savedTransform: target.style.transform || "",
            snapTargets: buildSnapTargets(elementId),
            cropStart: cropStart,
            isGroup: isGroup,
            priorScale: priorScale,
            // The grabbed (visual) box is the unscaled box times the prior
            // scale; map corner drags against it to derive the new scale.
            visualRect: {
                x: startRect.x, y: startRect.y,
                w: startRect.w * priorScale, h: startRect.h * priorScale,
            },
        };
        if (isGroup) {
            // Anchor the scale at the box origin so the handle math matches the
            // visual box. (Rotation, if any, is dropped for the preview only.)
            // ponytail: scale-only preview; rotated groups re-render correct on commit.
            target.style.transformOrigin = "0 0";
        } else {
            // Clear any optimistic transform from a prior drag so the
            // resize math operates on the inline left/top/width/height.
            target.style.transform = "none";
        }
        document.body.style.userSelect = "none";

        if (!isGroup) {
            window.__deck.send("Interaction", {
                kind: "ElementResizeStarted",
                element_id: elementId,
                handle: resizeHandleToRustEnum(handle.dataset.handle),
                position: { x: e.clientX, y: e.clientY },
            });
        }

        window.addEventListener("mousemove", onResizeMouseMove);
        window.addEventListener("mouseup", onResizeMouseUp);
    }

    // resizeHandleToRustEnum
    // Inputs: a handle name from the CSS data-handle attribute.
    // Output: the matching ResizeHandle variant name on the Rust side.
    function resizeHandleToRustEnum(name) {
        switch (name) {
            case "nw": return "TopLeft";
            case "n":  return "Top";
            case "ne": return "TopRight";
            case "e":  return "Right";
            case "se": return "BottomRight";
            case "s":  return "Bottom";
            case "sw": return "BottomLeft";
            case "w":  return "Left";
            default:   return "BottomRight";
        }
    }

    // handleEdges
    // Inputs: a handle name ("nw".."e"). Output: { west, east, north, south }
    // booleans for the edges that move under that handle.
    function handleEdges(name) {
        return {
            west: name.indexOf("w") >= 0,
            east: name.indexOf("e") >= 0,
            north: name.indexOf("n") >= 0,
            south: name.indexOf("s") >= 0,
        };
    }

    // snappedResizeRect
    // Inputs: the rect from computeResizeRect, the source MouseEvent, the
    // viewport scale, and whether to draw guides. Output: the snapped rect.
    // Feeds active edges through the snap engine (alignment + dimension-match
    // + grid) and renders guides as a side-effect when draw is true. Falls
    // back to the input rect when no snapshot exists.
    function snappedResizeRect(rect, e, scale, draw) {
        if (!resizeState || !resizeState.snapTargets) {
            return rect;
        }
        const out = window.__snap.forResize(
            rect, handleEdges(resizeState.handle), resizeState.snapTargets,
            {
                threshold: 3 / scale,
                gridEnabled: gridEnabled,
                suppress: !!e.metaKey,
                shift: !!e.shiftKey,
                alt: !!e.altKey,
                aspect: resizeState.aspect,
            },
        );
        if (draw) {
            renderGuides(out.guides);
        }
        return out.rect;
    }

    // computeResizeRect
    // Inputs: the resize state and the cumulative mouse delta in SLIDE
    // pixels (already scaled), plus modifier flags.
    // Output: { x, y, w, h } — the new slide-space rect.
    // Dataflow:
    //   - Each handle picks which edges move (nw moves left+top,
    //     ne moves right+top, etc).
    //   - shift (aspect): for corner handles, constrain the larger
    //     proportional change to the source aspect ratio.
    //   - alt (center): the OPPOSITE edge mirrors the moving edge, so
    //     the element grows symmetrically around its center.
    function computeResizeRect(state, dx, dy, shift, alt) {
        const handle = state.handle;
        const start = state.startRect;

        // Sign per handle: how dx, dy translate into edge offsets.
        // For each edge, we track the moving offset (dWest, dNorth,
        // dEast, dSouth) — positive values push that edge outward.
        let dWest = 0, dEast = 0, dNorth = 0, dSouth = 0;
        if (handle.indexOf("w") >= 0) { dWest = -dx; }
        if (handle.indexOf("e") >= 0) { dEast =  dx; }
        if (handle.indexOf("n") >= 0) { dNorth = -dy; }
        if (handle.indexOf("s") >= 0) { dSouth =  dy; }

        // Aspect-lock: corner handles get the dominant proportional
        // change applied to both axes. Edge handles ignore shift (their
        // perpendicular dimension is fixed by definition).
        if (shift && isCornerHandle(handle)) {
            const propW = (dWest + dEast) / start.w;
            const propH = (dNorth + dSouth) / start.h;
            const prop = Math.abs(propW) > Math.abs(propH) ? propW : propH;
            const scaledDW = start.w * prop;
            const scaledDH = start.h * prop;
            const wSign = dWest !== 0 ? Math.sign(dWest) : 0;
            const eSign = dEast !== 0 ? Math.sign(dEast) : 0;
            const nSign = dNorth !== 0 ? Math.sign(dNorth) : 0;
            const sSign = dSouth !== 0 ? Math.sign(dSouth) : 0;
            if (wSign !== 0) { dWest = wSign * Math.abs(scaledDW); }
            if (eSign !== 0) { dEast = eSign * Math.abs(scaledDW); }
            if (nSign !== 0) { dNorth = nSign * Math.abs(scaledDH); }
            if (sSign !== 0) { dSouth = sSign * Math.abs(scaledDH); }
        }

        // Center mode: mirror each moving edge to the opposite edge so
        // the centre of the element stays put.
        if (alt) {
            if (dWest !== 0)  { dEast = dWest; }
            if (dEast !== 0 && dWest === 0)  { dWest = dEast; }
            if (dNorth !== 0) { dSouth = dNorth; }
            if (dSouth !== 0 && dNorth === 0) { dNorth = dSouth; }
        }

        let newW = start.w + dWest + dEast;
        let newH = start.h + dNorth + dSouth;
        let newX = start.x - dWest;
        let newY = start.y - dNorth;

        if (newW < RESIZE_MIN_PX) {
            // Clamp without flipping: keep the un-moving edge fixed.
            if (handle.indexOf("w") >= 0) { newX = start.x + start.w - RESIZE_MIN_PX; }
            newW = RESIZE_MIN_PX;
        }
        if (newH < RESIZE_MIN_PX) {
            if (handle.indexOf("n") >= 0) { newY = start.y + start.h - RESIZE_MIN_PX; }
            newH = RESIZE_MIN_PX;
        }
        return { x: newX, y: newY, w: newW, h: newH };
    }

    function isCornerHandle(name) {
        return name === "nw" || name === "ne" || name === "sw" || name === "se";
    }

    // onResizeMouseMove
    // Inputs: a mousemove MouseEvent at window level.
    // Output: side-effect; computes the new slide-space rect, applies
    // it optimistically by writing inline left/top/width/height on the
    // shadow-DOM element, refreshes the overlay handles, and posts a
    // throttled ElementResized event.
    // groupResizeScale — absolute group scale for the current pointer position,
    // derived from the corner drag against the grabbed visual box (aspect
    // locked). Floored at 0.01 so the commit's scale-must-be-positive holds.
    function groupResizeScale(e, scale) {
        const dx = (e.clientX - resizeState.startMouse.x) / scale;
        const dy = (e.clientY - resizeState.startMouse.y) / scale;
        const synthetic = { handle: resizeState.handle, startRect: resizeState.visualRect };
        const r = computeResizeRect(synthetic, dx, dy, true, false);
        const f = resizeState.visualRect.w > 0 ? (r.w / resizeState.visualRect.w) : 1;
        return Math.max(0.01, resizeState.priorScale * f);
    }

    function onResizeMouseMove(e) {
        if (!resizeState) {
            return;
        }
        const scale = getViewportScale();
        if (resizeState.isGroup) {
            // Live uniform-scale preview so the group's contents grow/shrink
            // while dragging instead of snapping on drop.
            const s = groupResizeScale(e, scale);
            resizeState.target.style.transform = "scale(" + s + ")";
            updateSelectionOverlay();
            return;
        }
        const dx = (e.clientX - resizeState.startMouse.x) / scale;
        const dy = (e.clientY - resizeState.startMouse.y) / scale;
        const rect = snappedResizeRect(computeResizeRect(
            resizeState, dx, dy, !!e.shiftKey, !!e.altKey,
        ), e, scale, true);
        applyOptimisticRect(resizeState.target, rect);
        applyOptimisticCropScale(rect);
        updateSelectionOverlay();
        scheduleResizeReport(rect, e);
    }

    // croppedResizeStyles
    // Inputs: the new box rect. Output: { backgroundSize, backgroundPosition }
    // scaled proportionally with the box for a cropped image, or null when the
    // element being resized is not a cropped image.
    function croppedResizeStyles(rect) {
        if (!resizeState || !resizeState.cropStart) {
            return null;
        }
        const scaled = window.__crop.scaleForBox(
            resizeState.cropStart,
            resizeState.startRect.w, resizeState.startRect.h,
            rect.w, rect.h);
        return window.__crop.toStyles(scaled);
    }

    // applyOptimisticCropScale
    // Inputs: the new box rect. Output: side-effect; writes the scaled
    // background-size/position on a cropped image so the picture scales with
    // the box during the gesture. No-op otherwise.
    function applyOptimisticCropScale(rect) {
        const css = croppedResizeStyles(rect);
        if (css && resizeState) {
            resizeState.target.style.backgroundSize = css.backgroundSize;
            resizeState.target.style.backgroundPosition = css.backgroundPosition;
        }
    }

    function applyOptimisticRect(target, rect) {
        if (!target) {
            return;
        }
        target.style.left = rect.x + "px";
        target.style.top = rect.y + "px";
        target.style.width = rect.w + "px";
        target.style.height = rect.h + "px";
    }

    function scheduleResizeReport(rect, e) {
        if (!resizeState) {
            return;
        }
        pendingResize = {
            element_id: resizeState.elementId,
            handle: resizeHandleToRustEnum(resizeState.handle),
            new_position: { x: rect.x, y: rect.y },
            new_size: { width: rect.w, height: rect.h },
        };
        if (resizeRafScheduled) {
            return;
        }
        resizeRafScheduled = true;
        window.requestAnimationFrame(function () {
            if (pendingResize) {
                window.__deck.send("Interaction", Object.assign(
                    { kind: "ElementResized" },
                    pendingResize,
                ));
                pendingResize = null;
            }
            resizeRafScheduled = false;
        });
    }

    // onResizeMouseUp
    // Inputs: a mouseup MouseEvent.
    // Output: side-effect; ends the gesture, sends ElementResizeEnded
    // with the final slide-space rect, restores the saved transform,
    // and detaches the window-level listeners.
    function onResizeMouseUp(e) {
        if (!resizeState) {
            return;
        }
        const scale = getViewportScale();
        const dx = (e.clientX - resizeState.startMouse.x) / scale;
        const dy = (e.clientY - resizeState.startMouse.y) / scale;
        // Groups scale uniformly: commit the previewed scale via SetGroupScale.
        // The optimistic transform stays until the remount re-bakes it (avoids a
        // flash). No transaction was opened, so nothing to close here.
        if (resizeState.isGroup) {
            const finalScale = groupResizeScale(e, scale);
            window.__deck.send("Interaction", {
                kind: "SetGroupScale", element_id: resizeState.elementId,
                scale: finalScale,
            });
            clearGuides();
            document.body.style.userSelect = "";
            resizeState = null;
            pendingResize = null;
            window.removeEventListener("mousemove", onResizeMouseMove);
            window.removeEventListener("mouseup", onResizeMouseUp);
            updateSelectionOverlay();
            return;
        }
        const rect = snappedResizeRect(computeResizeRect(
            resizeState, dx, dy, !!e.shiftKey, !!e.altKey,
        ), e, scale, false);
        applyOptimisticRect(resizeState.target, rect);
        const cropCss = croppedResizeStyles(rect);
        applyOptimisticCropScale(rect);
        const msg = {
            kind: "ElementResizeEnded",
            element_id: resizeState.elementId,
            new_position: { x: rect.x, y: rect.y },
            new_size: { width: rect.w, height: rect.h },
        };
        if (cropCss) {
            msg.background_size = cropCss.backgroundSize;
            msg.background_position = cropCss.backgroundPosition;
        }
        window.__deck.send("Interaction", msg);
        clearGuides();
        if (resizeState.savedTransform === "") {
            resizeState.target.style.removeProperty("transform");
        } else {
            resizeState.target.style.transform = resizeState.savedTransform;
        }
        document.body.style.userSelect = "";
        resizeState = null;
        pendingResize = null;
        window.removeEventListener("mousemove", onResizeMouseMove);
        window.removeEventListener("mouseup", onResizeMouseUp);
        updateSelectionOverlay();
    }

    // ---------- multi-select proportional scale ----------
    // onMultiScaleMouseDown — grab a corner of the multi-selection bbox. Builds
    // the slide-space union box + per-element rects, anchors at the opposite
    // corner, and previews via a per-element transform about that anchor.
    function onMultiScaleMouseDown(e) {
        if (e.button !== 0 || !currentShadow) {
            return;
        }
        e.preventDefault();
        e.stopPropagation();
        const items = [];
        let ul = Infinity, ut = Infinity, ur = -Infinity, ub = -Infinity;
        for (let i = 0; i < currentSelectionIds.length; i++) {
            const node = findElement(currentSelectionIds[i]);
            if (!node) {
                continue;
            }
            const r = movingRectFromStyle(node);
            items.push({ id: currentSelectionIds[i], node: node, rect: r });
            ul = Math.min(ul, r.x); ut = Math.min(ut, r.y);
            ur = Math.max(ur, r.x + r.w); ub = Math.max(ub, r.y + r.h);
        }
        if (items.length < 2 || ur <= ul || ub <= ut) {
            return;
        }
        const name = e.currentTarget.dataset.handle;
        // Grabbed corner + opposite corner (anchor) in slide coords.
        const cornerX = name.indexOf("w") >= 0 ? ul : ur;
        const cornerY = name.indexOf("n") >= 0 ? ut : ub;
        const anchor = {
            x: name.indexOf("w") >= 0 ? ur : ul,
            y: name.indexOf("n") >= 0 ? ub : ut,
        };
        multiScaleState = { items: items, anchor: anchor, corner: { x: cornerX, y: cornerY } };
        document.body.style.userSelect = "none";
        window.addEventListener("mousemove", onMultiScaleMouseMove);
        window.addEventListener("mouseup", onMultiScaleMouseUp);
    }

    // multiScaleFactor — uniform factor from the pointer vs the anchor, using
    // the axis that moved most (so either-axis drag scales proportionally).
    function multiScaleFactor(e) {
        const stage = document.getElementById("viewport-container").getBoundingClientRect();
        const m = canvasMetrics();
        if (!m) {
            return 1;
        }
        const px = (e.clientX - stage.left - m.ox) / m.scale;
        const py = (e.clientY - stage.top - m.oy) / m.scale;
        const s = multiScaleState;
        const dx = s.corner.x - s.anchor.x;
        const dy = s.corner.y - s.anchor.y;
        const fx = Math.abs(dx) > 0.001 ? (px - s.anchor.x) / dx : 1;
        const fy = Math.abs(dy) > 0.001 ? (py - s.anchor.y) / dy : 1;
        return Math.max(0.05, Math.max(fx, fy));
    }

    function onMultiScaleMouseMove(e) {
        if (!multiScaleState) {
            return;
        }
        const f = multiScaleFactor(e);
        const a = multiScaleState.anchor;
        for (let i = 0; i < multiScaleState.items.length; i++) {
            const it = multiScaleState.items[i];
            it.node.style.transformOrigin = (a.x - it.rect.x) + "px " + (a.y - it.rect.y) + "px";
            it.node.style.transform = "scale(" + f + ")";
        }
    }

    function onMultiScaleMouseUp(e) {
        window.removeEventListener("mousemove", onMultiScaleMouseMove);
        window.removeEventListener("mouseup", onMultiScaleMouseUp);
        document.body.style.userSelect = "";
        const s = multiScaleState;
        multiScaleState = null;
        if (!s) {
            return;
        }
        const f = multiScaleFactor(e);
        // Clear the preview transforms; the remount re-bakes the geometry.
        for (let i = 0; i < s.items.length; i++) {
            s.items[i].node.style.removeProperty("transform");
            s.items[i].node.style.removeProperty("transform-origin");
        }
        if (Math.abs(f - 1) < 0.001) {
            return;
        }
        window.__deck.send("Interaction", {
            kind: "ScaleElements",
            element_ids: s.items.map(function (it) { return it.id; }),
            factor: f,
            anchor: { x: s.anchor.x, y: s.anchor.y },
        });
    }

    // ---------- inspector ----------
    // Section definitions. Each entry describes one collapsable section
    // with one or more property rows. `prop` is the wire name posted in
    // PropertyChanged events. `kind` controls coercion: "number" sends a
    // plain numeric string, "rotation-deg" converts degrees → radians on
    // send and radians → degrees on display, "css" sends the raw string.
    // `readonly` sections (z-index) render disabled inputs.
    // Element types an inspector section applies to. Shared geometry sections
    // (Position/Size/Transform) apply to every type; Appearance to boxy element
    // types; Typography to text only. Shared sections are listed first so
    // switching selection only changes the tail of the pane.
    const ALL_TYPES = ["text", "image", "shape", "media", "group", "table", "embed"];
    const NON_GROUP_TYPES = ["text", "image", "shape", "media", "table", "embed"];
    // Tables join the boxy + text type lists so the table ELEMENT gets Fill /
    // Border / Shadow / Typography. The same controls drive per-cell styling
    // when a cell set is active (sendPropertyChanged routes to CellStyleChanged);
    // table-level styles render behind cell style_overrides via inheritance.
    const BOXY_TYPES = ["text", "image", "shape", "media", "table"];
    const TEXT_TYPES = ["text", "table"];

    // Segmented-selector icons (inline SVG markup, currentColor stroke).
    // Declared before INSPECTOR_SECTIONS because its initializer references them.
    function segIcon(d) {
        return '<svg width="15" height="15" viewBox="0 0 24 24" fill="none"'
            + ' stroke="currentColor" stroke-width="1.9" stroke-linecap="round"'
            + ' stroke-linejoin="round"><path d="' + d + '"/></svg>';
    }
    const ALIGN_ICONS = {
        left: segIcon("M4 6h16M4 11h10M4 16h13"),
        center: segIcon("M4 6h16M7 11h10M5 16h14"),
        right: segIcon("M4 6h16M10 11h10M7 16h13"),
        justify: segIcon("M4 6h16M4 11h16M4 16h16"),
    };
    const VALIGN_ICONS = {
        top: segIcon("M4 5h16M10 9v8M14 9v8"),
        middle: segIcon("M4 12h16M10 6v3M10 15v3M14 6v3M14 15v3"),
        bottom: segIcon("M4 19h16M10 7v8M14 7v8"),
    };

    const INSPECTOR_SECTIONS = [
        {
            id: "transform",
            label: "Transform",
            appliesTo: NON_GROUP_TYPES,
            fields: [
                { prop: "x", label: "X", kind: "number", suffix: "px" },
                { prop: "y", label: "Y", kind: "number", suffix: "px" },
                { prop: "size", label: "Size", kind: "size-row", full: true, composite: true },
                { prop: "rotation", label: "Rotation", kind: "rotation-deg", suffix: "°" },
                { prop: "opacity", label: "Opacity", kind: "number", suffix: "" },
            ],
        },
        {
            id: "presets",
            label: "Presets",
            appliesTo: ALL_TYPES,
            fields: [
                { prop: "preset", label: "Style preset", kind: "presets", full: true, composite: true },
            ],
        },
        {
            id: "fill",
            label: "Fill",
            appliesTo: BOXY_TYPES,
            fields: [
                { prop: "background-color", label: "Fill", kind: "swatch", full: true, composite: true },
                { prop: "background-image", label: "Image", kind: "fill-image", full: true, composite: true },
                { prop: "background-size", label: "Object fit", kind: "object-fit", full: true, composite: true },
            ],
        },
        {
            id: "border",
            label: "Border",
            appliesTo: BOXY_TYPES,
            fields: [
                { prop: "border-style", label: "Style", kind: "border-style", full: true, composite: true },
                { prop: "border-width", label: "Width", kind: "cluster", full: true, composite: true, cluster: "width" },
                { prop: "border-color", label: "Color", kind: "swatch", full: true, composite: true },
                { prop: "border-radius", label: "Corner radius", kind: "cluster", full: true, composite: true, cluster: "radius" },
            ],
        },
        {
            id: "shadow",
            label: "Shadow",
            appliesTo: BOXY_TYPES,
            fields: [
                { prop: "box-shadow", label: "Shadow", kind: "shadow", full: true, composite: true, noLabel: true },
            ],
        },
        {
            id: "typography",
            label: "Typography",
            appliesTo: TEXT_TYPES,
            fields: [
                { prop: "font-family", label: "Font", kind: "font-combo", full: true, composite: true },
                { prop: "font-size", label: "Size", kind: "number", unit: "px", unitSelect: true },
                { prop: "font-weight", label: "Weight", kind: "number", suffix: "" },
                { prop: "line-height", label: "Line Height", kind: "number", suffix: "" },
                { prop: "letter-spacing", label: "Letter Spacing", kind: "number", unit: "px", unitSelect: true },
                {
                    prop: "text-align", label: "Alignment", kind: "segment", full: true,
                    options: [
                        { value: "left", icon: ALIGN_ICONS.left, tip: "Align left" },
                        { value: "center", icon: ALIGN_ICONS.center, tip: "Center" },
                        { value: "right", icon: ALIGN_ICONS.right, tip: "Align right" },
                        { value: "justify", icon: ALIGN_ICONS.justify, tip: "Justify" },
                    ],
                },
                {
                    prop: "justify-content", label: "Vertical", kind: "segment", full: true,
                    options: [
                        { value: "flex-start", icon: VALIGN_ICONS.top, tip: "Top" },
                        { value: "center", icon: VALIGN_ICONS.middle, tip: "Middle" },
                        { value: "flex-end", icon: VALIGN_ICONS.bottom, tip: "Bottom" },
                    ],
                },
                { prop: "text-style", label: "Style", kind: "text-style", full: true, readonly: true },
                { prop: "color", label: "Color", kind: "color", full: true },
            ],
        },
        // Custom sections — collapsible chrome wrapping pre-existing DOM (the
        // Custom CSS form and the Animations panel) rather than field rows.
        { id: "flexbox", label: "Flexbox", appliesTo: ["group"], custom: "group-flex-section" },
        { id: "custom-css", label: "Custom CSS", appliesTo: ALL_TYPES, custom: "inspector-custom" },
        { id: "animations", label: "Animations", appliesTo: ALL_TYPES, custom: "animations-section" },
    ];

    // Cache of input elements keyed by property name so refreshInspector
    // can fill them in O(1) and the change handlers can be wired once.
    const inspectorInputs = {};
    // Set of properties that the current pending PropertyChanged round
    // trip is waiting on. Used to suppress refresh-from-DOM clobbering
    // the user's in-flight typing.
    const inspectorPending = new Set();
    // Composite text-style (B/I/U/S) controls. They span multiple CSS props
    // so they are not keyed by a single prop in inspectorInputs; populate
    // re-syncs each from the selected element's declarations.
    const textStyleControls = [];

    // Composite Fill/Border/Shadow controls (swatch+opacity, 4-cell clusters,
    // shadow). Like text-style they wire their own commits and re-sync from the
    // declaration map via `.syncDecls(decls)` rather than the single-prop
    // populate path. Reset alongside textStyleControls in buildInspectorSections.
    const compositeControls = [];

    // Transform Width/Height aspect-ratio lock. Ephemeral UI state (default
    // off); when on, editing one dimension scales the other by the element's
    // live ratio (see onInspectorFieldCommit).
    let sizeRatioLinked = false;

    // Installed font families delivered by the Rust FontList message. The
    // font-family combobox reads this live, so a list arriving after the
    // inspector is built needs no rebuild.
    let availableFonts = [];

    // Border style segmented options. "None" shows as a word; the three line
    // styles render as a short stroked line preview (mirrors the mockup).
    function borderLine(dash) {
        const da = dash ? ' stroke-dasharray="' + dash + '"' : "";
        const cap = dash === "2 4" ? ' stroke-linecap="round"' : "";
        return '<svg width="26" height="2" viewBox="0 0 26 2"><line x1="1" y1="1"'
            + ' x2="25" y2="1" stroke="currentColor" stroke-width="2"' + da + cap + "/></svg>";
    }
    const BORDER_STYLE_OPTIONS = [
        { value: "none", icon: "None", tip: "No border" },
        { value: "solid", icon: borderLine(""), tip: "Solid" },
        { value: "dashed", icon: borderLine("5 4"), tip: "Dashed" },
        { value: "dotted", icon: borderLine("2 4"), tip: "Dotted" },
    ];

    // Object-fit segmented options for a fill image. Values are the
    // background-size each fit maps to ("fit" = natural size, see design spec).
    const OBJECT_FIT_OPTIONS = [
        { value: "100% 100%", icon: "Fill", tip: "Stretch to fill" },
        { value: "cover", icon: "Cover", tip: "Cover the box" },
        { value: "contain", icon: "Contain", tip: "Fit inside the box" },
        { value: "auto", icon: "Fit", tip: "Natural size" },
    ];

    // Cluster cell specs: the four longhand props (in cell order) plus the short
    // label each cell shows. `parse` reads the live values off a decl map.
    // Length units offered by the inspector unit chips (px is the default).
    const UNITS = ["px", "em", "rem", "pt", "in", "pc", "cm", "mm"];

    const CLUSTER_SPECS = {
        width: {
            cells: [
                { prop: "border-top-width", label: "T", tip: "Top" },
                { prop: "border-right-width", label: "R", tip: "Right" },
                { prop: "border-bottom-width", label: "B", tip: "Bottom" },
                { prop: "border-left-width", label: "L", tip: "Left" },
            ],
            parse: function (decls) {
                const w = window.__style.parseBorder(decls).widths;
                return [w.t, w.r, w.b, w.l];
            },
        },
        radius: {
            cells: [
                { prop: "border-top-left-radius", label: "TL", tip: "Top-left" },
                { prop: "border-top-right-radius", label: "TR", tip: "Top-right" },
                { prop: "border-bottom-right-radius", label: "BR", tip: "Bottom-right" },
                { prop: "border-bottom-left-radius", label: "BL", tip: "Bottom-left" },
            ],
            parse: function (decls) {
                const r = window.__style.parseRadius(decls);
                return [r.tl, r.tr, r.br, r.bl];
            },
        },
    };

    // The B/I/U/S toggle specs. `list` props (text-decoration) add/remove
    // their token within a space-separated list; `min` (font-weight) treats
    // any weight >= the threshold as "on".
    const TEXT_STYLE_BUTTONS = [
        { prop: "font-weight", on: "700", min: 600, glyph: "B", cls: "b", tip: "Bold" },
        { prop: "font-style", on: "italic", glyph: "I", cls: "i", tip: "Italic" },
        { prop: "text-decoration", on: "underline", list: true, glyph: "U", cls: "u", tip: "Underline" },
        { prop: "text-decoration", on: "line-through", list: true, glyph: "S", cls: "s", tip: "Strikethrough" },
    ];

    // Properties already surfaced by structured inspector fields (or set
    // structurally). Everything else on the element shows in the Custom CSS
    // declarations list.
    const KNOWN_PROPS = {
        "position": 1, "display": 1, "left": 1, "top": 1, "right": 1, "bottom": 1,
        "width": 1, "height": 1, "transform": 1, "opacity": 1, "z-index": 1,
        "background-color": 1, "border": 1, "border-radius": 1, "box-shadow": 1,
        "background-image": 1, "background-size": 1, "background-repeat": 1,
        "background-position": 1,
        "border-style": 1, "border-color": 1, "border-width": 1,
        "border-top-width": 1, "border-right-width": 1,
        "border-bottom-width": 1, "border-left-width": 1,
        "border-top-left-radius": 1, "border-top-right-radius": 1,
        "border-bottom-right-radius": 1, "border-bottom-left-radius": 1,
        "font-family": 1, "font-size": 1, "font-weight": 1, "color": 1,
        "text-align": 1, "justify-content": 1, "line-height": 1,
        "letter-spacing": 1, "font-style": 1, "text-decoration": 1,
        // Structural invariant, not user-editable junk (see WYSIWYG principle).
        "white-space": 1,
    };

    // buildInspectorSections
    // Inputs: none (reads INSPECTOR_SECTIONS).
    // Output: side-effect; populates #inspector-scroll with one
    // <section> per group, each with a collapsable header and a body of
    // <input> fields. Wires the change handlers so every commit posts
    // PropertyChanged.
    function buildInspectorSections() {
        const root = document.getElementById("inspector-scroll");
        if (!root) {
            return;
        }
        root.replaceChildren();
        textStyleControls.length = 0;
        compositeControls.length = 0;
        for (let i = 0; i < INSPECTOR_SECTIONS.length; i++) {
            const section = INSPECTOR_SECTIONS[i];
            root.appendChild(buildSection(section));
        }
        const form = document.getElementById("inspector-custom");
        if (form && !form.dataset.wired) {
            form.dataset.wired = "1";
            form.addEventListener("submit", onCustomCssSubmit);
        }
    }

    // buildSection
    // Inputs: a section definition.
    // Output: a <section> DOM node with header + body, fully wired.
    function buildSection(def) {
        const sec = document.createElement("section");
        sec.className = "inspector__section";
        sec.dataset.sectionId = def.id;

        const header = document.createElement("button");
        header.type = "button";
        header.className = "inspector__section-header";
        header.textContent = def.label;
        const chev = document.createElement("span");
        chev.className = "inspector__chevron";
        chev.setAttribute("aria-hidden", "true");
        header.appendChild(chev);
        header.addEventListener("click", function () {
            const collapsed = sec.dataset.collapsed === "true";
            sec.dataset.collapsed = collapsed ? "false" : "true";
        });
        sec.appendChild(header);

        const body = document.createElement("div");
        body.className = "inspector__section-body";
        if (def.custom) {
            // Relocate the pre-built node (Custom CSS form / Animations panel)
            // into this section's body; flow layout, not the field grid.
            body.classList.add("inspector__section-body--flow");
            const node = document.getElementById(def.custom);
            if (node) {
                body.appendChild(node);
            }
        } else {
            for (let i = 0; i < def.fields.length; i++) {
                body.appendChild(buildField(def.fields[i]));
            }
        }
        sec.appendChild(body);
        return sec;
    }

    // buildField
    // Inputs: a field definition.
    // Output: a labelled control (text input, number input, color swatch, or
    // select) registered in inspectorInputs and wired with the change handler.
    function buildField(field) {
        const wrap = document.createElement("div");
        wrap.className = "inspector__field";
        if (field.full) {
            wrap.classList.add("inspector__field--full");
        }
        const label = document.createElement("label");
        label.className = "inspector__field-label";
        label.textContent = field.label + (field.suffix ? " (" + field.suffix.trim() + ")" : "");
        const control = buildFieldControl(field);
        control.dataset.prop = field.prop;
        control.dataset.kind = field.kind;
        // noLabel fields carry their own per-input labels (e.g. the shadow grid),
        // so suppress the redundant row label.
        if (field.noLabel) {
            const id = "inspector-input-" + field.prop.replace(/[^a-z0-9]/gi, "-");
            control.id = id;
            inspectorInputs[field.prop] = control;
            wrap.appendChild(control);
            return wrap;
        }
        // CSS number fields carry a unit (e.g. "px") the bare value must be
        // suffixed with on commit; geometry number fields (x/y/w/h) have no
        // unit and stay bare floats for the Rust geometry path.
        if (field.unit) {
            control.dataset.unit = field.unit;
        }
        // Composite controls (swatch/cluster/shadow/border-style/size-row) wire
        // their own commits internally, so skip the single-prop change handler
        // that would double-post.
        if (!field.readonly && !field.composite) {
            control.addEventListener("change", onInspectorFieldCommit);
        }
        const id = "inspector-input-" + field.prop.replace(/[^a-z0-9]/gi, "-");
        control.id = id;
        label.setAttribute("for", id);
        wrap.appendChild(label);
        wrap.appendChild(control);
        inspectorInputs[field.prop] = control;
        return wrap;
    }

    // UNIT_CHEVRON: the small down-caret drawn inside a unit chip.
    const UNIT_CHEVRON = '<svg width="9" height="9" viewBox="0 0 24 24" fill="none"'
        + ' stroke="currentColor" stroke-width="3" stroke-linecap="round"'
        + ' stroke-linejoin="round"><path d="M6 9l6 6 6-6"/></svg>';

    // makeDropdown
    // Inputs: { label, options:[{value,label}], value, placeholder, variant,
    //   className, onChange }. Output: a trigger element (button for "field",
    //   span chip for "chip") styled like the Add-Animation menu. It owns a
    //   fixed-position popover (single header = `label`, then the options) built
    //   lazily on open; click-off / Esc close it. Exposes `.value` get/set,
    //   `.setOptions()`, and fires `change` on selection (drop-in for a native
    //   <select>). See docs/… unified-dropdown spec.
    function makeDropdown(opts) {
        const variant = opts.variant === "chip" ? "chip" : "field";
        const label = opts.label || "";
        let placeholder = opts.placeholder || "";
        let options = (opts.options || []).slice();
        let value = opts.value == null ? "" : String(opts.value);
        const trigger = document.createElement(variant === "chip" ? "span" : "button");
        if (variant === "chip") {
            trigger.className = "inspector__unitchip tt";
            trigger.setAttribute("data-tip", label);
            trigger.setAttribute("data-key", "");
        } else {
            trigger.type = "button";
            trigger.className = "inspector__dropdown";
        }
        if (opts.className) { trigger.classList.add(opts.className); }
        const lab = document.createElement("span");
        lab.className = variant === "chip"
            ? "inspector__unitchip-label" : "inspector__dropdown-label";
        const caret = document.createElement("span");
        caret.className = variant === "chip"
            ? "inspector__unitchip-caret" : "inspector__dropdown-caret";
        caret.innerHTML = UNIT_CHEVRON;
        trigger.appendChild(lab);
        trigger.appendChild(caret);
        const menu = document.createElement("div");
        menu.className = "dropdown-menu";

        function labelFor(v) {
            for (let i = 0; i < options.length; i++) {
                if (options[i].value === v) { return options[i].label; }
            }
            return "";
        }
        function relabel() {
            const t = labelFor(value);
            lab.textContent = t !== "" ? t : placeholder;
        }
        function buildMenu() {
            menu.replaceChildren();
            const h = document.createElement("div");
            h.className = "anim-menu__cat";
            h.textContent = label;
            menu.appendChild(h);
            for (let i = 0; i < options.length; i++) {
                const o = options[i];
                const b = document.createElement("button");
                b.type = "button";
                b.className = "anim-menu__item";
                b.textContent = o.label;
                b.setAttribute("aria-selected", o.value === value ? "true" : "false");
                (function (v) {
                    b.addEventListener("click", function () { select(v); close(); });
                }(o.value));
                menu.appendChild(b);
            }
        }
        function select(v) {
            value = String(v);
            relabel();
            if (opts.onChange) { opts.onChange(value); }
            trigger.dispatchEvent(new Event("change"));
        }
        function open() {
            buildMenu();
            document.body.appendChild(menu);
            menu.style.minWidth = trigger.getBoundingClientRect().width + "px";
            menu.classList.add("dropdown-menu--open");
            positionColorPopover(menu, trigger);
            document.addEventListener("pointerdown", onOutside, true);
            document.addEventListener("keydown", onEsc, true);
        }
        function close() {
            menu.classList.remove("dropdown-menu--open");
            menu.remove();
            document.removeEventListener("pointerdown", onOutside, true);
            document.removeEventListener("keydown", onEsc, true);
        }
        function onOutside(e) {
            if (!menu.contains(e.target) && !trigger.contains(e.target)) { close(); }
        }
        function onEsc(e) {
            if (e.key === "Escape") { e.preventDefault(); close(); }
        }
        trigger.addEventListener("click", function (e) {
            e.stopPropagation();
            if (menu.classList.contains("dropdown-menu--open")) { close(); } else { open(); }
        });
        Object.defineProperty(trigger, "value", {
            get: function () { return value; },
            set: function (v) { value = v == null ? "" : String(v); relabel(); },
        });
        trigger.setOptions = function (newOptions) {
            options = (newOptions || []).slice();
            relabel();
        };
        trigger.setPlaceholder = function (p) {
            placeholder = p || "";
            relabel();
        };
        relabel();
        return trigger;
    }

    // makeUnitChip
    // Inputs: getUnit() → current unit string, setUnit(u) → commit a new unit.
    // Output: the "px ▾" chip (a chip-variant dropdown of UNITS). `.sync()`
    // relabels from getUnit(). Errors: asserts both callbacks are functions.
    function makeUnitChip(getUnit, setUnit) {
        console.assert(typeof getUnit === "function" && typeof setUnit === "function",
            "unit chip needs get/set");
        const chip = makeDropdown({
            label: "Unit",
            variant: "chip",
            options: UNITS.map(function (u) { return { value: u, label: u }; }),
            value: getUnit() || "px",
            onChange: function (u) { setUnit(u); },
        });
        chip.sync = function () { chip.value = getUnit() || "px"; };
        return chip;
    }

    // makeUnitNumberControl
    // Inputs: a "number" field definition carrying unitSelect.
    // Output: a box holding a numeric <input> plus a unit chip. Exposes `.value`
    // (the bare number, proxying the input) and a live `dataset.unit`; fires
    // `change` on number commit (Enter/blur) and on unit change. Reinterprets on
    // unit switch (keeps the number). `setUnit()` sets the unit without a commit
    // (used by populate). The normal buildField wiring appends dataset.unit.
    function makeUnitNumberControl(field) {
        const box = document.createElement("div");
        box.className = "inspector__unitfield";
        let unit = field.unit || "px";
        box.dataset.unit = unit;
        const input = document.createElement("input");
        input.className = "inspector__input";
        input.spellcheck = false;
        const chip = makeUnitChip(function () { return unit; }, function (u) {
            unit = u;
            box.dataset.unit = u;
            box.dispatchEvent(new Event("change"));
        });
        input.addEventListener("change", function () { box.dispatchEvent(new Event("change")); });
        input.addEventListener("keydown", function (e) {
            if (e.key === "Enter") { e.preventDefault(); input.blur(); }
        });
        box.appendChild(input);
        box.appendChild(chip);
        box.setUnit = function (u) {
            unit = u || "px";
            box.dataset.unit = unit;
            chip.sync();
        };
        Object.defineProperty(box, "value", {
            get: function () { return input.value; },
            set: function (v) { input.value = v; },
        });
        return box;
    }

    // buildFieldControl
    // Inputs: a field definition.
    // Output: the bare control element for the field's kind — a <select> for
    // "select", a color swatch for "color", otherwise a text <input> (the
    // Enter-to-blur affordance is wired for text inputs only).
    function buildFieldControl(field) {
        if (field.kind === "number" && field.unitSelect) {
            return makeUnitNumberControl(field);
        }
        if (field.kind === "segment") {
            return makeSegmentControl(field.options || []);
        }
        if (field.kind === "text-style") {
            return makeTextStyleControl();
        }
        if (field.kind === "color") {
            return makeColorControl();
        }
        if (field.kind === "swatch") {
            return makeSwatchOpacityControl(field.prop);
        }
        if (field.kind === "border-style") {
            return makeBorderStyleControl();
        }
        if (field.kind === "cluster") {
            return makeClusterControl(CLUSTER_SPECS[field.cluster]);
        }
        if (field.kind === "shadow") {
            return makeShadowControl();
        }
        if (field.kind === "size-row") {
            return makeSizeRowControl();
        }
        if (field.kind === "fill-image") {
            return makeFillImageControl();
        }
        if (field.kind === "object-fit") {
            return makeObjectFitControl();
        }
        if (field.kind === "font-combo") {
            return makeFontComboControl();
        }
        if (field.kind === "presets") {
            return makePresetsControl();
        }
        const input = document.createElement("input");
        input.className = "inspector__input";
        input.spellcheck = false;
        if (field.readonly) {
            input.readOnly = true;
            input.tabIndex = -1;
        } else {
            input.addEventListener("keydown", function (e) {
                if (e.key === "Enter") {
                    e.preventDefault();
                    input.blur();
                }
            });
        }
        return input;
    }

    // makeSegmentControl
    // Inputs: an array of { value, icon, tip } options.
    // Output: a segmented button row that behaves like a form control: it
    // exposes a synthetic `.value` (the selected option) and dispatches a
    // `change` event on click, so it reuses the existing commit + populate
    // plumbing unchanged. Setting `.value` reflects the pressed button.
    function makeSegmentControl(options) {
        console.assert(Array.isArray(options), "segment options must be array");
        const box = document.createElement("div");
        box.className = "inspector__segment";
        box.setAttribute("role", "group");
        let current = "";
        for (let i = 0; i < options.length; i++) {
            const opt = options[i];
            const b = document.createElement("button");
            b.type = "button";
            b.className = "inspector__segment-btn tt";
            b.dataset.value = opt.value;
            b.setAttribute("aria-pressed", "false");
            b.setAttribute("data-tip", opt.tip || "");
            b.setAttribute("data-key", "");
            b.innerHTML = opt.icon || "";
            b.addEventListener("click", function () {
                box.value = opt.value;
                box.dispatchEvent(new Event("change"));
            });
            box.appendChild(b);
        }
        Object.defineProperty(box, "value", {
            get: function () { return current; },
            set: function (v) {
                current = (v === null || v === undefined) ? "" : String(v);
                for (let i = 0; i < box.children.length; i++) {
                    const on = box.children[i].dataset.value === current;
                    box.children[i].setAttribute("aria-pressed", on ? "true" : "false");
                }
            },
        });
        return box;
    }

    // makeColorSlider
    // Inputs: label text, max value, and onInput / onCommit callbacks.
    // Output: { row, input, readout } — a labelled range slider row used for
    // the H/S/L/A axes of the colour popover. `input` fires onInput live and
    // onCommit on release (commit-on-change, preview-on-input).
    function makeColorSlider(label, max, onInput, onCommit) {
        const row = document.createElement("div");
        row.className = "colorpop__slider";
        const lab = document.createElement("span");
        lab.className = "colorpop__slider-label";
        lab.textContent = label;
        const input = document.createElement("input");
        input.type = "range";
        input.min = "0";
        input.max = String(max);
        input.step = "1";
        const readout = document.createElement("span");
        readout.className = "colorpop__slider-val";
        input.addEventListener("input", onInput);
        input.addEventListener("change", onCommit);
        row.appendChild(lab);
        row.appendChild(input);
        row.appendChild(readout);
        return { row: row, input: input, readout: readout };
    }

    // positionColorPopover
    // Inputs: the popover element and its anchor (the inline swatch). Output:
    // side-effect; pins the popover (position:fixed) just below the anchor,
    // clamped into the viewport so it never spills off-screen.
    function positionColorPopover(pop, anchor) {
        const r = anchor.getBoundingClientRect();
        const pw = pop.offsetWidth || 240;
        const ph = pop.offsetHeight || 300;
        let left = r.left;
        let top = r.bottom + 8;
        left = Math.max(8, Math.min(left, window.innerWidth - pw - 8));
        let above = false;
        if (top + ph > window.innerHeight - 8) {
            top = Math.max(8, r.top - ph - 8);
            above = true;
        }
        pop.style.left = left + "px";
        pop.style.top = top + "px";
        pop.classList.toggle("colorpop--above", above);
        const cx = r.left + r.width / 2 - left;
        pop.style.setProperty("--arrow-x", Math.max(10, Math.min(cx, pw - 10)) + "px");
    }

    // makeColorControl
    // Output: an inline swatch trigger whose `.value` round-trips a full
    // hex-or-rgba() colour string and which dispatches `change` on commit
    // (unchanged public contract). Clicking the swatch opens a popover with an
    // HS wheel and H/S/L/A sliders; HSL is the edit model, rgba() the stored
    // value. See docs/superpowers/specs/2026-06-27-color-picker-design.md.
    function makeColorControl() {
        const box = document.createElement("div");
        box.className = "inspector__color";
        const swatch = document.createElement("button");
        swatch.type = "button";
        swatch.className = "inspector__color-swatch";
        const fill = document.createElement("span");
        fill.className = "inspector__color-fill";
        swatch.appendChild(fill);
        const hexInput = document.createElement("input");
        hexInput.className = "inspector__color-hex";
        hexInput.spellcheck = false;
        // Growing gap between the hex text and the percentage: clicking here
        // opens the picker (only the hex text itself edits).
        const gap = document.createElement("span");
        gap.className = "inspector__color-gap";
        const pct = document.createElement("span");
        pct.className = "inspector__color-pct";
        pct.textContent = "100%";
        box.appendChild(swatch);
        box.appendChild(hexInput);
        box.appendChild(gap);
        box.appendChild(pct);

        const state = { h: 0, s: 0, l: 0, a: 100, none: false };
        const pop = buildColorPopover(state, render, commit, setNone);
        const alphaPop = buildAlphaPopover(state, render, commit);
        document.body.appendChild(pop.el);
        document.body.appendChild(alphaPop.el);

        function stateHex() {
            const rgb = window.__style.hslToRgb(state.h, state.s, state.l);
            return window.__style.rgbToHex(rgb.r, rgb.g, rgb.b);
        }
        // render: repaint the inline row + popovers from state. No commit
        // (callers commit explicitly). Skips inputs the user is editing.
        function render() {
            const hex = stateHex();
            const css = window.__style.composeRgba(hex, state.a);
            if (state.none) {
                box.classList.add("inspector__color--none");
                fill.style.background = "";
                if (document.activeElement !== hexInput) { hexInput.value = "None"; }
                pct.textContent = "";
            } else {
                box.classList.remove("inspector__color--none");
                fill.style.background = css;
                if (document.activeElement !== hexInput) {
                    hexInput.value = hex.toUpperCase();
                }
                pct.textContent = Math.round(state.a) + "%";
            }
            hexInput.size = Math.max(hexInput.value.length, 1);
            pop.render(state, hex, css);
            alphaPop.render(state, hex);
        }
        function commit() {
            box.dispatchEvent(new Event("change"));
        }
        function setNone() {
            state.none = true;
            closePop();
            render();
            commit();
        }
        // Apply a parsed { hex, alpha } to state, keeping hue on achromatic
        // colours so the wheel doesn't snap to red on grey/black/white.
        function applyColor(hex, alpha, setAlpha) {
            const rgb = window.__style.hexToRgb(hex);
            const hsl = window.__style.rgbToHsl(rgb.r, rgb.g, rgb.b);
            if (hsl.s > 0) { state.h = hsl.h; }
            state.s = hsl.s;
            state.l = hsl.l;
            if (setAlpha) { state.a = alpha; }
            state.none = false;
        }
        function currentCss() {
            if (state.none) { return ""; }
            return window.__style.composeRgba(stateHex(), state.a);
        }

        function openPop() {
            closeAlpha();
            positionColorPopover(pop.el, swatch);
            pop.el.classList.add("colorpop--open");
            render();
            document.addEventListener("pointerdown", onOutside, true);
            document.addEventListener("keydown", onEsc, true);
        }
        function closePop() {
            pop.el.classList.remove("colorpop--open");
            document.removeEventListener("pointerdown", onOutside, true);
            document.removeEventListener("keydown", onEsc, true);
        }
        function onOutside(e) {
            if (!pop.el.contains(e.target) && e.target !== swatch
                    && !swatch.contains(e.target) && e.target !== gap) {
                closePop();
            }
        }
        function onEsc(e) {
            if (e.key === "Escape") { e.preventDefault(); closePop(); closeAlpha(); }
        }
        function openAlpha() {
            if (state.none) { return; }
            closePop();
            positionColorPopover(alphaPop.el, pct);
            alphaPop.el.classList.add("colorpop--open");
            render();
            document.addEventListener("pointerdown", onAlphaOutside, true);
            document.addEventListener("keydown", onEsc, true);
        }
        function closeAlpha() {
            alphaPop.el.classList.remove("colorpop--open");
            document.removeEventListener("pointerdown", onAlphaOutside, true);
        }
        function onAlphaOutside(e) {
            if (!alphaPop.el.contains(e.target) && e.target !== pct) {
                closeAlpha();
            }
        }

        // Zone 1: swatch (and the gap after the hex text) → colour popover.
        function togglePop(e) {
            e.stopPropagation();
            if (pop.el.classList.contains("colorpop--open")) { closePop(); } else { openPop(); }
        }
        swatch.addEventListener("click", togglePop);
        gap.addEventListener("click", togglePop);
        // Zone 3: percentage → opacity popover.
        pct.addEventListener("click", function (e) {
            e.stopPropagation();
            if (alphaPop.el.classList.contains("colorpop--open")) { closeAlpha(); } else { openAlpha(); }
        });
        // Zone 2: hex text → inline edit. 8-digit hex sets colour + alpha;
        // "none"/empty clears; invalid reverts on blur.
        hexInput.addEventListener("change", function () {
            const raw = hexInput.value.trim().toLowerCase();
            if (raw === "" || raw === "none") { setNone(); return; }
            const m8 = /^#?([0-9a-f]{6})([0-9a-f]{2})$/.exec(raw);
            if (m8) {
                const alpha = Math.round(parseInt(m8[2], 16) / 255 * 100);
                applyColor("#" + m8[1], alpha, true);
                render();
                commit();
                return;
            }
            const hexed = raw[0] === "#" ? raw : "#" + raw;
            if (!/^#([0-9a-f]{3}|[0-9a-f]{6})$/.test(hexed)) {
                render();
                return;
            }
            const parsed = window.__style.parseRgba(hexed);
            applyColor(parsed.hex, parsed.alpha, false);
            render();
            commit();
        });
        hexInput.addEventListener("keydown", function (e) {
            if (e.key === "Enter") { e.preventDefault(); hexInput.blur(); }
        });

        Object.defineProperty(box, "value", {
            get: currentCss,
            set: function (v) {
                // While a popover is open the user is editing — ignore the echo
                // from the committed round-trip so it can't reset live edits.
                if (pop.el.classList.contains("colorpop--open")
                    || alphaPop.el.classList.contains("colorpop--open")) {
                    return;
                }
                const s = String(v == null ? "" : v).trim().toLowerCase();
                if (s === "" || s === "none" || s === "transparent") {
                    state.none = true;
                    render();
                    return;
                }
                const parsed = window.__style.parseRgba(v);
                applyColor(parsed.hex, parsed.alpha, true);
                render();
            },
        });
        render();
        return box;
    }

    // buildColorPopover
    // Inputs: shared `state` ({h,s,l,a}), a render() that repaints from state,
    // and a commit() that fires the control's `change`. Output: { el, render }
    // — the popover DOM (an HS wheel + H/S/L/A sliders + hex field) wired to
    // mutate state then render/commit. `render(state, hex, css)` syncs the
    // popover's own controls (called by the parent's render).
    function buildColorPopover(state, render, commit, setNone) {
        const el = document.createElement("div");
        el.className = "colorpop";
        const noneBtn = document.createElement("button");
        noneBtn.type = "button";
        noneBtn.className = "colorpop__none";
        noneBtn.textContent = "None";
        noneBtn.addEventListener("click", function () { setNone(); });
        el.appendChild(noneBtn);
        const wheel = document.createElement("div");
        wheel.className = "colorpop__wheel";
        // Lightness wash over the hue/sat disc: white above L=50, black below,
        // opacity tracking distance from 50 so the wheel reads at the actual
        // lightness (WYSIWYG). Sits under the dot.
        const lum = document.createElement("span");
        lum.className = "colorpop__wheel-lum";
        const dot = document.createElement("span");
        dot.className = "colorpop__dot";
        wheel.appendChild(lum);
        wheel.appendChild(dot);
        el.appendChild(wheel);
        wireColorWheel(wheel, state, render, commit);

        function onAxis(key, scale) {
            return function (e) {
                state[key] = Number(e.target.value) * scale;
                state.none = false;
                render();
            };
        }
        const h = makeColorSlider("H", 360, onAxis("h", 1), commit);
        const s = makeColorSlider("S", 100, onAxis("s", 1), commit);
        const l = makeColorSlider("L", 100, onAxis("l", 1), commit);
        const a = makeColorSlider("A", 100, onAxis("a", 1), commit);
        el.appendChild(h.row);
        el.appendChild(s.row);
        el.appendChild(l.row);
        el.appendChild(a.row);

        const hexRow = document.createElement("div");
        hexRow.className = "colorpop__hexrow";
        const hexInput = document.createElement("input");
        hexInput.className = "colorpop__hex";
        hexInput.spellcheck = false;
        hexRow.appendChild(hexInput);
        el.appendChild(hexRow);
        hexInput.addEventListener("change", function () {
            const parsed = window.__style.parseRgba(hexInput.value);
            const rgb = window.__style.hexToRgb(parsed.hex);
            const hsl = window.__style.rgbToHsl(rgb.r, rgb.g, rgb.b);
            state.h = hsl.h;
            state.s = hsl.s;
            state.l = hsl.l;
            state.none = false;
            render();
            commit();
        });
        hexInput.addEventListener("keydown", function (e) {
            if (e.key === "Enter") { e.preventDefault(); hexInput.blur(); }
        });

        function renderPopover(st, hex, css) {
            setColorAxis(h, st.h, Math.round(st.h));
            setColorAxis(s, st.s, Math.round(st.s));
            setColorAxis(l, st.l, Math.round(st.l));
            setColorAxis(a, st.a, Math.round(st.a));
            h.input.style.background = "linear-gradient(to right,"
                + " #f00, #ff0, #0f0, #0ff, #00f, #f0f, #f00)";
            s.input.style.background = "linear-gradient(to right, hsl("
                + st.h + ", 0%, 50%), hsl(" + st.h + ", 100%, 50%))";
            l.input.style.background = "linear-gradient(to right, #000, hsl("
                + st.h + ", " + st.s + "%, 50%), #fff)";
            a.input.style.background = "linear-gradient(to right,"
                + " transparent, " + hex + ")";
            lum.style.background = st.l >= 50 ? "#fff" : "#000";
            lum.style.opacity = String(Math.abs(st.l - 50) / 50);
            const R = wheel.offsetWidth / 2 || 100;
            const rad = (st.s / 100) * R;
            const theta = st.h * Math.PI / 180;
            dot.style.left = (R + rad * Math.sin(theta)) + "px";
            dot.style.top = (R - rad * Math.cos(theta)) + "px";
            dot.style.background = css;
            if (document.activeElement !== hexInput) {
                hexInput.value = hex.toUpperCase();
            }
        }
        return { el: el, render: renderPopover };
    }

    // buildAlphaPopover
    // Inputs: shared `state`, render(), commit(). Output: { el, render } — a
    // small popover holding one 0..100 alpha slider + readout. Drag mutates
    // state.a then render()s live; release commits. Opened from the row's
    // percentage span; positioned by positionColorPopover.
    function buildAlphaPopover(state, render, commit) {
        const el = document.createElement("div");
        el.className = "colorpop colorpop--alpha";
        const slider = makeColorSlider("A", 100, function (e) {
            state.a = Number(e.target.value);
            render();
        }, commit);
        el.appendChild(slider.row);
        function renderAlpha(st, hex) {
            setColorAxis(slider, st.a, Math.round(st.a));
            slider.input.style.background = "linear-gradient(to right,"
                + " transparent, " + hex + ")";
        }
        return { el: el, render: renderAlpha };
    }

    // setColorAxis: set a slider's value (unless the user is dragging it) and
    // its numeric readout. Keeps live drags from being clobbered by render.
    function setColorAxis(slider, value, shown) {
        if (document.activeElement !== slider.input) {
            slider.input.value = String(Math.round(value));
        }
        slider.readout.textContent = String(shown);
    }

    // wireColorWheel
    // Inputs: the wheel element, shared state, render(), commit(). Output:
    // side-effect; pointer drag on the wheel sets hue (angle) + saturation
    // (radius), rendering live and committing on release. Angle is measured
    // clockwise from top to match the conic hue gradient.
    function wireColorWheel(wheel, state, render, commit) {
        function fromPointer(e) {
            const r = wheel.getBoundingClientRect();
            const R = r.width / 2;
            const dx = e.clientX - r.left - R;
            const dy = e.clientY - r.top - R;
            const dist = Math.min(Math.sqrt(dx * dx + dy * dy), R);
            let deg = Math.atan2(dx, -dy) * 180 / Math.PI;
            if (deg < 0) { deg += 360; }
            state.h = deg;
            state.s = R > 0 ? (dist / R) * 100 : 0;
            state.none = false;
            render();
        }
        function onMove(e) { fromPointer(e); }
        function onUp() {
            document.removeEventListener("pointermove", onMove, true);
            document.removeEventListener("pointerup", onUp, true);
            commit();
        }
        wheel.addEventListener("pointerdown", function (e) {
            e.preventDefault();
            fromPointer(e);
            document.addEventListener("pointermove", onMove, true);
            document.addEventListener("pointerup", onUp, true);
        });
    }

    // Chain glyph shared by the cluster link toggle and the ratio lock.
    const LINK_ICON = '<svg width="12" height="12" viewBox="0 0 24 24" fill="none"'
        + ' stroke="currentColor" stroke-width="2" stroke-linecap="round"'
        + ' stroke-linejoin="round"><path d="M10 13a4 4 0 0 0 5.6.5l2.5-2.5a4 4 0 0'
        + ' 0-5.6-5.6L11 7"/><path d="M14 11a4 4 0 0 0-5.6-.5L5.9 13a4 4 0 0 0 5.6'
        + ' 5.6L13 17"/></svg>';

    // numOr0: coerce an inspector input string to a finite number (0 fallback).
    function numOr0(v) {
        const n = Number(String(v == null ? "" : v).replace(/px$/i, "").trim());
        return isFinite(n) ? n : 0;
    }
    function setIfIdle(input, value) {
        if (input && document.activeElement !== input) {
            input.value = value;
        }
    }

    // makeSwatchOpacityControl
    // Inputs: the CSS property the control owns (background-color / border-color).
    // Output: a composite control wrapping the colour picker — alpha now lives
    // inside the picker, so the standalone opacity field is gone. Commits the
    // picker's full hex/rgba() value on change and re-syncs via syncDecls.
    // Registered in compositeControls. Errors: asserts a non-empty prop.
    function makeSwatchOpacityControl(prop) {
        console.assert(typeof prop === "string" && prop !== "", "swatch prop required");
        const box = document.createElement("div");
        box.className = "inspector__swatchrow";
        const color = makeColorControl();
        color.classList.add("inspector__swatchrow-color");
        box.appendChild(color);
        color.addEventListener("change", function () {
            sendPropertyChanged(prop, color.value);
        });
        box.syncDecls = function (decls) {
            color.value = decls[prop] || "";
        };
        Object.defineProperty(box, "value", { get: function () { return ""; }, set: function () {} });
        compositeControls.push(box);
        return box;
    }

    // makeBorderStyleControl
    // Output: a segmented None/solid/dashed/dotted selector that commits
    // `border-style` and re-syncs from parseBorder (so a legacy `border`
    // shorthand still lights the right segment). Registered in compositeControls.
    function makeBorderStyleControl() {
        const box = makeSegmentControl(BORDER_STYLE_OPTIONS);
        box.addEventListener("change", function () {
            sendPropertyChanged("border-style", box.value);
        });
        box.syncDecls = function (decls) {
            box.value = window.__style.parseBorder(decls).style;
        };
        compositeControls.push(box);
        return box;
    }

    // makeClusterCell
    // Inputs: a { label, tip } cell spec. Output: { wrap, input } — a labelled
    // mini number field used by the border-width / corner-radius clusters and
    // the shadow grid.
    function makeClusterCell(cell) {
        const wrap = document.createElement("div");
        wrap.className = "inspector__cluster-cell tt";
        wrap.setAttribute("data-tip", cell.tip || "");
        wrap.setAttribute("data-key", "");
        const lab = document.createElement("span");
        lab.className = "inspector__cluster-cell-label";
        lab.textContent = cell.label;
        const input = document.createElement("input");
        input.className = "inspector__cluster-input";
        input.spellcheck = false;
        wrap.appendChild(lab);
        wrap.appendChild(input);
        return { wrap: wrap, input: input };
    }

    // makeShadowCell
    // Inputs: a label string. Output: { wrap, input } — a number field with its
    // label stacked ABOVE the input (vs. the cluster cell's inline single-char
    // label), so multi-char names like "Blur"/"Spread" don't crowd the value.
    function makeShadowCell(label) {
        const wrap = document.createElement("div");
        wrap.className = "inspector__shadow-cell";
        const lab = document.createElement("span");
        lab.className = "inspector__shadow-cell-label";
        lab.textContent = label;
        const input = document.createElement("input");
        input.className = "inspector__cluster-input";
        input.spellcheck = false;
        wrap.appendChild(lab);
        wrap.appendChild(input);
        return { wrap: wrap, input: input };
    }

    // makeClusterControl
    // Inputs: a CLUSTER_SPECS entry (four longhand props + a parse fn).
    // Output: a 4-cell number cluster with a link toggle. Linked edits write
    // the value to all four longhands; unlinked edits write only the touched
    // cell. The link auto-reflects uniformity on sync; clicking it while
    // unlinked collapses all cells to the first and re-links. Registered in
    // compositeControls. Errors: asserts a well-formed spec.
    function makeClusterControl(spec) {
        console.assert(spec && Array.isArray(spec.cells), "cluster spec required");
        const box = document.createElement("div");
        box.className = "inspector__cluster";
        let linked = true;
        let unit = "px";
        const inputs = [];
        const link = document.createElement("button");
        link.type = "button";
        link.className = "inspector__cluster-link tt";
        link.setAttribute("data-tip", "Link sides");
        link.setAttribute("data-key", "");
        link.innerHTML = LINK_ICON;
        const grid = document.createElement("div");
        grid.className = "inspector__cluster-grid";
        const chip = makeUnitChip(function () { return unit; }, function (u) {
            unit = u;
            recommitAll();
        });
        function emitAll(v) {
            for (let j = 0; j < inputs.length; j++) {
                inputs[j].value = String(v);
                sendPropertyChanged(spec.cells[j].prop, v + unit);
            }
        }
        // Re-post every cell with its own current number (used on unit switch —
        // reinterpret, so per-side values are kept).
        function recommitAll() {
            for (let j = 0; j < inputs.length; j++) {
                sendPropertyChanged(spec.cells[j].prop, numOr0(inputs[j].value) + unit);
            }
        }
        for (let i = 0; i < spec.cells.length; i++) {
            const cell = makeClusterCell(spec.cells[i]);
            inputs.push(cell.input);
            grid.appendChild(cell.wrap);
            (function (idx, input) {
                input.addEventListener("change", function () {
                    const v = numOr0(input.value);
                    if (linked) {
                        emitAll(v);
                    } else {
                        sendPropertyChanged(spec.cells[idx].prop, v + unit);
                    }
                });
                input.addEventListener("keydown", function (e) {
                    if (e.key === "Enter") { e.preventDefault(); input.blur(); }
                });
            }(i, cell.input));
        }
        link.addEventListener("click", function () {
            linked = !linked;
            box.dataset.linked = linked ? "true" : "false";
            if (linked) {
                emitAll(numOr0(inputs[0].value));
            }
        });
        box.appendChild(link);
        box.appendChild(grid);
        box.appendChild(chip);
        box.syncDecls = function (decls) {
            const vals = spec.parse(decls);
            let uniform = true;
            for (let j = 0; j < inputs.length; j++) {
                setIfIdle(inputs[j], vals[j]);
                if (vals[j] !== vals[0]) {
                    uniform = false;
                }
            }
            linked = uniform;
            box.dataset.linked = linked ? "true" : "false";
            // Reflect the stored unit from the first present longhand (else px).
            let found = "";
            for (let j = 0; j < spec.cells.length && found === ""; j++) {
                found = window.__style.splitLength(decls[spec.cells[j].prop] || "").unit;
            }
            unit = found || "px";
            chip.sync();
        };
        box.dataset.linked = "true";
        Object.defineProperty(box, "value", { get: function () { return ""; }, set: function () {} });
        compositeControls.push(box);
        return box;
    }

    // makeShadowControl
    // Output: an X/Y/Blur/Spread number grid plus a colour swatch that commit a
    // single composed `box-shadow` on any change and re-sync from
    // parseBoxShadow. Shadow colour is hex only (no alpha) for now. Registered
    // in compositeControls.
    function makeShadowControl() {
        const box = document.createElement("div");
        box.className = "inspector__shadow";
        const grid = document.createElement("div");
        grid.className = "inspector__shadow-grid";
        const order = [
            { key: "x", label: "X" }, { key: "y", label: "Y" },
            { key: "blur", label: "Blur" }, { key: "spread", label: "Spread" },
        ];
        const inputs = {};
        const color = makeColorControl();
        color.classList.add("inspector__shadow-color");
        function commit() {
            sendPropertyChanged("box-shadow", window.__style.composeBoxShadow({
                x: numOr0(inputs.x.value), y: numOr0(inputs.y.value),
                blur: numOr0(inputs.blur.value), spread: numOr0(inputs.spread.value),
                color: color.value,
            }));
        }
        for (let i = 0; i < order.length; i++) {
            const cell = makeShadowCell(order[i].label);
            inputs[order[i].key] = cell.input;
            grid.appendChild(cell.wrap);
            cell.input.addEventListener("change", commit);
            (function (input) {
                input.addEventListener("keydown", function (e) {
                    if (e.key === "Enter") { e.preventDefault(); input.blur(); }
                });
            }(cell.input));
        }
        color.addEventListener("change", commit);
        box.appendChild(grid);
        box.appendChild(color);
        box.syncDecls = function (decls) {
            const s = window.__style.parseBoxShadow(decls["box-shadow"] || "");
            setIfIdle(inputs.x, s.x);
            setIfIdle(inputs.y, s.y);
            setIfIdle(inputs.blur, s.blur);
            setIfIdle(inputs.spread, s.spread);
            color.value = s.color;
        };
        Object.defineProperty(box, "value", { get: function () { return ""; }, set: function () {} });
        compositeControls.push(box);
        return box;
    }

    // parseAssetId
    // Inputs: a background-image value. Output: the asset id inside a
    // var(--asset-<id>) reference, or "" when none.
    function parseAssetId(bg) {
        const m = /var\(--asset-([^)]+)\)/.exec(String(bg == null ? "" : bg));
        return m ? m[1] : "";
    }

    // makeFillImageControl
    // Output: a no-preview image picker (same field chrome as other rows) for
    // the element's fill image. Clicking opens a file dialog; the upload routes
    // through importImageFile with the selected element id (as_element_fill),
    // so Rust writes background-image over background-color. syncDecls shows the
    // asset filename (or "Choose…") and toggles the clear button. Registered in
    // compositeControls.
    function makeFillImageControl() {
        const box = document.createElement("div");
        box.className = "inspector__fillimage";
        const name = document.createElement("span");
        name.className = "inspector__fillimage-name";
        name.textContent = "Choose…";
        const clear = document.createElement("button");
        clear.type = "button";
        clear.className = "inspector__fillimage-clear tt";
        clear.setAttribute("data-tip", "Remove image");
        clear.setAttribute("data-key", "");
        clear.hidden = true;
        clear.innerHTML = '<svg width="13" height="13" viewBox="0 0 24 24" fill="none"'
            + ' stroke="currentColor" stroke-width="2.2" stroke-linecap="round">'
            + '<path d="M6 6l12 12M18 6 6 18"/></svg>';
        const file = document.createElement("input");
        file.type = "file";
        file.accept = "image/*";
        file.style.display = "none";
        box.appendChild(name);
        box.appendChild(clear);
        box.appendChild(file);
        box.addEventListener("click", function (e) {
            if (e.target === clear || clear.contains(e.target)) {
                return;
            }
            file.click();
        });
        file.addEventListener("change", function () {
            const f = file.files && file.files[0];
            if (f && currentSelectionIds.length === 1) {
                importImageFile(f, null, false, currentSelectionIds[0]);
            }
            file.value = "";
        });
        clear.addEventListener("click", function () {
            sendPropertyChanged("background-image", "");
            sendPropertyChanged("background-size", "");
            sendPropertyChanged("background-repeat", "");
            sendPropertyChanged("background-position", "");
        });
        box.syncDecls = function (decls) {
            const id = parseAssetId(decls["background-image"] || "");
            if (id !== "") {
                name.textContent = assetFilename(id) || "Image";
                name.classList.add("inspector__fillimage-name--set");
                clear.hidden = false;
            } else {
                name.textContent = "Choose…";
                name.classList.remove("inspector__fillimage-name--set");
                clear.hidden = true;
            }
        };
        Object.defineProperty(box, "value", { get: function () { return ""; }, set: function () {} });
        compositeControls.push(box);
        return box;
    }

    // makeObjectFitControl
    // Output: a segmented Fill/Cover/Contain/Fit selector that maps to
    // background-size. The whole field hides when the element has no fill image.
    // Registered in compositeControls.
    function makeObjectFitControl() {
        const box = makeSegmentControl(OBJECT_FIT_OPTIONS);
        box.addEventListener("change", function () {
            sendPropertyChanged("background-size", box.value);
        });
        box.syncDecls = function (decls) {
            const hasImage = parseAssetId(decls["background-image"] || "") !== "";
            const field = box.closest(".inspector__field");
            if (field) {
                field.style.display = hasImage ? "" : "none";
            }
            box.value = decls["background-size"] || "";
        };
        compositeControls.push(box);
        return box;
    }

    // fontUnquote
    // Inputs: a font-family value. Output: the first family with surrounding
    // quotes stripped (the combobox shows a single bare name).
    function fontUnquote(v) {
        const s = String(v == null ? "" : v).trim();
        const first = s.split(",")[0].trim();
        return first.replace(/^["']|["']$/g, "").trim();
    }

    // makeFontComboControl
    // Output: a searchable font-family combobox — a text input plus a filtered
    // popover over the installed families (availableFonts). Typing filters;
    // ArrowUp/Down move the highlight; Enter or click commits; Esc/blur closes.
    // A free-typed value still commits (manual entry survives when a font is not
    // enumerated). Commits send a quoted font-family. Registered in
    // compositeControls; syncDecls fills the input from the element.
    function makeFontComboControl() {
        const box = document.createElement("div");
        box.className = "inspector__fontcombo";
        const input = document.createElement("input");
        input.className = "inspector__input inspector__fontcombo-input";
        input.spellcheck = false;
        input.setAttribute("autocomplete", "off");
        input.placeholder = "System default";
        const pop = document.createElement("ul");
        pop.className = "inspector__fontcombo-pop";
        pop.hidden = true;
        box.appendChild(input);
        box.appendChild(pop);
        // Buffered rendering: matches holds the full filtered list, `shown` how
        // many <li> are mounted. The popover scrolls the rest in by the chunk.
        const FONT_CHUNK = 80;
        let matches = [];
        let shown = 0;
        let current = "";
        let highlight = -1;
        function closePop() {
            pop.hidden = true;
            highlight = -1;
        }
        function commit(value) {
            const v = fontUnquote(value);
            input.value = v;
            closePop();
            if (v === current) {
                return;
            }
            current = v;
            sendPropertyChanged("font-family", v === "" ? "" : '"' + v + '"');
        }
        function computeMatches() {
            const q = input.value.trim().toLowerCase();
            matches = [];
            for (let i = 0; i < availableFonts.length; i++) {
                if (q === "" || availableFonts[i].toLowerCase().indexOf(q) >= 0) {
                    matches.push(availableFonts[i]);
                }
            }
        }
        function appendChunk() {
            const end = Math.min(shown + FONT_CHUNK, matches.length);
            for (let i = shown; i < end; i++) {
                pop.appendChild(buildFontItem(matches[i], commit));
            }
            shown = end;
        }
        function reflectHighlight() {
            for (let i = 0; i < pop.children.length; i++) {
                pop.children[i].setAttribute("aria-selected", i === highlight ? "true" : "false");
            }
            if (highlight >= 0 && pop.children[highlight]) {
                pop.children[highlight].scrollIntoView({ block: "nearest" });
            }
        }
        function renderPop() {
            computeMatches();
            pop.replaceChildren();
            shown = 0;
            appendChunk();
            highlight = matches.length > 0 ? 0 : -1;
            pop.hidden = matches.length === 0;
            reflectHighlight();
        }
        pop.addEventListener("scroll", function () {
            if (shown < matches.length
                    && pop.scrollTop + pop.clientHeight >= pop.scrollHeight - 48) {
                appendChunk();
            }
        });
        input.addEventListener("input", renderPop);
        input.addEventListener("focus", renderPop);
        input.addEventListener("keydown", function (e) {
            onFontComboKey(e, pop, input, commit, function (n) {
                highlight = n;
                reflectHighlight();
            }, function () { return highlight; }, renderPop, closePop);
        });
        input.addEventListener("blur", function () {
            window.setTimeout(closePop, 120);
            commit(input.value);
        });
        box.syncDecls = function (decls) {
            const raw = String(decls["font-family"] || "").trim();
            // A theme binding (var(--theme-*)) or no value means "system
            // default" — show the placeholder, not the raw token. Committing an
            // empty value clears the inline override back to that default.
            const isDefault = raw === "" || raw.indexOf("var(") === 0;
            current = isDefault ? "" : fontUnquote(raw);
            if (document.activeElement !== input) {
                input.value = current;
            }
        };
        Object.defineProperty(box, "value", { get: function () { return ""; }, set: function () {} });
        compositeControls.push(box);
        return box;
    }

    // buildFontItem
    // Inputs: a family name and the commit callback. Output: a popover <li>
    // previewing the family in its own face; mousedown commits (preventDefault
    // keeps input focus so no blur-close race).
    function buildFontItem(name, commit) {
        const li = document.createElement("li");
        li.className = "inspector__fontcombo-item";
        li.textContent = name;
        li.style.fontFamily = '"' + name + '"';
        li.addEventListener("mousedown", function (e) {
            e.preventDefault();
            commit(name);
        });
        return li;
    }

    // onFontComboKey
    // Inputs: the keydown event plus the combobox parts and highlight
    // get/set/render/close helpers. Output: side-effect; arrow keys move the
    // highlight, Enter commits the highlighted (or typed) value, Esc closes.
    function onFontComboKey(e, pop, input, commit, setHi, getHi, renderPop, closePop) {
        if (pop.hidden && e.key === "ArrowDown") {
            renderPop();
            return;
        }
        if (e.key === "ArrowDown") {
            e.preventDefault();
            setHi(Math.min(getHi() + 1, pop.children.length - 1));
        } else if (e.key === "ArrowUp") {
            e.preventDefault();
            setHi(Math.max(getHi() - 1, 0));
        } else if (e.key === "Enter") {
            e.preventDefault();
            const hi = getHi();
            if (!pop.hidden && hi >= 0 && pop.children[hi]) {
                commit(pop.children[hi].textContent);
            } else {
                commit(input.value);
            }
        } else if (e.key === "Escape") {
            closePop();
        }
    }

    // Declarations excluded when saving a preset: layout/identity plus the
    // element-bound fill image (an asset ref + placement belong to one element,
    // not a reusable style).
    const PRESET_EXCLUDE = {
        "left": 1, "top": 1, "width": 1, "height": 1, "transform": 1,
        "opacity": 1, "z-index": 1, "position": 1, "display": 1,
        "background-image": 1, "background-size": 1, "background-repeat": 1,
        "background-position": 1,
    };

    // capturePresetDecls
    // Inputs: an element node. Output: its style-attribute declarations minus
    // the excluded set — the reusable "look" to store in a preset rule.
    function capturePresetDecls(el) {
        const all = parseStyleAttr(el.getAttribute("style") || "");
        const out = {};
        const keys = Object.keys(all);
        for (let i = 0; i < keys.length; i++) {
            if (!PRESET_EXCLUDE[keys[i]]) {
                out[keys[i]] = all[keys[i]];
            }
        }
        return out;
    }

    // selectedElementType
    // Output: the data-element-type of the single selected element, or "".
    function selectedElementType() {
        if (currentSelectionIds.length !== 1) {
            return "";
        }
        const el = findElement(currentSelectionIds[0]);
        return (el && el.dataset.elementType) || "";
    }

    // applyPreset
    // Inputs: an element type and preset class name. Output: side-effect;
    // replays the preset's declarations as PropertyChanged commits (injecting
    // over the element's inline styles, exactly like manual field edits).
    function applyPreset(type, className) {
        const presets = window.__preset.parsePresets(currentGlobalsCss);
        let hit = null;
        for (let i = 0; i < presets.length; i++) {
            if (presets[i].type === type && presets[i].className === className) {
                hit = presets[i];
                break;
            }
        }
        if (!hit) {
            return;
        }
        const keys = Object.keys(hit.declarations);
        for (let i = 0; i < keys.length; i++) {
            sendPropertyChanged(keys[i], hit.declarations[keys[i]]);
        }
    }

    // onSavePreset
    // Inputs: the name input. Output: side-effect; captures the selected
    // element's look, upserts a [data-element-type].class rule into the globals
    // CSS, and ships GlobalsCssEditRequested. No-op without a single selection,
    // a name, or any capturable declaration.
    function onSavePreset(nameInput) {
        if (currentSelectionIds.length !== 1) {
            return;
        }
        const name = String(nameInput.value).trim();
        if (name === "") {
            return;
        }
        const el = findElement(currentSelectionIds[0]);
        if (!el) {
            return;
        }
        const type = el.dataset.elementType || "";
        const decls = capturePresetDecls(el);
        if (Object.keys(decls).length === 0) {
            return;
        }
        const className = window.__preset.slugifyClass(name);
        currentGlobalsCss = window.__preset.upsertPresetRule(currentGlobalsCss, type, className, decls);
        window.__deck.send("Interaction", {
            kind: "GlobalsCssEditRequested", new_css: currentGlobalsCss,
        });
        nameInput.value = "";
    }

    // makePresetsControl
    // Output: the Presets section — an Apply dropdown (filtered to the selected
    // element's type) and a name+Save row. Registered in compositeControls;
    // syncDecls repopulates the dropdown from the live globals CSS.
    function makePresetsControl() {
        const box = document.createElement("div");
        box.className = "inspector__presets";
        let currentType = "";
        const select = makeDropdown({
            label: "Preset",
            className: "inspector__presets-apply",
            placeholder: "Apply preset…",
            options: [],
            value: "",
            onChange: function (cls) {
                if (cls !== "" && currentType !== "") {
                    applyPreset(currentType, cls);
                }
                select.value = "";
            },
        });
        const saveRow = document.createElement("div");
        saveRow.className = "inspector__presets-save";
        const nameInput = document.createElement("input");
        nameInput.className = "inspector__input inspector__presets-name";
        nameInput.placeholder = "Save current as…";
        nameInput.spellcheck = false;
        const saveBtn = document.createElement("button");
        saveBtn.type = "button";
        saveBtn.className = "inspector__presets-savebtn";
        saveBtn.textContent = "Save";
        saveRow.appendChild(nameInput);
        saveRow.appendChild(saveBtn);
        box.appendChild(select);
        box.appendChild(saveRow);
        function rebuild() {
            const presets = window.__preset.parsePresets(currentGlobalsCss)
                .filter(function (p) { return p.type === currentType; });
            select.setPlaceholder(presets.length ? "Apply preset…" : "No presets for this type");
            select.setOptions(presets.map(function (p) {
                return { value: p.className, label: p.className };
            }));
            select.value = "";
        }
        saveBtn.addEventListener("click", function () { onSavePreset(nameInput); });
        nameInput.addEventListener("keydown", function (e) {
            if (e.key === "Enter") {
                e.preventDefault();
                onSavePreset(nameInput);
            }
        });
        box.syncDecls = function () {
            currentType = selectedElementType();
            rebuild();
        };
        Object.defineProperty(box, "value", { get: function () { return ""; }, set: function () {} });
        compositeControls.push(box);
        return box;
    }

    // makeSizeInput
    // Inputs: the geometry prop ("width"/"height") and its short label.
    // Output: { wrap, input } — a labelled number field wired to the generic
    // single-prop commit path (dataset.prop/kind drive onInspectorFieldCommit).
    function makeSizeInput(prop, label) {
        const wrap = document.createElement("div");
        wrap.className = "inspector__sizerow-cell";
        const lab = document.createElement("span");
        lab.className = "inspector__sizerow-label";
        lab.textContent = label;
        const input = document.createElement("input");
        input.className = "inspector__sizerow-input";
        input.spellcheck = false;
        input.dataset.prop = prop;
        input.dataset.kind = "number";
        input.addEventListener("change", onInspectorFieldCommit);
        input.addEventListener("keydown", function (e) {
            if (e.key === "Enter") { e.preventDefault(); input.blur(); }
        });
        wrap.appendChild(lab);
        wrap.appendChild(input);
        return { wrap: wrap, input: input };
    }

    // makeSizeRowControl
    // Output: the Width [chain] Height row. The two inputs register directly in
    // inspectorInputs (so populate/commit reach them by prop) and the centre
    // chain toggles the module-level sizeRatioLinked aspect lock that
    // maybeScaleSibling reads. The wrapper is composite (its own children are
    // wired), so buildField skips the wrapper-level change handler.
    function makeSizeRowControl() {
        const box = document.createElement("div");
        box.className = "inspector__sizerow";
        const w = makeSizeInput("width", "W");
        const h = makeSizeInput("height", "H");
        const link = document.createElement("button");
        link.type = "button";
        link.className = "inspector__sizerow-link tt";
        link.setAttribute("data-tip", "Lock width/height ratio");
        link.setAttribute("data-key", "");
        link.innerHTML = LINK_ICON;
        link.dataset.on = sizeRatioLinked ? "true" : "false";
        link.addEventListener("click", function () {
            sizeRatioLinked = !sizeRatioLinked;
            link.dataset.on = sizeRatioLinked ? "true" : "false";
        });
        box.appendChild(w.wrap);
        box.appendChild(link);
        box.appendChild(h.wrap);
        inspectorInputs.width = w.input;
        inspectorInputs.height = h.input;
        Object.defineProperty(box, "value", { get: function () { return ""; }, set: function () {} });
        return box;
    }

    // makeTextStyleControl
    // Output: the B/I/U/S toggle group. Each button commits its own CSS
    // prop directly (sendPropertyChanged), so the group is wired internally
    // rather than through the single-prop change handler. `syncDecls(decls)`
    // (called by populateInspector) stores the live declarations and reflects
    // the pressed state.
    function makeTextStyleControl() {
        const box = document.createElement("div");
        box.className = "inspector__tstyle";
        box.setAttribute("role", "group");
        box._decls = {};
        for (let i = 0; i < TEXT_STYLE_BUTTONS.length; i++) {
            const spec = TEXT_STYLE_BUTTONS[i];
            const b = document.createElement("button");
            b.type = "button";
            b.className = "inspector__tstyle-btn inspector__tstyle-btn--" + spec.cls + " tt";
            b.textContent = spec.glyph;
            b.setAttribute("aria-pressed", "false");
            b.setAttribute("data-tip", spec.tip);
            b.setAttribute("data-key", "");
            b.addEventListener("click", function () { toggleTextStyle(box, spec); });
            box.appendChild(b);
        }
        Object.defineProperty(box, "value", {
            get: function () { return ""; },
            set: function () { /* state comes from syncDecls, not .value */ },
        });
        box.syncDecls = function (decls) { syncTextStyle(box, decls || {}); };
        textStyleControls.push(box);
        return box;
    }

    // isTextStyleActive
    // Inputs: a declaration map and a TEXT_STYLE_BUTTONS spec.
    // Output: true when that style is currently applied.
    function isTextStyleActive(decls, spec) {
        const cur = String(decls[spec.prop] || "").trim();
        if (spec.list) {
            return cur.split(/\s+/).indexOf(spec.on) >= 0;
        }
        if (spec.min) {
            const n = parseInt(cur, 10);
            return isFinite(n) ? (n >= spec.min) : (cur === spec.on);
        }
        return cur === spec.on;
    }

    // toggleTextStyle
    // Inputs: the control box (holds ._decls) and the clicked spec.
    // Output: side-effect; commits the toggled value. For list props the
    // token is added/removed within the space-separated list; otherwise the
    // value flips between `on` and "" (clear). ponytail: a stale ._decls
    // between commits could drop a sibling token; the round trip re-syncs.
    function toggleTextStyle(box, spec) {
        const decls = box._decls || {};
        const active = isTextStyleActive(decls, spec);
        let next;
        if (spec.list) {
            const cur = String(decls[spec.prop] || "").trim();
            const tokens = (cur === "") ? [] : cur.split(/\s+/);
            const idx = tokens.indexOf(spec.on);
            if (active && idx >= 0) {
                tokens.splice(idx, 1);
            } else if (!active) {
                tokens.push(spec.on);
            }
            next = tokens.join(" ");
        } else {
            next = active ? "" : spec.on;
        }
        sendPropertyChanged(spec.prop, next);
    }

    // syncTextStyle
    // Inputs: the control box and a declaration map.
    // Output: side-effect; stores the decls and reflects the pressed state.
    function syncTextStyle(box, decls) {
        box._decls = decls;
        for (let i = 0; i < TEXT_STYLE_BUTTONS.length; i++) {
            const on = isTextStyleActive(decls, TEXT_STYLE_BUTTONS[i]);
            box.children[i].setAttribute("aria-pressed", on ? "true" : "false");
        }
    }

    // renderCustomDeclarations
    // Inputs: a declaration map for the selected element.
    // Output: side-effect; fills #inspector-custom-list with a removable chip
    // for every declaration not covered by a structured field (KNOWN_PROPS).
    function renderCustomDeclarations(decls) {
        const list = document.getElementById("inspector-custom-list");
        if (!list) {
            return;
        }
        list.replaceChildren();
        const keys = Object.keys(decls || {});
        for (let i = 0; i < keys.length; i++) {
            const prop = keys[i];
            if (KNOWN_PROPS[prop]) {
                continue;
            }
            list.appendChild(buildDeclChip(prop, decls[prop]));
        }
    }

    // buildDeclChip
    // Inputs: a CSS property name and its value.
    // Output: a "prop : value [×]" row; the × commits an empty value (clear).
    function buildDeclChip(prop, value) {
        const row = document.createElement("div");
        row.className = "inspector__decl";
        const p = document.createElement("span");
        p.className = "inspector__decl-prop";
        p.textContent = prop;
        const colon = document.createElement("span");
        colon.className = "inspector__decl-colon";
        colon.textContent = ":";
        const v = document.createElement("span");
        v.className = "inspector__decl-val";
        v.textContent = value;
        const rm = document.createElement("button");
        rm.type = "button";
        rm.className = "inspector__decl-remove tt";
        rm.setAttribute("data-tip", "Remove");
        rm.setAttribute("data-key", "");
        rm.innerHTML = '<svg width="13" height="13" viewBox="0 0 24 24" fill="none"'
            + ' stroke="currentColor" stroke-width="2.2" stroke-linecap="round">'
            + '<path d="M6 6l12 12M18 6 6 18"/></svg>';
        rm.addEventListener("click", function () { sendPropertyChanged(prop, ""); });
        row.appendChild(p);
        row.appendChild(colon);
        row.appendChild(v);
        row.appendChild(rm);
        return row;
    }

    // onInspectorFieldCommit
    // Inputs: an Event from a wired inspector input (change / Enter blur).
    // Output: side-effect; posts a PropertyChanged event with the field's
    // wire-encoded value. Suppresses the post when the value is unchanged
    // from what the DOM already shows (avoids round-trip churn).
    function onInspectorFieldCommit(e) {
        const input = e.target;
        if (!input || input.readOnly) {
            return;
        }
        const prop = input.dataset.prop;
        const kind = input.dataset.kind || "css";
        const raw = input.value;
        let wire = encodeForWire(kind, raw);
        // Append the CSS unit so a typed "16" lands as "16px" inline; empty
        // (clear) and null (invalid) pass through untouched.
        if (kind === "number" && input.dataset.unit && wire !== null && wire !== "") {
            wire = wire + input.dataset.unit;
        }
        if (wire === null) {
            // Invalid input — restore the displayed value from DOM and bail.
            refreshInspector();
            return;
        }
        if (kind === "css" && cssValueRejected(prop, wire)) {
            refreshInspector();
            return;
        }
        sendPropertyChanged(prop, wire);
        maybeScaleSibling(prop, wire);
    }

    // maybeScaleSibling
    // Inputs: the committed property and its wire value. Output: side-effect;
    // when the aspect lock is on and a width/height was committed, scales the
    // paired dimension by the element's live ratio and commits it too (option A
    // — ratio recomputed from the pre-edit geometry on every edit).
    function maybeScaleSibling(prop, wire) {
        if (!sizeRatioLinked || (prop !== "width" && prop !== "height")) {
            return;
        }
        if (currentSelectionIds.length !== 1) {
            return;
        }
        const el = findElement(currentSelectionIds[0]);
        if (!el) {
            return;
        }
        const decls = parseStyleAttr(el.getAttribute("style") || "");
        const curW = numOr0(stripPx(decls.width));
        const curH = numOr0(stripPx(decls.height));
        const next = numOr0(wire);
        if (curW <= 0 || curH <= 0 || next <= 0) {
            return;
        }
        const isWidth = prop === "width";
        const ratio = isWidth ? (curH / curW) : (curW / curH);
        const other = String(Math.round(next * ratio));
        const otherProp = isWidth ? "height" : "width";
        if (inspectorInputs[otherProp]) {
            inspectorInputs[otherProp].value = other;
        }
        sendPropertyChanged(otherProp, other);
    }

    // encodeForWire
    // Inputs: the input field's kind, the raw string the user typed.
    // Output: the value string to send in PropertyChanged, or null when
    // the input was invalid (e.g. non-numeric for a numeric field).
    // Dataflow: strip suffix-y characters from numeric inputs, then parse
    // and reformat; for rotation, convert degrees → radians; for css,
    // pass through verbatim (empty → clear, see interpret_property_changed
    // on the Rust side).
    function encodeForWire(kind, raw) {
        // CSS strings, select/segment tokens, and color hexes pass through.
        if (kind === "css" || kind === "select" || kind === "color"
                || kind === "segment") {
            return String(raw);
        }
        const trimmed = String(raw).trim();
        if (trimmed === "") {
            return ""; // empty → clear (only meaningful for CSS in Rust; numeric returns "" → Nothing).
        }
        // Strip optional unit suffixes ("px", "°") so the user can type
        // "200px" or "45°" and it still parses.
        const numeric = trimmed.replace(/(px|em|rem|pt|in|pc|cm|mm|deg|rad|°|%)\s*$/i, "").trim();
        const n = Number(numeric);
        if (!isFinite(n)) {
            return null;
        }
        if (kind === "rotation-deg") {
            return String(n * Math.PI / 180);
        }
        return String(n);
    }

    // onCustomCssSubmit
    // Inputs: a submit Event from the custom CSS form.
    // Output: side-effect; sends one PropertyChanged keyed on the typed
    // CSS property, with the typed value. Clears the inputs on success.
    function onCustomCssSubmit(e) {
        e.preventDefault();
        const keyInput = document.getElementById("inspector-custom-key");
        const valInput = document.getElementById("inspector-custom-value");
        if (!keyInput || !valInput) {
            return;
        }
        const prop = String(keyInput.value).trim();
        const value = String(valInput.value);
        if (prop === "") {
            return;
        }
        if (cssValueRejected(prop, value)) {
            return;
        }
        sendPropertyChanged(prop, value);
        keyInput.value = "";
        valInput.value = "";
    }

    // cssValueRejected
    // Inputs: a CSS property name and a value. Output: true when the value is
    // non-empty AND the browser rejects it for that property (so applying it
    // would silently no-op); raises an error toast as a side-effect. Empty
    // values are allowed (they clear the property).
    function cssValueRejected(property, value) {
        const v = String(value).trim();
        if (v === "") {
            return false;
        }
        if (window.CSS && typeof window.CSS.supports === "function"
                && !window.CSS.supports(property, v)) {
            showToast("Invalid value for " + property,
                property + ": '" + v + "' was rejected");
            return true;
        }
        return false;
    }

    // sendPropertyChanged
    // Inputs: a property name, a wire-formatted value.
    // Output: side-effect; posts a PropertyChanged IPC envelope for the
    // currently-selected element (no-op if no single selection).
    function sendPropertyChanged(prop, value) {
        // When a cell set is active, style writes target those cells instead of
        // the element's inline styles (per-cell style_overrides).
        if (tableCellSel && focusedTableId() === tableCellSel.elementId
                && tableCellSel.cells.length > 0) {
            window.__deck.send("Interaction", {
                kind: "CellStyleChanged",
                element_id: tableCellSel.elementId,
                cells: tableCellSel.cells.map(function (rc) { return [rc[0], rc[1]]; }),
                property: prop,
                value: value,
            });
            return;
        }
        if (currentSelectionIds.length !== 1) {
            return;
        }
        const elementId = currentSelectionIds[0];
        inspectorPending.add(prop);
        window.__deck.send("Interaction", {
            kind: "PropertyChanged",
            element_id: elementId,
            property: prop,
            value: value,
        });
    }

    // refreshInspector
    // Inputs: none (reads currentSelectionIds + shadow DOM).
    // Output: side-effect; updates the inspector subtitle and every
    // input's value. When no element is selected the inputs go blank
    // and a placeholder message appears; for a single selection the
    // values are read out of the element's `style` attribute (the DOM
    // is the source of truth visible to the user, and the tree → DOM
    // pipeline keeps the two synced).
    function refreshInspector() {
        const subtitle = document.getElementById("inspector-target");
        if (!subtitle) {
            return;
        }
        refreshCropBox();
        refreshTableBox();
        // A selected guide owns the inspector (position only).
        if (selectedGuideId !== null) {
            clearInspectorInputs();
            showGuideInspector();
            return;
        }
        hideGuideInspector();
        // No selection: in slide mode the pane targets the slide (Slide box);
        // otherwise (layout mode) just blank the element controls.
        if (currentSelectionIds.length === 0) {
            clearInspectorInputs();
            const slideMode = currentMode === "slide";
            // Both modes show the Slide box with no selection: slide mode edits
            // the active slide; layout mode edits the active layout's theme
            // background (only the Fill/Image controls — slide-only fields hide).
            subtitle.textContent = slideMode ? "Slide" : "Layout";
            setSlideBoxVisible(true);
            setElementInspectorVisible(false, null);
            renderSlideBox();
            return;
        }
        setSlideBoxVisible(false);
        if (currentSelectionIds.length > 1) {
            subtitle.textContent = currentSelectionIds.length + " selected";
            clearInspectorInputs();
            setElementInspectorVisible(false, null);
            return;
        }
        const id = currentSelectionIds[0];
        subtitle.textContent = id;
        const el = findElement(id);
        if (!el) {
            clearInspectorInputs();
            setElementInspectorVisible(false, null);
            return;
        }
        const type = el.dataset.elementType || "";
        setElementInspectorVisible(true, type);
        const decls = parseStyleAttr(el.getAttribute("style") || "");
        populateInspector(decls);
        refreshGroupFlexSection();
        inspectorPending.clear();
    }

    // setSectionVisible / setElementInspectorVisible
    // Toggle inspector sections by the selected element's type, plus the
    // custom-CSS form and Animations section (single-element chrome). When
    // `show` is false (no/multi selection) everything element-specific hides.
    function setElementInspectorVisible(show, type) {
        const root = document.getElementById("inspector-scroll");
        if (root) {
            const sections = root.querySelectorAll("[data-section-id]");
            for (let i = 0; i < sections.length; i++) {
                const sec = sections[i];
                const def = sectionDefById(sec.dataset.sectionId);
                const applies = show && def && def.appliesTo.indexOf(type) >= 0;
                sec.style.display = applies ? "" : "none";
            }
        }
    }

    function sectionDefById(id) {
        for (let i = 0; i < INSPECTOR_SECTIONS.length; i++) {
            if (INSPECTOR_SECTIONS[i].id === id) {
                return INSPECTOR_SECTIONS[i];
            }
        }
        return null;
    }

    function toggleDisplay(elementId, show) {
        const el = document.getElementById(elementId);
        if (el) {
            el.style.display = show ? "" : "none";
        }
    }

    // setSlideBoxVisible
    // Show/hide the Slide box (#slide-box), the no-selection slide-mode pane.
    // Uses an explicit display (the box's stylesheet default is hidden).
    function setSlideBoxVisible(show) {
        const el = document.getElementById("slide-box");
        if (el) {
            el.style.display = show ? "block" : "none";
        }
    }

    // renderSlideBox
    // Inputs: none (reads slideInspectorData).
    // Output: side-effect; fills the Slide box controls from the latest
    // SlideInspectorUpdate and (once) wires their commit handlers.
    function renderSlideBox() {
        wireSlideBox();
        const layoutMode = currentMode === "layout";
        // Background source: the active layout in layout mode, the active slide
        // otherwise. Slide-only fields (title/layout/notes) hide in layout mode.
        const data = layoutMode ? layoutBgData : slideInspectorData;
        const box = document.getElementById("slide-box");
        if (box) {
            const slideOnly = box.querySelectorAll("[data-slide-only]");
            for (let i = 0; i < slideOnly.length; i++) {
                slideOnly[i].hidden = layoutMode;
            }
            const header = document.getElementById("slide-box-header");
            if (header) {
                header.firstChild.textContent = layoutMode ? "Layout " : "Slide ";
            }
        }
        const bg = document.getElementById("slide-bg");
        const layout = document.getElementById("slide-layout");
        const title = document.getElementById("slide-title");
        const notes = document.getElementById("slide-notes");
        if (bg && document.activeElement !== bg) {
            bg.value = isHexColor((data && data.background) || "") ? data.background : "#000000";
        }
        const titleLabel = document.getElementById("slide-title-label");
        if (titleLabel) {
            titleLabel.textContent = layoutMode ? "Name" : "Title";
        }
        if (title && document.activeElement !== title) {
            title.value = (data && (layoutMode ? data.name : data.title)) || "";
        }
        if (notes && document.activeElement !== notes) {
            notes.value = (data && data.notes) || "";
        }
        // Background-image well: show a thumbnail of the current image (resolved
        // from the asset blob cache via its var(--asset-<id>) id) and toggle the
        // clear button.
        const bgImgPick = document.getElementById("slide-bg-image");
        const bgImgClear = document.getElementById("slide-bg-image-clear");
        if (bgImgPick) {
            const raw = (data && data.background_image) || "";
            const m = /var\(--asset-([^)]+)\)/.exec(raw);
            const url = m ? cropImageUrl(m[1]) : "";
            if (url) {
                bgImgPick.style.backgroundImage = "url(\"" + url + "\")";
                bgImgPick.textContent = "";
                bgImgPick.dataset.hasImage = "1";
            } else {
                bgImgPick.style.backgroundImage = "";
                bgImgPick.textContent = "Choose…";
                delete bgImgPick.dataset.hasImage;
            }
            if (bgImgClear) {
                bgImgClear.hidden = !url;
            }
        }
        if (layout) {
            const layouts = (data && data.layouts) || [];
            layout.setOptions(layouts.map(function (l) {
                return { value: l.id, label: l.name || l.id };
            }));
            layout.value = (data && data.layout_id) || "";
        }
        // Transition controls (slide-only): dropdown + duration/easing, the
        // timing row hidden when the transition is None (a cut).
        if (!layoutMode) {
            renderSlideTransition(data);
        }
        // Slide animation controller (slide mode only; the field is slide-only).
        if (!layoutMode) {
            renderSlideAnimations();
        }
    }

    // renderSlideTransition
    // Inputs: the active slide's inspector data.
    // Output: side-effect; reflects data.transition into the dropdown + the
    // duration/easing controls, hiding the timing row for a None (cut).
    function renderSlideTransition(data) {
        wireSlideTransition();
        const sel = document.getElementById("slide-transition");
        const timing = document.getElementById("slide-transition-timing");
        const dur = document.getElementById("slide-transition-dur");
        const ease = document.getElementById("slide-transition-easing");
        const t = (data && data.transition) || null;
        const kind = (t && t.kind) || "None";
        if (sel && document.activeElement !== sel) {
            sel.value = kind;
        }
        if (timing) {
            timing.hidden = kind === "None";
        }
        if (dur && document.activeElement !== dur) {
            dur.value = t ? String(t.duration_ms) : "400";
        }
        if (ease) {
            ease.value = (t && t.easing) || "ease-out";
        }
    }

    function isHexColor(s) {
        return /^#[0-9a-f]{6}$/i.test(String(s));
    }

    // wireSlideBox
    // Wire the Slide box controls once. Each posts an Interaction targeting the
    // active slide (the Rust side supplies the id, except the title which reuses
    // the thumbnail-rename event carrying the slide id from the cached data).
    function wireSlideBox() {
        const box = document.getElementById("slide-box");
        if (!box || box.dataset.wired) {
            return;
        }
        box.dataset.wired = "1";
        // Collapse toggle, matching the inspector sections.
        const header = document.getElementById("slide-box-header");
        if (header) {
            header.addEventListener("click", function () {
                const collapsed = box.dataset.collapsed === "true";
                box.dataset.collapsed = collapsed ? "false" : "true";
            });
        }
        // Mount the custom color control (chromeless swatch + hex) in place
        // of a raw <input type=color>; id "slide-bg" so render/commit below
        // find it. It exposes a synthetic .value + a "change" event.
        const mount = document.getElementById("slide-bg-mount");
        const bg = makeColorControl();
        bg.id = "slide-bg";
        if (mount) {
            mount.appendChild(bg);
        }
        bg.addEventListener("change", function () {
            if (bg.value === "") {
                showToast("Slide background can't be None",
                    "Pick a colour or set a background image");
                renderSlideBox();
                return;
            }
            window.__deck.send("Interaction", {
                kind: "SetSlideBackgroundRequested", background: bg.value,
            });
        });
        // Background-image well: pick imports the file as the slide bg, clear
        // resets it. Both target the active slide (Rust supplies the id).
        const bgImgPick = document.getElementById("slide-bg-image");
        const bgImgFile = document.getElementById("slide-bg-image-file");
        const bgImgClear = document.getElementById("slide-bg-image-clear");
        if (bgImgPick && bgImgFile) {
            bgImgPick.addEventListener("click", function () {
                bgImgFile.click();
            });
            bgImgFile.addEventListener("change", function () {
                const file = bgImgFile.files && bgImgFile.files[0];
                if (file) {
                    importImageFile(file, null, true);
                }
                bgImgFile.value = "";
            });
        }
        if (bgImgClear) {
            bgImgClear.addEventListener("click", function () {
                window.__deck.send("Interaction", { kind: "SetSlideBackgroundImageCleared" });
            });
        }
        const layoutMount = document.getElementById("slide-layout-mount");
        if (layoutMount && !layoutMount.dataset.wired) {
            layoutMount.dataset.wired = "1";
            const layout = makeDropdown({ label: "Layout", options: [], value: "" });
            layout.id = "slide-layout";
            layoutMount.appendChild(layout);
            layout.addEventListener("change", function () {
                window.__deck.send("Interaction", {
                    kind: "SetSlideLayoutRequested", layout_id: layout.value,
                });
            });
        }
        const title = document.getElementById("slide-title");
        if (title) {
            title.addEventListener("blur", function () {
                if (currentMode === "layout") {
                    if (!layoutBgData || !layoutBgData.layout_id) {
                        return;
                    }
                    window.__deck.send("Interaction", {
                        kind: "LayoutNameEditRequested",
                        layout_id: layoutBgData.layout_id,
                        new_name: title.value,
                    });
                    return;
                }
                if (!slideInspectorData) {
                    return;
                }
                window.__deck.send("Interaction", {
                    kind: "SlideTitleEditRequested",
                    slide_id: slideInspectorData.slide_id,
                    new_title: title.value,
                });
            });
        }
        const notes = document.getElementById("slide-notes");
        if (notes) {
            notes.addEventListener("blur", function () {
                window.__deck.send("Interaction", {
                    kind: "SetSlideNotesRequested", notes: notes.value,
                });
            });
        }
        wireSlideTransition();
    }

    // readSlideTransition: assemble a SlideTransition from the controls, or null
    // (cut) when the dropdown is None. Duration falls back to 400, easing to the
    // first preset, so a partly-filled form still sends a valid struct.
    function readSlideTransition() {
        const sel = document.getElementById("slide-transition");
        const kind = sel ? sel.value : "None";
        if (!kind || kind === "None") {
            return null;
        }
        const durEl = document.getElementById("slide-transition-dur");
        const parsed = durEl ? parseInt(durEl.value, 10) : NaN;
        const dur = Number.isFinite(parsed) && parsed > 0 ? parsed : 400;
        const easeEl = document.getElementById("slide-transition-easing");
        const easing = (easeEl && easeEl.value) || "ease-out";
        return { kind: kind, duration_ms: dur, easing: easing };
    }

    // wireSlideTransition: mount the easing segmented control and wire the three
    // transition controls; each change posts the full transition (or null).
    function wireSlideTransition() {
        const mount = document.getElementById("slide-transition-easing-mount");
        if (mount && !mount.dataset.wired) {
            mount.dataset.wired = "1";
            const opts = ANIM_EASINGS.map(function (e) {
                return { value: e.token, icon: e.label, tip: e.label };
            });
            const seg = makeSegmentControl(opts);
            seg.id = "slide-transition-easing";
            mount.appendChild(seg);
            seg.addEventListener("change", sendSlideTransition);
        }
        const selMount = document.getElementById("slide-transition-mount");
        if (selMount && !selMount.dataset.wired) {
            selMount.dataset.wired = "1";
            const sel = makeDropdown({
                label: "Transition",
                options: SLIDE_TRANSITIONS.map(function (t) { return { value: t, label: t }; }),
                value: "None",
            });
            sel.id = "slide-transition";
            selMount.appendChild(sel);
            sel.addEventListener("change", function () {
                const timing = document.getElementById("slide-transition-timing");
                if (timing) {
                    timing.hidden = sel.value === "None";
                }
                sendSlideTransition();
            });
        }
        const dur = document.getElementById("slide-transition-dur");
        if (dur) {
            dur.addEventListener("change", sendSlideTransition);
        }
    }

    // sendSlideTransition: post the active slide's transition (Rust supplies id).
    function sendSlideTransition() {
        window.__deck.send("Interaction", {
            kind: "SetSlideTransitionRequested", transition: readSlideTransition(),
        });
    }

    // initDeckTitle
    // Inputs: the deck title string and a focus flag (both from Configure).
    // Output: side-effect; fills the top-left title input, wires its commit
    // handlers once (blur + Enter post SetDeckTitleRequested), and — when
    // launched as a new deck from a layout — focuses and selects the field so
    // the user can immediately name the deck.
    function initDeckTitle(title, focus) {
        const input = document.getElementById("deck-title");
        if (!input) {
            return;
        }
        input.value = title || "";
        if (!input.dataset.wired) {
            input.dataset.wired = "1";
            const commit = function () {
                window.__deck.send("Interaction", {
                    kind: "SetDeckTitleRequested", title: input.value,
                });
            };
            input.addEventListener("blur", commit);
            input.addEventListener("keydown", function (e) {
                if (e.key === "Enter") {
                    e.preventDefault();
                    e.stopPropagation();
                    input.blur();
                }
            });
        }
        if (focus) {
            // Defer so the input is laid out and the webview has focus.
            window.requestAnimationFrame(function () {
                input.focus();
                input.select();
            });
        }
    }

    // clearInspectorInputs
    // Inputs: none.
    // Output: side-effect; empties every registered inspector input so
    // stale values do not survive a selection change.
    function clearInspectorInputs() {
        const keys = Object.keys(inspectorInputs);
        for (let i = 0; i < keys.length; i++) {
            const input = inspectorInputs[keys[i]];
            if (input) {
                input.value = "";
            }
        }
        for (let i = 0; i < textStyleControls.length; i++) {
            textStyleControls[i].syncDecls({});
        }
        for (let i = 0; i < compositeControls.length; i++) {
            compositeControls[i].syncDecls({});
        }
        renderCustomDeclarations({});
    }

    // populateInspector
    // Inputs: a parsed declaration map (property → value).
    // Output: side-effect; fills each registered input with the matching
    // declaration, mapping CSS → inspector kinds:
    //   "x"/"y"        ← left/top         (strip "px")
    //   "width"/"height" ← width/height   (strip "px")
    //   "rotation"     ← transform        (extract rad, → degrees)
    //   "opacity"      ← opacity          (verbatim)
    //   z-index        ← z-index          (verbatim, readonly)
    //   other          ← match by name    (verbatim CSS string)
    function populateInspector(decls) {
        setIfNotPending("x", stripPx(decls.left));
        setIfNotPending("y", stripPx(decls.top));
        setIfNotPending("width", stripPx(decls.width));
        setIfNotPending("height", stripPx(decls.height));
        setIfNotPending("opacity", decls.opacity || "");
        setIfNotPending("rotation", radiansToDegreesStr(extractRotationRad(decls.transform)));
        setIfNotPending("z-index", decls["z-index"] || "");
        const cssOnly = [
            // Fill/Border/Shadow now drive composite controls (below), not these
            // verbatim fields.
            // Typography props whose inspector name IS the CSS property, set
            // verbatim (select tokens, color hex, unitless nums). font-family
            // drives the combobox composite (syncDecls), not this path.
            "font-weight", "color", "text-align",
            "justify-content", "line-height",
        ];
        for (let i = 0; i < cssOnly.length; i++) {
            const key = cssOnly[i];
            setIfNotPending(key, decls[key] || "");
        }
        // Typography length fields split the stored value into the number input
        // and the unit chip.
        setUnitNumber("font-size", decls["font-size"]);
        setUnitNumber("letter-spacing", decls["letter-spacing"]);
        // Composite controls + the Custom CSS declarations list.
        for (let i = 0; i < textStyleControls.length; i++) {
            textStyleControls[i].syncDecls(decls);
        }
        for (let i = 0; i < compositeControls.length; i++) {
            compositeControls[i].syncDecls(decls);
        }
        renderCustomDeclarations(decls);
    }

    // setUnitNumber: populate a unit-number control (font-size / letter-spacing)
    // from its raw CSS value — the bare number into the input, the unit into the
    // chip. Respects the same pending / active-edit guards as setIfNotPending.
    function setUnitNumber(prop, raw) {
        const box = inspectorInputs[prop];
        if (!box || inspectorPending.has(prop)) {
            return;
        }
        const parts = window.__style.splitLength(raw);
        if (box.setUnit && parts.unit !== "") {
            box.setUnit(parts.unit);
        }
        const input = box.firstChild;
        if (document.activeElement === input) {
            return;
        }
        box.value = parts.num;
    }

    function setIfNotPending(prop, value) {
        const input = inspectorInputs[prop];
        if (!input) {
            return;
        }
        // If the user is mid-edit and waiting on the round trip, leave
        // their typed value alone. inspectorPending is cleared at the
        // end of refreshInspector — the next round arrives with the
        // commit they were waiting on.
        if (inspectorPending.has(prop)) {
            return;
        }
        if (document.activeElement === input) {
            return;
        }
        input.value = value;
    }

    // parseStyleAttr
    // Inputs: a `style` attribute string ("k: v; k: v;").
    // Output: an object map from property name → value (trimmed).
    function parseStyleAttr(s) {
        const out = {};
        const parts = s.split(";");
        for (let i = 0; i < parts.length; i++) {
            const decl = parts[i].trim();
            if (decl === "") {
                continue;
            }
            const colon = decl.indexOf(":");
            if (colon < 0) {
                continue;
            }
            const k = decl.slice(0, colon).trim();
            const v = decl.slice(colon + 1).trim();
            out[k] = v;
        }
        return out;
    }

    function stripPx(v) {
        if (typeof v !== "string") {
            return "";
        }
        return v.replace(/px\s*$/i, "").trim();
    }

    // extractRotationRad
    // Inputs: a transform CSS value (e.g. "rotate(0.5rad)").
    // Output: the rotation in radians as a number, or 0 when absent /
    // unparseable.
    function extractRotationRad(transform) {
        if (typeof transform !== "string") {
            return 0;
        }
        const m = transform.match(/rotate\(\s*([-+]?[0-9]*\.?[0-9]+)\s*rad\s*\)/i);
        if (!m) {
            return 0;
        }
        const n = Number(m[1]);
        return isFinite(n) ? n : 0;
    }

    function radiansToDegreesStr(rad) {
        const deg = rad * 180 / Math.PI;
        // Round to two decimals to keep the display clean while preserving
        // round-trip stability (the wire roundtrips via the unrounded rad).
        return String(Math.round(deg * 100) / 100);
    }

    // ---------- object panel ----------
    // Last ObjectTreeUpdate payload, retained so we can re-render
    // selection highlights without a fresh tree payload arriving.
    let lastObjectTree = null;
    // Object-pane collapse state: group element ids whose children are hidden
    // (absent = expanded). Session-only; survives ObjectTreeUpdate re-renders.
    const collapsedGroups = new Set();
    // Long-click timer + the threshold (in ms and px) that distinguishes
    // a click from a press-and-hold to rename.
    const LONG_CLICK_MS = 500;
    const LONG_CLICK_MOVE_PX = 4;
    let longClickTimer = null;
    let longClickAnchor = null; // {x, y, elementId, labelNode}
    // The data-transfer key used for drag-and-drop. We never inspect
    // dataTransfer values cross-window so any unique string works.
    const DRAG_TYPE = "application/x-carousel-element-id";
    // Tracks which element id is currently being dragged so the drop
    // target computation does not have to read dataTransfer (its values
    // are unavailable during dragover on some browsers).
    let panelDragId = null;

    // renderObjectPanel
    // Inputs: an ObjectTreeData payload (or null to render empty).
    // Output: side-effect; rebuilds #objects-tree from scratch.
    function renderObjectPanel(tree) {
        lastObjectTree = tree;
        const host = document.getElementById("objects-tree");
        if (!host) {
            return;
        }
        host.replaceChildren();
        if (!tree || !Array.isArray(tree.nodes) || tree.nodes.length === 0) {
            const empty = document.createElement("div");
            empty.className = "objects__empty";
            empty.textContent = "No elements on this slide.";
            host.appendChild(empty);
            return;
        }
        // Top-level z-order: index 0 is bottom of stack visually. The
        // panel matches that — first row in the panel = z-index 0. (Some
        // UIs invert this and list "top-most first"; we follow the data
        // model literally so the panel order matches the SPEC §11.2
        // "tree mirror" wording.)
        for (let i = 0; i < tree.nodes.length; i++) {
            host.appendChild(buildObjectNode(tree.nodes[i], 0));
        }
        updateObjectPanelSelection();
    }

    // collectGroupIds — every group element id at any depth in the tree.
    function collectGroupIds(nodes, out) {
        const list = nodes || (lastObjectTree && lastObjectTree.nodes) || [];
        const acc = out || [];
        for (let i = 0; i < list.length; i++) {
            if (list[i].element_type === "group") {
                acc.push(list[i].id);
            }
            if (Array.isArray(list[i].children) && list[i].children.length > 0) {
                collectGroupIds(list[i].children, acc);
            }
        }
        return acc;
    }

    // toggleGroupCollapsed — flip one group's collapse state and re-render.
    function toggleGroupCollapsed(id) {
        if (collapsedGroups.has(id)) {
            collapsedGroups.delete(id);
        } else {
            collapsedGroups.add(id);
        }
        renderObjectPanel(lastObjectTree);
    }

    // toggleAllGroups — the header button. If more than one group is expanded,
    // collapse every group (any depth); otherwise expand all.
    function toggleAllGroups() {
        const ids = collectGroupIds();
        let expanded = 0;
        for (let i = 0; i < ids.length; i++) {
            if (!collapsedGroups.has(ids[i])) {
                expanded += 1;
            }
        }
        if (expanded > 1) {
            for (let i = 0; i < ids.length; i++) {
                collapsedGroups.add(ids[i]);
            }
        } else {
            collapsedGroups.clear();
        }
        renderObjectPanel(lastObjectTree);
    }

    // buildObjectNode
    // Inputs: an ObjectTreeNode (id, element_type, children), the depth
    // (used purely so future styling can target nesting level).
    // Output: a DOM subtree representing this node and its descendants.
    function buildObjectNode(node, depth) {
        const wrap = document.createElement("div");
        wrap.className = "objects__node-wrap";
        wrap.dataset.elementId = node.id;
        wrap.dataset.depth = String(depth);

        const row = document.createElement("div");
        row.className = "objects__node";
        row.setAttribute("role", "treeitem");
        row.draggable = true;
        row.dataset.elementId = node.id;
        row.dataset.elementType = node.element_type;
        row.tabIndex = 0;

        // Disclosure triangle — visible-but-inert for non-groups so
        // alignment stays consistent.
        const disclosure = document.createElement("span");
        disclosure.className = "objects__disclosure";
        const collapsed = node.element_type === "group" && collapsedGroups.has(node.id);
        if (node.element_type === "group") {
            disclosure.textContent = collapsed ? "▸" : "▾";
            disclosure.dataset.role = "disclosure";
            disclosure.addEventListener("click", function (e) {
                e.stopPropagation();
                toggleGroupCollapsed(node.id);
            });
        } else {
            disclosure.classList.add("objects__disclosure--empty");
            disclosure.textContent = "•";
        }
        if (collapsed) {
            wrap.dataset.collapsed = "true";
        }
        row.appendChild(disclosure);

        const badge = document.createElement("span");
        badge.className = "objects__badge objects__badge--" + node.element_type;
        badge.textContent = badgeGlyph(node.element_type);
        row.appendChild(badge);

        const label = document.createElement("span");
        label.className = "objects__label";
        label.textContent = node.id;
        label.dataset.role = "label";
        row.appendChild(label);

        // Listeners.
        row.addEventListener("mousedown", onPanelMouseDown);
        row.addEventListener("dragstart", onPanelDragStart);
        row.addEventListener("dragover", onPanelDragOver);
        row.addEventListener("dragleave", onPanelDragLeave);
        row.addEventListener("drop", onPanelDrop);
        row.addEventListener("dragend", onPanelDragEnd);
        row.addEventListener("dblclick", function (e) {
            // Double-click (and long-click) edit the element's id — the
            // value shown on the row, the data-element-id, and the
            // object-tree key, all one and the same.
            e.preventDefault();
            editElementId(label, node.id);
        });

        wrap.appendChild(row);

        if (Array.isArray(node.children) && node.children.length > 0) {
            const kids = document.createElement("div");
            kids.className = "objects__children";
            for (let i = 0; i < node.children.length; i++) {
                kids.appendChild(buildObjectNode(node.children[i], depth + 1));
            }
            wrap.appendChild(kids);
        }
        return wrap;
    }

    function badgeGlyph(type) {
        switch (type) {
            case "text": return "T";
            case "shape": return "▭";
            case "group": return "▤";
            case "image": return "▣";
            case "media": return "▶";
            case "table": return "▦";
            case "embed": return "<>";
            default: return "?";
        }
    }

    // updateObjectPanelSelection
    // Inputs: none (reads currentSelectionIds + the rendered tree).
    // Output: side-effect; sets aria-selected on every row whose id
    // appears in the current selection set, clears the others.
    function updateObjectPanelSelection() {
        const host = document.getElementById("objects-tree");
        if (!host) {
            return;
        }
        const rows = host.querySelectorAll(".objects__node");
        const selected = new Set(currentSelectionIds);
        for (let i = 0; i < rows.length; i++) {
            const id = rows[i].dataset.elementId || "";
            rows[i].setAttribute("aria-selected", selected.has(id) ? "true" : "false");
        }
    }

    // onPanelMouseDown
    // Inputs: mousedown on a node row.
    // Output: side-effect; (a) sends SetSelectionFromPanel for the
    // clicked element (shift extends selection), (b) starts the
    // long-click timer that escalates a hold into rename mode.
    function onPanelMouseDown(e) {
        if (e.button !== 0) {
            return;
        }
        // Ignore clicks that started on the disclosure chevron — those
        // are reserved for future expand/collapse; passing through to
        // selection here would be confusing.
        if (e.target && e.target.dataset && e.target.dataset.role === "disclosure") {
            return;
        }
        const row = e.currentTarget;
        const elementId = row.dataset.elementId;
        if (!elementId) {
            return;
        }
        sendPanelSelection(elementId, !!e.shiftKey);
        const label = row.querySelector("[data-role='label']");
        if (label) {
            armLongClick(e.clientX, e.clientY, elementId, label);
        }
    }

    function sendPanelSelection(elementId, additive) {
        let ids;
        if (additive) {
            const existing = new Set(currentSelectionIds);
            if (existing.has(elementId)) {
                existing.delete(elementId);
            } else {
                existing.add(elementId);
            }
            ids = Array.from(existing);
        } else {
            ids = [elementId];
        }
        window.__deck.send("Interaction", {
            kind: "SetSelectionFromPanel",
            element_ids: ids,
        });
    }

    function armLongClick(x, y, elementId, labelNode) {
        cancelLongClick();
        longClickAnchor = { x: x, y: y, elementId: elementId, labelNode: labelNode };
        longClickTimer = window.setTimeout(function () {
            if (longClickAnchor) {
                editElementId(longClickAnchor.labelNode, longClickAnchor.elementId);
            }
            cancelLongClick();
        }, LONG_CLICK_MS);
        window.addEventListener("mousemove", onLongClickMove);
        window.addEventListener("mouseup", onLongClickRelease);
    }

    function cancelLongClick() {
        if (longClickTimer !== null) {
            window.clearTimeout(longClickTimer);
            longClickTimer = null;
        }
        longClickAnchor = null;
        window.removeEventListener("mousemove", onLongClickMove);
        window.removeEventListener("mouseup", onLongClickRelease);
    }

    function onLongClickMove(e) {
        if (!longClickAnchor) {
            cancelLongClick();
            return;
        }
        const dx = e.clientX - longClickAnchor.x;
        const dy = e.clientY - longClickAnchor.y;
        if (Math.hypot(dx, dy) > LONG_CLICK_MOVE_PX) {
            cancelLongClick();
        }
    }

    function onLongClickRelease() {
        cancelLongClick();
    }

    // floatingEdit
    // Inputs: an anchor node to position over, the initial text, and a
    // commit callback invoked with the final value (only on commit, not on
    // cancel). Spawns a fixed-position <input> overlaid on the anchor
    // rather than nesting one inside it — so it works even when the anchor
    // lives inside a <button> (the thumbnail label), where a nested input
    // would be invalid HTML. Enter / blur commit; Escape cancels. The
    // backend is authoritative for the committed value (it sanitizes ids
    // and rebroadcasts), so this only sends the raw text.
    function floatingEdit(anchorNode, initialValue, commitFn) {
        if (!anchorNode || document.querySelector(".floating-edit")) {
            return; // one editor at a time
        }
        const rect = anchorNode.getBoundingClientRect();
        const input = document.createElement("input");
        input.type = "text";
        input.className = "floating-edit";
        input.value = initialValue;
        input.spellcheck = false;
        input.style.left = rect.left + "px";
        input.style.top = rect.top + "px";
        input.style.width = Math.max(rect.width, 80) + "px";
        document.body.appendChild(input);
        input.focus();
        input.select();
        let resolved = false;
        const finish = function (commit) {
            if (resolved) {
                return;
            }
            resolved = true;
            const value = input.value;
            if (input.parentNode) {
                input.parentNode.removeChild(input);
            }
            if (commit) {
                commitFn(value);
            }
        };
        input.addEventListener("blur", function () {
            finish(true);
        });
        input.addEventListener("keydown", function (e) {
            e.stopPropagation();
            if (e.key === "Enter") {
                e.preventDefault();
                finish(true);
            } else if (e.key === "Escape") {
                e.preventDefault();
                finish(false);
            }
        });
    }

    // editElementId
    // Inputs: the label DOM node to overlay, and the element's current id.
    // Output: side-effect; opens a floating editor prefilled with the id
    // and, on commit, sends ElementIdEditRequested. The Rust side
    // sanitizes the value (whitespace runs → '_'), renames the element,
    // remounts, and refreshes the panel. Shared by the object panel's
    // double-click and long-click affordances.
    function editElementId(labelNode, elementId) {
        floatingEdit(labelNode, elementId, function (value) {
            window.__deck.send("Interaction", {
                kind: "ElementIdEditRequested",
                element_id: elementId,
                new_id: value,
            });
        });
    }

    // ----- drag-and-drop -----

    function onPanelDragStart(e) {
        cancelLongClick();
        const row = e.currentTarget;
        const elementId = row.dataset.elementId || "";
        if (!elementId) {
            e.preventDefault();
            return;
        }
        panelDragId = elementId;
        if (e.dataTransfer) {
            e.dataTransfer.setData(DRAG_TYPE, elementId);
            e.dataTransfer.effectAllowed = "move";
        }
    }

    function onPanelDragEnd() {
        clearDropTargets();
        panelDragId = null;
    }

    function clearDropTargets() {
        const tree = document.getElementById("objects-tree");
        if (!tree) {
            return;
        }
        const rows = tree.querySelectorAll(".objects__node[data-drop-target]");
        for (let i = 0; i < rows.length; i++) {
            rows[i].removeAttribute("data-drop-target");
        }
    }

    // onPanelDragOver
    // Inputs: dragover event on a row.
    // Output: side-effect; sets data-drop-target on this row to one of
    // "before" / "after" / "inside", governs the visual cue.
    function onPanelDragOver(e) {
        if (!panelDragId) {
            return;
        }
        const row = e.currentTarget;
        const targetId = row.dataset.elementId || "";
        if (targetId === panelDragId) {
            return; // cannot drop on itself
        }
        e.preventDefault();
        if (e.dataTransfer) {
            e.dataTransfer.dropEffect = "move";
        }
        const rect = row.getBoundingClientRect();
        const y = e.clientY - rect.top;
        const isGroup = row.dataset.elementType === "group";
        let zone;
        if (isGroup) {
            if (y < rect.height * 0.25) {
                zone = "before";
            } else if (y > rect.height * 0.75) {
                zone = "after";
            } else {
                zone = "inside";
            }
        } else {
            zone = y < rect.height / 2 ? "before" : "after";
        }
        clearDropTargets();
        row.dataset.dropTarget = zone;
    }

    function onPanelDragLeave(e) {
        const row = e.currentTarget;
        // Only clear if leaving for an unrelated target — re-entering
        // immediately on dragover will set it again.
        if (e.relatedTarget && row.contains(e.relatedTarget)) {
            return;
        }
        row.removeAttribute("data-drop-target");
    }

    // onPanelDrop
    // Inputs: drop event on a row.
    // Output: side-effect; sends ReparentElementRequested with the
    // computed (parent, position) coordinates. Position is in
    // post-removal terms — see ReparentElement docs on the Rust side.
    function onPanelDrop(e) {
        if (!panelDragId) {
            return;
        }
        const row = e.currentTarget;
        const targetId = row.dataset.elementId || "";
        const zone = row.dataset.dropTarget || "";
        clearDropTargets();
        if (targetId === panelDragId || !zone) {
            return;
        }
        e.preventDefault();

        const dragId = panelDragId;
        const dropInfo = computeDropTarget(dragId, targetId, zone);
        panelDragId = null;
        if (!dropInfo) {
            return;
        }
        window.__deck.send("Interaction", {
            kind: "ReparentElementRequested",
            element_id: dragId,
            new_parent_id: dropInfo.new_parent_id,
            new_position: dropInfo.new_position,
        });
    }

    // computeDropTarget
    // Inputs: the dragging element id, the row id under the cursor,
    // and the zone ("before" | "after" | "inside").
    // Output: { new_parent_id, new_position } in post-removal coordinates,
    // or null when the result would be a no-op or invalid.
    // Dataflow:
    //   1. Look up source (parent_id, index) in lastObjectTree.
    //   2. Look up target (parent_id, index) in lastObjectTree.
    //   3. Translate zone into a target parent + display index:
    //        "before" → target.parent, target.index
    //        "after"  → target.parent, target.index + 1
    //        "inside" → target.id itself, end-of-children
    //   4. Adjust for post-removal: when source.parent == target.parent
    //      and source.index < display_index, subtract 1.
    function computeDropTarget(dragId, targetId, zone) {
        if (!lastObjectTree) {
            return null;
        }
        const tree = lastObjectTree;
        const source = locateInTree(tree, dragId);
        const target = locateInTree(tree, targetId);
        if (!source || !target) {
            return null;
        }
        let newParentId;
        let displayIndex;
        if (zone === "inside") {
            // Cannot drop onto self-as-parent already filtered above.
            // Also guard against dropping under one's own descendant.
            if (containsDescendant(source.node, targetId)) {
                return null;
            }
            newParentId = targetId;
            displayIndex = target.node.children.length;
        } else {
            newParentId = target.parentId;
            displayIndex = zone === "before" ? target.index : target.index + 1;
        }
        let position = displayIndex;
        if (source.parentId === newParentId && source.index < displayIndex) {
            position -= 1;
        }
        if (source.parentId === newParentId && source.index === position) {
            return null; // no-op
        }
        return { new_parent_id: newParentId, new_position: position };
    }

    // locateInTree
    // Inputs: an ObjectTreeData payload and an element id.
    // Output: { node, parentId, index } when found, else null. parentId
    // for top-level nodes is the slide root id (tree.root_id).
    function locateInTree(tree, elementId) {
        if (!tree || !Array.isArray(tree.nodes)) {
            return null;
        }
        return scanLevel(tree.nodes, tree.root_id, elementId);
    }

    function scanLevel(nodes, parentId, elementId) {
        for (let i = 0; i < nodes.length; i++) {
            if (nodes[i].id === elementId) {
                return { node: nodes[i], parentId: parentId, index: i };
            }
            if (Array.isArray(nodes[i].children) && nodes[i].children.length > 0) {
                const found = scanLevel(nodes[i].children, nodes[i].id, elementId);
                if (found) {
                    return found;
                }
            }
        }
        return null;
    }

    function containsDescendant(node, candidateId) {
        if (!node || !Array.isArray(node.children)) {
            return false;
        }
        for (let i = 0; i < node.children.length; i++) {
            if (node.children[i].id === candidateId) {
                return true;
            }
            if (containsDescendant(node.children[i], candidateId)) {
                return true;
            }
        }
        return false;
    }

    // ----- toolbar -----

    function wireObjectsToolbar() {
        const buttons = document.querySelectorAll(".objects__add");
        for (let i = 0; i < buttons.length; i++) {
            buttons[i].addEventListener("click", function (e) {
                e.preventDefault();
                const type = e.currentTarget.dataset.elementType || "";
                if (!type) {
                    return;
                }
                window.__deck.send("Interaction", {
                    kind: "InsertElementRequested",
                    element_type: type,
                });
            });
        }
        const collapseAll = document.getElementById("objects-collapse-all");
        if (collapseAll) {
            collapseAll.addEventListener("click", toggleAllGroups);
        }
        // Add image: pick a file, then import it as a centered new image
        // element (position null -> Rust centers it on the slide).
        const addImage = document.getElementById("tool-add-image");
        if (addImage) {
            const picker = document.createElement("input");
            picker.type = "file";
            picker.accept = "image/*";
            picker.style.display = "none";
            addImage.appendChild(picker);
            addImage.addEventListener("click", function () { picker.click(); });
            picker.addEventListener("change", function () {
                const f = picker.files && picker.files[0];
                if (f) {
                    importImageFile(f, null);
                }
                picker.value = "";
            });
        }
        // Undo / redo: reuse the synthetic-key path the accelerators use.
        const undoBtn = document.getElementById("undo-btn");
        if (undoBtn) {
            undoBtn.addEventListener("click", function () { sendSyntheticKey("undo", {}); });
        }
        const redoBtn = document.getElementById("redo-btn");
        if (redoBtn) {
            redoBtn.addEventListener("click", function () { sendSyntheticKey("redo", {}); });
        }
    }

    // ----- share / export menu -----

    // The three export actions, each a card in the Share dropdown. `key` is the
    // synthetic accelerator name the Rust side already handles (Save / Export
    // HTML / Print PDF); `icon` is inline SVG markup.
    const SHARE_EXPORTS = [
        {
            key: "save_deck",
            name: "Save to file",
            sub: "Carousel deck",
            icon: '<path d="M5 4h11l3 3v13H5zM8 4v5h7M8 14h8M8 17h8"/>',
        },
        {
            key: "export_html",
            name: "Export for web",
            sub: "HTML",
            icon: '<path d="M9 8 5 12l4 4M15 8l4 4-4 4"/>',
        },
        {
            key: "export_pdf",
            name: "Print to PDF",
            sub: "Document",
            icon: '<path d="M7 3h7l4 4v14H7zM14 3v4h4M10 13h4M10 16h4"/>',
        },
    ];

    // buildShareMenu — the export dropdown: one rectangular icon+name card per
    // SHARE_EXPORTS entry. Clicking a card fires its synthetic accelerator and
    // closes the menu (the close handler is wired by the caller).
    function buildShareMenu(onPick) {
        const menu = document.createElement("div");
        menu.id = "share-menu";
        menu.className = "share-menu";
        menu.hidden = true;
        for (let i = 0; i < SHARE_EXPORTS.length; i++) {
            const opt = SHARE_EXPORTS[i];
            const card = document.createElement("button");
            card.type = "button";
            card.className = "share-menu__card";
            const ic = document.createElement("span");
            ic.className = "share-menu__icon";
            ic.innerHTML = '<svg width="18" height="18" viewBox="0 0 24 24" fill="none"'
                + ' stroke="currentColor" stroke-width="1.7" stroke-linecap="round"'
                + ' stroke-linejoin="round">' + opt.icon + "</svg>";
            const txt = document.createElement("span");
            txt.className = "share-menu__text";
            const name = document.createElement("span");
            name.className = "share-menu__name";
            name.textContent = opt.name;
            const sub = document.createElement("span");
            sub.className = "share-menu__sub";
            sub.textContent = opt.sub;
            txt.appendChild(name);
            txt.appendChild(sub);
            card.appendChild(ic);
            card.appendChild(txt);
            card.addEventListener("click", function () { onPick(opt.key); });
            menu.appendChild(card);
        }
        return menu;
    }

    // wireShareMenu — toggle the export dropdown under the Share button. A card
    // click runs the matching synthetic accelerator; an outside click or Escape
    // closes it.
    function wireShareMenu() {
        const btn = document.getElementById("share-btn");
        if (!btn) {
            return;
        }
        let isOpen = false;
        const menu = buildShareMenu(function (key) {
            close();
            sendSyntheticKey(key, {});
        });
        document.body.appendChild(menu);
        function onDoc(e) {
            if (!menu.contains(e.target) && !btn.contains(e.target)) {
                close();
            }
        }
        function onKey(e) {
            if (e.key === "Escape") { close(); }
        }
        function close() {
            if (!isOpen) { return; }
            isOpen = false;
            menu.hidden = true;
            btn.setAttribute("aria-expanded", "false");
            document.removeEventListener("mousedown", onDoc, true);
            document.removeEventListener("keydown", onKey, true);
        }
        function open() {
            const r = btn.getBoundingClientRect();
            menu.style.top = (r.bottom + 6) + "px";
            menu.style.right = Math.max(8, window.innerWidth - r.right) + "px";
            menu.hidden = false;
            isOpen = true;
            btn.setAttribute("aria-expanded", "true");
            document.addEventListener("mousedown", onDoc, true);
            document.addEventListener("keydown", onKey, true);
        }
        btn.addEventListener("click", function (e) {
            e.preventDefault();
            if (isOpen) { close(); } else { open(); }
        });
    }

    // showChromiumDownload — open/update the PDF-export Chromium download modal.
    // total null -> indeterminate bar; else a percentage fill.
    function showChromiumDownload(received, total) {
        let box = document.getElementById("chromium-download");
        if (!box) {
            box = document.createElement("div");
            box.id = "chromium-download";
            box.className = "chromium-dl";
            box.innerHTML = '<div class="chromium-dl__panel">'
                + '<h2 class="chromium-dl__title">Downloading Chromium…</h2>'
                + '<p class="chromium-dl__sub">Needed once to export PDF.</p>'
                + '<div class="chromium-dl__track"><div class="chromium-dl__bar"></div></div>'
                + '</div>';
            document.body.appendChild(box);
        }
        const bar = box.querySelector(".chromium-dl__bar");
        if (total && total > 0) {
            bar.classList.remove("chromium-dl__bar--indet");
            bar.style.width = Math.min(100, Math.round((received / total) * 100)) + "%";
        } else {
            bar.classList.add("chromium-dl__bar--indet");
        }
    }

    // finishChromiumDownload — close on success, or show the error inline.
    function finishChromiumDownload(ok, message) {
        const box = document.getElementById("chromium-download");
        if (!box) {
            return;
        }
        if (ok) {
            box.remove();
        } else {
            const sub = box.querySelector(".chromium-dl__sub");
            if (sub) {
                sub.textContent = message || "Download failed.";
                sub.style.color = "#c0392b";
            }
        }
    }

    // showQuitDialog — raise the unsaved-changes quit confirmation. Cancel
    // dismisses locally; the other two buttons reply with QuitConfirmed and let
    // Rust drive the save (if any) and the exit. Idempotent: re-raising while
    // already open is a no-op.
    function showQuitDialog() {
        if (document.getElementById("quit-dialog")) {
            return;
        }
        const box = document.createElement("div");
        box.id = "quit-dialog";
        box.className = "quit-dlg";
        box.innerHTML = '<div class="quit-dlg__panel" role="dialog" aria-modal="true">'
            + '<h2 class="quit-dlg__title">Unsaved changes</h2>'
            + '<p class="quit-dlg__sub">Save your work before exiting?</p>'
            + '<div class="quit-dlg__row">'
            + '<button type="button" class="quit-dlg__btn quit-dlg__btn--cancel">Cancel</button>'
            + '<button type="button" class="quit-dlg__btn quit-dlg__btn--discard">Exit without saving</button>'
            + '<button type="button" class="quit-dlg__btn quit-dlg__btn--save">Save and exit</button>'
            + '</div></div>';
        box.querySelector(".quit-dlg__btn--cancel").addEventListener("click", function () {
            box.remove();
        });
        box.querySelector(".quit-dlg__btn--discard").addEventListener("click", function () {
            window.__deck.send("Interaction", { kind: "QuitConfirmed", save: false });
        });
        box.querySelector(".quit-dlg__btn--save").addEventListener("click", function () {
            window.__deck.send("Interaction", { kind: "QuitConfirmed", save: true });
        });
        document.body.appendChild(box);
    }

    // ---------- thumbnail row ----------
    // Slide dimensions sent by the last SlideListUpdate. Thumbnails
    // are rendered by mounting a copy of the slide HTML inside a small
    // container and applying a CSS scale so the 1920×1080 slide fits
    // into ~160×90.
    let thumbnailDims = { width: 1920, height: 1080 };
    let thumbnailThemeCss = "";
    // slideId -> cached HTML. MountSlide refreshes individual entries.
    const thumbnailHtmlCache = Object.create(null);
    // The currently-mounted slide id, kept locally so highlightActive…
    // can run even if SlideListUpdate hasn't arrived yet.
    let activeSlideId = null;

    // THUMB_KINDS
    // Per-mode descriptors so the thumbnail row renders slides or layouts
    // from one set of functions (Stage 11). Each maps the payload/entry
    // shape and the interaction events for its kind. The DOM mount id stays
    // `dataset.slideId` for both so MountSlide's updateThumbnailHtml(id)
    // keys uniformly (in layout mode the mounted canvas id IS the layout
    // id).
    const THUMB_KINDS = {
        slide: {
            listKey: "slides",
            activeKey: "active_slide_id",
            idOf: function (e) { return e.slide_id; },
            labelOf: function (e) { return e.title || e.slide_id; },
            // Untitled slides fall back to the id; start the rename editor
            // empty rather than prefilling a ULID.
            editInitial: function (e) {
                return (e.title === e.slide_id) ? "" : (e.title || "");
            },
            clickKind: "SlideThumbnailClicked",
            clickField: "slide_id",
            renameKind: "SlideTitleEditRequested",
            renameField: "new_title",
            addKind: "AddSlideRequested",
            // Slides open a layout picker first (previews of the theme's
            // layouts) instead of inserting a blank slide outright.
            pickerKind: "SlideLayoutPickerRequested",
            emptyText: "No slides.",
            addTitle: "New slide",
        },
        layout: {
            listKey: "layouts",
            activeKey: "active_layout_id",
            idOf: function (e) { return e.layout_id; },
            labelOf: function (e) { return e.name || e.layout_id; },
            editInitial: function (e) { return e.name || ""; },
            clickKind: "LayoutThumbnailClicked",
            clickField: "layout_id",
            renameKind: "LayoutNameEditRequested",
            renameField: "new_name",
            addKind: "AddLayoutRequested",
            emptyText: "No layouts.",
            addTitle: "New layout",
        },
    };

    // renderThumbnailRow
    // Inputs: a SlideListData / LayoutListData payload and the kind
    // ("slide" | "layout").
    // Output: side-effect; rebuilds #thumbnail-row from scratch with one
    // .thumb per item, each mounting the item HTML inside its own shadow
    // root at a scaled-down size, followed by the "+" add tile.
    function renderThumbnailRow(payload, kind) {
        const spec = THUMB_KINDS[kind] || THUMB_KINDS.slide;
        const row = document.getElementById("thumbnail-row");
        if (!row) {
            return;
        }
        thumbnailDims = {
            width: (payload && payload.width) || 1920,
            height: (payload && payload.height) || 1080,
        };
        thumbnailThemeCss = (payload && payload.theme_css) || "";
        if (payload && typeof payload.globals_css === "string") {
            currentGlobalsCss = payload.globals_css;
        }
        const items = (payload && Array.isArray(payload[spec.listKey]))
            ? payload[spec.listKey]
            : [];
        // Slide count badge in the thumbnails header.
        if (kind === "slide") {
            const badge = document.getElementById("thumbs-count");
            if (badge) {
                badge.textContent = String(items.length);
            }
        }
        // Seed / refresh the HTML cache from the payload.
        for (let i = 0; i < items.length; i++) {
            const entry = items[i];
            const id = entry && spec.idOf(entry);
            if (id) {
                thumbnailHtmlCache[id] = entry.html || "";
            }
        }
        row.replaceChildren();
        if (items.length === 0) {
            const empty = document.createElement("div");
            empty.className = "thumb__empty";
            empty.textContent = spec.emptyText;
            row.appendChild(empty);
            row.appendChild(buildAddTile(spec));
            return;
        }
        const active = (payload && payload[spec.activeKey]) || activeSlideId;
        if (active) {
            activeSlideId = active;
        }
        for (let i = 0; i < items.length; i++) {
            row.appendChild(buildThumbnail(items[i], i, active, spec));
        }
        row.appendChild(buildAddTile(spec));
        updateSlideFocusState();
        refitThumbnails();
        scrollActiveThumbnailIntoView();
    }

    // buildAddTile
    // Inputs: the kind spec.
    // Output: a <button>.thumb--add DOM node that asks the Rust side to
    // insert a blank slide / layout after the active one. Lives at the tail
    // of the row so it reads as "append".
    function buildAddTile(spec) {
        const btn = document.createElement("button");
        btn.type = "button";
        btn.className = "thumb thumb--add";
        btn.title = spec.addTitle;
        btn.setAttribute("aria-label", spec.addTitle);

        const glyph = document.createElement("span");
        glyph.className = "thumb__add-glyph";
        glyph.setAttribute("aria-hidden", "true");
        glyph.textContent = "+";
        btn.appendChild(glyph);

        const label = document.createElement("span");
        label.className = "thumb__label";
        label.textContent = "New";
        btn.appendChild(label);

        btn.addEventListener("click", function () {
            window.__deck.send("Interaction", { kind: spec.pickerKind || spec.addKind });
        });
        return btn;
    }

    // closeLayoutPicker
    // Output: side-effect; removes the new-slide layout picker overlay and its
    // Esc listener, if present.
    function closeLayoutPicker() {
        const existing = document.getElementById("layout-picker");
        if (existing) {
            existing.remove();
        }
        document.removeEventListener("keydown", onLayoutPickerKey, true);
    }

    // onLayoutPickerKey — Esc dismisses the picker (capture so it wins over
    // other global keydown handlers).
    function onLayoutPickerKey(e) {
        if (e.key === "Escape") {
            e.preventDefault();
            e.stopPropagation();
            closeLayoutPicker();
        }
    }

    // pickLayoutTile
    // Inputs: a layout id ("" for blank), a display label, and the optional
    // entry HTML for the preview (empty -> a plain blank tile).
    // Output: a button mounting a scaled preview that, on click, inserts a new
    // slide seeded from that layout and closes the picker.
    function pickLayoutTile(layoutId, label, html) {
        const btn = document.createElement("button");
        btn.type = "button";
        btn.className = "layout-picker__tile";
        const preview = document.createElement("div");
        preview.className = "thumb__preview layout-picker__preview";
        const mount = document.createElement("div");
        mount.className = "thumb__mount";
        const shadow = mount.attachShadow({ mode: "open" });
        shadow.innerHTML = "<style>" + thumbnailThemeCss + "</style>"
            + "<style class=\"globals-css\">" + currentGlobalsCss + "</style>"
            + "<style class=\"anim-kf\">" + builtinKeyframesCss + "</style>"
            + "<style class=\"asset-vars\">" + buildAssetVarCss() + "</style>"
            + (html || "");
        preview.appendChild(mount);
        const cap = document.createElement("span");
        cap.className = "layout-picker__label";
        cap.textContent = label;
        btn.appendChild(preview);
        btn.appendChild(cap);
        window.requestAnimationFrame(function () { applyThumbnailScale(preview, mount); });
        btn.addEventListener("click", function () {
            window.__deck.send("Interaction", { kind: "AddSlideRequested", layout_id: layoutId });
            closeLayoutPicker();
        });
        return btn;
    }

    // openLayoutPicker
    // Inputs: a SlideLayoutPickerData payload (theme layouts + preview HTML,
    // theme/globals CSS, dimensions).
    // Output: side-effect; pops a modal overlay of layout previews (plus a
    // Blank option). Choosing one inserts a new slide on that layout. Backdrop
    // click or Esc dismisses.
    function openLayoutPicker(payload) {
        closeLayoutPicker();
        thumbnailDims = {
            width: (payload && payload.width) || 1920,
            height: (payload && payload.height) || 1080,
        };
        thumbnailThemeCss = (payload && payload.theme_css) || "";
        if (payload && typeof payload.globals_css === "string") {
            currentGlobalsCss = payload.globals_css;
        }
        const layouts = (payload && Array.isArray(payload.layouts)) ? payload.layouts : [];
        const overlay = document.createElement("div");
        overlay.id = "layout-picker";
        overlay.className = "layout-picker";
        const panel = document.createElement("div");
        panel.className = "layout-picker__panel";
        const title = document.createElement("h2");
        title.className = "layout-picker__title";
        title.textContent = "Choose a layout";
        const grid = document.createElement("div");
        grid.className = "layout-picker__grid";
        grid.appendChild(pickLayoutTile("", "Blank", ""));
        for (let i = 0; i < layouts.length; i++) {
            const l = layouts[i];
            grid.appendChild(pickLayoutTile(l.layout_id, l.name || l.layout_id, l.html));
        }
        panel.appendChild(title);
        panel.appendChild(grid);
        overlay.appendChild(panel);
        overlay.addEventListener("mousedown", function (e) {
            if (e.target === overlay) {
                closeLayoutPicker();
            }
        });
        document.body.appendChild(overlay);
        document.addEventListener("keydown", onLayoutPickerKey, true);
    }

    // buildThumbnail
    // Inputs: a list entry, its display index (1-based badge), the active
    // id, and the kind spec.
    // Output: a <button>.thumb DOM node fully wired (click → switch active
    // slide/layout; dblclick label → rename).
    function buildThumbnail(entry, index, activeId, spec) {
        const itemId = spec.idOf(entry);
        const btn = document.createElement("button");
        btn.type = "button";
        btn.className = "thumb";
        btn.dataset.slideId = itemId;
        if (itemId === activeId) {
            btn.setAttribute("aria-current", "true");
        }
        btn.title = spec.labelOf(entry);

        const preview = document.createElement("div");
        preview.className = "thumb__preview";

        const mount = document.createElement("div");
        mount.className = "thumb__mount";
        mount.dataset.slideId = itemId;
        // Mount inside its own shadow root so theme + globals CSS are
        // scoped. The asset-vars block resolves any image elements to blob
        // URLs, mirroring the viewport mount.
        const shadow = mount.attachShadow({ mode: "open" });
        shadow.innerHTML = "<style>" + thumbnailThemeCss + "</style>"
            + "<style class=\"globals-css\">" + currentGlobalsCss + "</style>"
            + "<style class=\"anim-kf\">" + builtinKeyframesCss + "</style>"
            + "<style class=\"asset-vars\">" + buildAssetVarCss() + "</style>"
            + (entry.html || "");
        preview.appendChild(mount);

        // Caption row: slide number (mono, accent) left of the title.
        const caption = document.createElement("div");
        caption.className = "thumb__caption";
        const num = document.createElement("span");
        num.className = "thumb__num";
        num.textContent = String(index + 1);
        const label = document.createElement("span");
        label.className = "thumb__label";
        label.textContent = spec.labelOf(entry);
        label.addEventListener("dblclick", function (e) {
            // Edit the item's display name. stopPropagation so the
            // double-click does not also fire the switch-active click.
            e.preventDefault();
            e.stopPropagation();
            floatingEdit(label, spec.editInitial(entry), function (value) {
                const msg = { kind: spec.renameKind };
                msg[spec.clickField] = itemId;
                msg[spec.renameField] = value;
                window.__deck.send("Interaction", msg);
            });
        });

        caption.appendChild(num);
        caption.appendChild(label);
        btn.appendChild(preview);
        btn.appendChild(caption);

        // Per-slide delete affordance (slides only). stopPropagation on
        // mousedown/click so it never switches the active slide.
        if (spec.clickKind === "SlideThumbnailClicked") {
            const del = document.createElement("button");
            del.type = "button";
            del.className = "thumb__delete";
            del.title = "Delete slide";
            del.textContent = "×";
            del.addEventListener("mousedown", function (e) {
                e.stopPropagation();
            });
            del.addEventListener("click", function (e) {
                e.preventDefault();
                e.stopPropagation();
                window.__deck.send("Interaction", {
                    kind: "RemoveSlideRequested",
                    slide_id: itemId,
                });
            });
            btn.appendChild(del);
        }

        // Defer the scale to next frame so getBoundingClientRect on
        // .thumb__preview is reliable even before the row is in the DOM.
        window.requestAnimationFrame(function () {
            applyThumbnailScale(preview, mount);
        });

        btn.addEventListener("click", function () {
            // Clicking a thumbnail is an explicit slide-level selection.
            slideSelected = true;
            updateSlideFocusState();
            const msg = { kind: spec.clickKind };
            msg[spec.clickField] = itemId;
            window.__deck.send("Interaction", msg);
        });
        return btn;
    }

    // applyThumbnailScale
    // Inputs: the preview frame (fixed thumbnail-px size), the mount
    // element holding the shadow root.
    // Output: side-effect; sets transform: scale(...) on the mount so
    // a 1920×1080 slide fits inside the preview's actual pixel size.
    // Re-reads the preview size at call time so future CSS tweaks
    // (responsive width, zooming) keep working without code changes.
    function applyThumbnailScale(preview, mount) {
        if (!preview || !mount) {
            return;
        }
        const rect = preview.getBoundingClientRect();
        if (rect.width <= 0 || rect.height <= 0) {
            return;
        }
        const sx = rect.width / thumbnailDims.width;
        const sy = rect.height / thumbnailDims.height;
        const s = Math.min(sx, sy);
        mount.style.width = thumbnailDims.width + "px";
        mount.style.height = thumbnailDims.height + "px";
        mount.style.transform = "scale(" + s + ")";
    }

    // updateThumbnailHtml
    // Inputs: a slide id, the latest HTML for that slide, the theme CSS.
    // Output: side-effect; updates the in-memory cache and, if a
    // matching .thumb is on screen, re-renders just that thumbnail's
    // shadow root so its mini-preview reflects the new state.
    // Dataflow: cache write -> find the .thumb__mount with the matching
    // data-slide-id -> rewrite its shadow innerHTML -> re-apply scale.
    function updateThumbnailHtml(slideId, html, themeCss) {
        if (!slideId) {
            return;
        }
        thumbnailHtmlCache[slideId] = html || "";
        if (typeof themeCss === "string") {
            thumbnailThemeCss = themeCss;
        }
        const row = document.getElementById("thumbnail-row");
        if (!row) {
            return;
        }
        const mounts = row.querySelectorAll(".thumb__mount");
        for (let i = 0; i < mounts.length; i++) {
            if (mounts[i].dataset.slideId !== slideId) {
                continue;
            }
            const mount = mounts[i];
            if (mount.shadowRoot) {
                mount.shadowRoot.innerHTML =
                    "<style>" + thumbnailThemeCss + "</style>"
                    + "<style class=\"globals-css\">" + currentGlobalsCss + "</style>"
                    + "<style class=\"asset-vars\">" + buildAssetVarCss() + "</style>"
                    + (html || "");
            }
            const preview = mount.parentElement;
            window.requestAnimationFrame(function () {
                applyThumbnailScale(preview, mount);
            });
        }
    }

    // refreshThumbnailAssetVars
    // Inputs: none (reads assetBlobCache).
    // Output: side-effect; rewrites the .asset-vars <style> inside every
    // thumbnail's shadow root so newly-imported images appear in the
    // thumbnail previews without a full SlideListUpdate rebuild.
    function refreshThumbnailAssetVars() {
        const row = document.getElementById("thumbnail-row");
        if (!row) {
            return;
        }
        const css = buildAssetVarCss();
        const mounts = row.querySelectorAll(".thumb__mount");
        for (let i = 0; i < mounts.length; i++) {
            const sr = mounts[i].shadowRoot;
            if (!sr) {
                continue;
            }
            const styleEl = sr.querySelector("style.asset-vars");
            if (styleEl) {
                styleEl.textContent = css;
            }
        }
    }

    // highlightActiveThumbnail
    // Inputs: the currently active slide id.
    // Output: side-effect; sets aria-current="true" on the matching
    // thumbnail and clears it elsewhere. Also scrolls the active
    // thumbnail into view if it would otherwise be clipped.
    function highlightActiveThumbnail(slideId) {
        if (!slideId) {
            return;
        }
        activeSlideId = slideId;
        const row = document.getElementById("thumbnail-row");
        if (!row) {
            return;
        }
        const thumbs = row.querySelectorAll(".thumb");
        for (let i = 0; i < thumbs.length; i++) {
            if (thumbs[i].dataset.slideId === slideId) {
                thumbs[i].setAttribute("aria-current", "true");
            } else {
                thumbs[i].removeAttribute("aria-current");
            }
        }
        scrollActiveThumbnailIntoView();
    }

    function scrollActiveThumbnailIntoView() {
        const row = document.getElementById("thumbnail-row");
        if (!row || !activeSlideId) {
            return;
        }
        const active = row.querySelector(
            '.thumb[data-slide-id="' + cssEscape(activeSlideId) + '"]'
        );
        if (!active) {
            return;
        }
        const rRect = row.getBoundingClientRect();
        const aRect = active.getBoundingClientRect();
        if (aRect.left < rRect.left || aRect.right > rRect.right) {
            active.scrollIntoView({ behavior: "smooth", inline: "center", block: "nearest" });
        }
    }

    function cssEscape(value) {
        if (window.CSS && typeof window.CSS.escape === "function") {
            return window.CSS.escape(value);
        }
        return String(value).replace(/(["\\])/g, "\\$1");
    }

    // ---------- image drag-and-drop import ----------
    // Accepted image MIME prefixes. We only handle still images for now;
    // video/audio drops are ignored.
    const IMPORT_MAX_FILES = 32;

    // clientToSlideCoords
    // Inputs: a client x / y (window CSS pixels).
    // Output: { x, y } in slide coordinates (the 1920×1080 space), or
    // null when no slide is mounted. Uses the mounted .slide element's
    // on-screen rect as the origin and divides by the viewport scale.
    function clientToSlideCoords(clientX, clientY) {
        if (!currentShadow) {
            return null;
        }
        const slide = currentShadow.querySelector(".slide");
        if (!slide) {
            return null;
        }
        const rect = slide.getBoundingClientRect();
        const scale = getViewportScale() || 1;
        if (rect.width <= 0 || rect.height <= 0) {
            return null;
        }
        return {
            x: (clientX - rect.left) / scale,
            y: (clientY - rect.top) / scale,
        };
    }

    // onViewportDragOver
    // Inputs: a dragover DragEvent on the viewport container.
    // Output: side-effect; preventDefault (required to allow a drop) and
    // flag the viewport with a drop-active class when the drag carries
    // files. Returning without preventDefault would reject the drop.
    function onViewportDragOver(e) {
        if (!dragCarriesFiles(e)) {
            return;
        }
        e.preventDefault();
        if (e.dataTransfer) {
            e.dataTransfer.dropEffect = "copy";
        }
        const container = document.getElementById("viewport-container");
        if (container) {
            container.classList.add("viewport--drop-active");
        }
    }

    function onViewportDragLeave(e) {
        // Only clear when the pointer truly left the container, not when
        // moving between children.
        const container = document.getElementById("viewport-container");
        if (!container) {
            return;
        }
        if (e.relatedTarget && container.contains(e.relatedTarget)) {
            return;
        }
        container.classList.remove("viewport--drop-active");
    }

    // onViewportDrop
    // Inputs: a drop DragEvent on the viewport container.
    // Output: side-effect; for every image file in the transfer, reads
    // its bytes, decodes natural dimensions, and posts an AssetImported
    // event with the drop position mapped to slide coordinates.
    function onViewportDrop(e) {
        const container = document.getElementById("viewport-container");
        if (container) {
            container.classList.remove("viewport--drop-active");
        }
        if (!e.dataTransfer) {
            return;
        }
        const files = e.dataTransfer.files;
        if (!files || files.length === 0) {
            return;
        }
        e.preventDefault();
        const slidePos = clientToSlideCoords(e.clientX, e.clientY);
        const count = Math.min(files.length, IMPORT_MAX_FILES);
        for (let i = 0; i < count; i++) {
            const file = files[i];
            if (!file || !/^image\//.test(file.type)) {
                continue;
            }
            importImageFile(file, slidePos);
        }
    }

    // dragCarriesFiles
    // Inputs: a DragEvent.
    // Output: true when the drag's dataTransfer advertises files. Used
    // so we only intercept (and preventDefault) drags we can handle.
    function dragCarriesFiles(e) {
        if (!e.dataTransfer) {
            return false;
        }
        const types = e.dataTransfer.types;
        if (!types) {
            return false;
        }
        for (let i = 0; i < types.length; i++) {
            if (types[i] === "Files") {
                return true;
            }
        }
        return false;
    }

    // importImageFile
    // Inputs: a File (image/*), the slide-space drop position (or null).
    // Output: side-effect; reads bytes → base64, decodes pixel
    // dimensions, sends one AssetImported event. Asynchronous; failures
    // are logged and dropped.
    // Dataflow: FileReader → ArrayBuffer → base64 string in parallel
    // with an Image() decode for natural width/height; once both are
    // ready, dispatch.
    function importImageFile(file, slidePos, asSlideBackground, elementFill) {
        const reader = new FileReader();
        reader.onerror = function () {
            console.error("importImageFile: read failed for", file.name);
        };
        reader.onload = function () {
            const buffer = reader.result;
            if (!(buffer instanceof ArrayBuffer)) {
                return;
            }
            const base64 = arrayBufferToBase64(buffer);
            decodeImageDimensions(file, function (dims) {
                window.__deck.send("Interaction", {
                    kind: "AssetImported",
                    content_base64: base64,
                    original_filename: file.name || "image",
                    media_type: file.type || "application/octet-stream",
                    width: dims.width,
                    height: dims.height,
                    position: slidePos,
                    as_slide_background: !!asSlideBackground,
                    as_element_fill: elementFill || null,
                });
            });
        };
        reader.readAsArrayBuffer(file);
    }

    // decodeImageDimensions
    // Inputs: a File, a callback receiving { width, height }.
    // Output: side-effect; loads the file into an Image() to read its
    // natural pixel size. On failure (e.g. SVG without intrinsic size)
    // falls back to { 0, 0 } so the Rust side applies its default size.
    function decodeImageDimensions(file, cb) {
        const url = URL.createObjectURL(file);
        const img = new Image();
        img.onload = function () {
            const dims = { width: img.naturalWidth || 0, height: img.naturalHeight || 0 };
            URL.revokeObjectURL(url);
            cb(dims);
        };
        img.onerror = function () {
            URL.revokeObjectURL(url);
            cb({ width: 0, height: 0 });
        };
        img.src = url;
    }

    // arrayBufferToBase64
    // Inputs: an ArrayBuffer.
    // Output: a standard-alphabet base64 string. Chunked so very large
    // buffers don't blow the call-stack via String.fromCharCode.apply.
    function arrayBufferToBase64(buffer) {
        const bytes = new Uint8Array(buffer);
        const chunkSize = 0x8000;
        let binary = "";
        let offset = 0;
        let iter = 0;
        while (offset < bytes.length && iter < MAX_BATCH_ITER) {
            const end = Math.min(offset + chunkSize, bytes.length);
            const chunk = bytes.subarray(offset, end);
            binary += String.fromCharCode.apply(null, chunk);
            offset = end;
            iter += 1;
        }
        return window.btoa(binary);
    }

    // ---------- bootstrap ----------
    document.addEventListener("DOMContentLoaded", function () {
        // Wire mouse handlers on viewport / window so dragging continues
        // beyond the viewport's bounding box.
        const viewport = document.getElementById("viewport-container");
        if (viewport) {
            viewport.addEventListener("mousedown", onMouseDown);
            viewport.addEventListener("dblclick", onViewportDblClick);
            viewport.addEventListener("dragover", onViewportDragOver);
            viewport.addEventListener("dragleave", onViewportDragLeave);
            viewport.addEventListener("drop", onViewportDrop);
        }
        // Focus-region tracking: a mousedown anywhere in a region focuses it
        // (capture phase so it runs before the region's own handlers).
        const objectsPanel = document.getElementById("object-panel");
        if (objectsPanel) {
            objectsPanel.addEventListener("mousedown", function () {
                setFocusRegion("objects");
            }, true);
        }
        if (viewport) {
            viewport.addEventListener("mousedown", function () {
                setFocusRegion("preview");
            }, true);
        }
        const thumbRow = document.getElementById("thumbnail-row");
        if (thumbRow) {
            thumbRow.addEventListener("mousedown", function (e) {
                setFocusRegion("navigator");
                // A press on the strip's negative space (not on a thumbnail)
                // deselects: no slide highlight, and clear any element selection
                // so nothing is highlighted anywhere.
                const onThumb = e.target && e.target.closest && e.target.closest(".thumb");
                if (!onThumb) {
                    slideSelected = false;
                    updateSlideFocusState();
                    if (currentSelectionIds.length > 0) {
                        window.__deck.send("Interaction", {
                            kind: "SetSelectionFromPanel", element_ids: [],
                        });
                    }
                }
            }, true);
        }
        // Seed the initial ring on the default (preview) region.
        if (viewport) {
            viewport.classList.add("is-focused");
        }
        // Zoom controls.
        const zoomOutBtn = document.getElementById("zoom-out");
        if (zoomOutBtn) {
            zoomOutBtn.addEventListener("click", function () { zoomStep(-ZOOM_STEP); });
        }
        const zoomInBtn = document.getElementById("zoom-in");
        if (zoomInBtn) {
            zoomInBtn.addEventListener("click", function () { zoomStep(ZOOM_STEP); });
        }
        const zoomFitBtn = document.getElementById("zoom-fit");
        if (zoomFitBtn) {
            zoomFitBtn.addEventListener("click", setZoomFit);
        }
        const toolSelectBtn = document.getElementById("tool-select");
        if (toolSelectBtn) {
            toolSelectBtn.addEventListener("click", function () { setTool("select"); });
        }
        const toolHandBtn = document.getElementById("tool-hand");
        if (toolHandBtn) {
            toolHandBtn.addEventListener("click", function () { setTool("hand"); });
        }
        applyZoom();
        window.addEventListener("mousemove", onMouseMove);
        window.addEventListener("mouseup", onMouseUp);
        window.addEventListener("resize", function () {
            // In fit mode the scale tracks the pane width (applyZoom also
            // redraws rulers + guides).
            if (zoomMode === "fit") {
                applyZoom();
            } else {
                if (currentSelectionIds.length > 0) {
                    updateSelectionOverlay();
                }
                refreshRulers();
                renderRulerGuides();
                renderCanvasScrim();
            }
            positionDividers();
            refitThumbnails();
        });
        // Suppress the window-level default drop behavior (which would
        // navigate away to the dropped file) outside the viewport.
        window.addEventListener("dragover", function (e) {
            if (dragCarriesFiles(e)) {
                e.preventDefault();
            }
        });
        window.addEventListener("drop", function (e) {
            if (dragCarriesFiles(e)) {
                e.preventDefault();
            }
        });
        const gridBtn = document.getElementById("grid-toggle");
        if (gridBtn) {
            gridBtn.addEventListener("click", function () {
                setGridEnabled(!gridEnabled);
            });
        }
        bindCropInspectorControls();
        wireGuideInspector();
        buildInspectorSections();
        refreshInspector();
        wireObjectsToolbar();
        wireTableBox();
        wireShareMenu();
        wireLayoutEditorControls();
        wireAnimationsSection();
        wirePaneResizers();
        renderObjectPanel(null);
        // Capture the canvas floor + place dividers after first layout (the
        // window is at its default spawn size here).
        window.requestAnimationFrame(function () {
            captureCanvasMin();
            positionDividers();
            refitThumbnails();
            renderCanvasScrim();
        });
        window.__deck.send("Ready", null);
    });

    // ---------- animations panel ----------

    const SLIDE_TRANSITIONS = ["None", "Fade", "Push", "Dissolve", "Wipe", "Flip", "Cube"];

    const ANIM_TRIGGERS = [
        { value: "on_click", label: "On click" },
        { value: "with_previous", label: "With previous" },
        { value: "after_previous", label: "After previous" },
    ];
    const ANIM_EASINGS = [
        { label: "Out", token: "ease-out" },
        { label: "In-out", token: "ease-in-out" },
        { label: "Spring", token: "cubic-bezier(.34,1.56,.64,1)" },
        { label: "Linear", token: "linear" },
    ];
    const ANIM_DIRECTIONS = [
        { value: "top", label: "Up" },
        { value: "bottom", label: "Down" },
        { value: "left", label: "Left" },
        { value: "right", label: "Right" },
    ];
    const ANIM_CAT_ICON = {
        entrance: "→", emphasis: "★", exit: "←", property: "{ }",
    };

    // animSend / animAdd / animUpdate / animRemove / animReplace
    // Thin posters for the four animation IPC events. `animReplace` swaps an
    // entry's effect (the UpdateAnimation event cannot change the effect, so a
    // remove + add with the new catalog id is used; the entry re-appends).
    function animSend(kind, body) {
        body.kind = kind;
        window.__deck.send("Interaction", body);
    }
    function animAdd(catalogId, direction, elementId) {
        const el = elementId
            || (currentSelectionIds.length === 1 ? currentSelectionIds[0] : null);
        if (!el) {
            return;
        }
        animSend("AddAnimation", {
            element_id: el,
            catalog_id: catalogId,
            direction: direction || null,
        });
    }
    function animUpdate(animId, patch) {
        animSend("UpdateAnimation", Object.assign({ animation_id: animId }, patch));
    }
    function animRemove(animId) {
        animSend("RemoveAnimationRequested", { animation_id: animId });
    }
    // animReplace — swap an entry's effect. The effect cannot be patched in
    // place, so remove + re-add with the new catalog id. elementId keeps the
    // re-add on the right element (the slide controller edits any element, not
    // just the selection).
    function animReplace(animId, catalogId, direction, elementId) {
        animRemove(animId);
        animAdd(catalogId, direction, elementId);
    }

    // animDirectionOf
    // Output: the direction token for a directional keyframe (the trailing
    // top|bottom|left|right segment), else null.
    function animDirectionOf(entry) {
        const kf = entry && entry.keyframe;
        const m = kf && /-(top|bottom|left|right)$/.exec(kf);
        return m ? m[1] : null;
    }

    // catalogForEntry
    // Output: the catalog item backing an entry — by exact keyframe match, or
    // by prefix for a directional effect, or the property item. May be null.
    function catalogForEntry(entry) {
        if (entry.category === "property") {
            return animationCatalog.find(function (i) { return i.kind === "property"; }) || null;
        }
        const exact = animationCatalog.find(function (i) { return i.keyframe === entry.keyframe; });
        if (exact) {
            return exact;
        }
        const dir = animDirectionOf(entry);
        if (!dir) {
            return null;
        }
        const base = String(entry.keyframe).replace(/-(top|bottom|left|right)$/, "");
        return animationCatalog.find(function (i) {
            return i.directional && String(i.keyframe).replace(/-(top|bottom|left|right)$/, "") === base;
        }) || null;
    }

    // animEffectLabel / animTriggerLabel / animEffectSummary — bar text.
    function animEffectLabel(entry) {
        const item = catalogForEntry(entry);
        return item ? item.label : (entry.effect_id || "Effect");
    }
    function animTriggerLabel(entry) {
        const t = ANIM_TRIGGERS.find(function (x) { return x.value === entry.trigger; });
        return t ? t.label : entry.trigger;
    }
    function animEffectSummary(entry) {
        if (entry.category === "property") {
            const ts = entry.targets || [];
            if (ts.length === 0) {
                return "Property change";
            }
            const first = ts[0].property + " → " + ts[0].value;
            return ts.length > 1 ? (first + " +" + (ts.length - 1)) : first;
        }
        const dir = animDirectionOf(entry);
        return animEffectLabel(entry) + (dir ? " (" + dir + ")" : "");
    }

    // morphStateFromAttrs
    // Inputs: element id.
    // Output: {enabled, duration_ms, easing} read from the element's
    // data-morph-* attributes. Returns defaults when attributes are absent.
    function morphStateFromAttrs(elId) {
        const el = currentShadow ? currentShadow.querySelector('[data-element-id="' + String(elId).replace(/"/g, '\\"') + '"]') : null;
        if (!el) {
            return { enabled: false, duration_ms: 300, easing: "ease-in-out" };
        }
        const enabled = el.hasAttribute("data-morph-next");
        const duration_ms = parseInt(el.getAttribute("data-morph-dur") || "300", 10);
        const easing = el.getAttribute("data-morph-ease") || "ease-in-out";
        return { enabled, duration_ms, easing };
    }

    // renderMorphControl
    // Inputs: element id.
    // Output: a <div> with a checkbox, duration input, and easing select,
    // showing the current morph transition state. Duration/easing are hidden
    // unless the checkbox is checked.
    function renderMorphControl(elId) {
        const state = morphStateFromAttrs(elId);
        const wrapper = document.createElement("div");
        wrapper.className = "morph-control";
        const checkboxLabel = document.createElement("label");
        checkboxLabel.className = "morph-check-label";
        const checkbox = document.createElement("input");
        checkbox.type = "checkbox";
        checkbox.className = "morph-enabled";
        checkbox.checked = state.enabled;
        checkbox.dataset.elementId = elId;
        checkboxLabel.appendChild(checkbox);
        checkboxLabel.appendChild(document.createTextNode("Transition to next slide"));
        wrapper.appendChild(checkboxLabel);

        const row1 = document.createElement("div");
        row1.className = "morph-row";
        if (!state.enabled) {
            row1.hidden = true;
        }
        const durationLabel = document.createElement("label");
        durationLabel.textContent = "Duration (ms):";
        const durationInput = document.createElement("input");
        durationInput.type = "number";
        durationInput.className = "morph-duration";
        durationInput.min = "1";
        durationInput.value = state.duration_ms;
        durationInput.dataset.elementId = elId;
        row1.appendChild(durationLabel);
        row1.appendChild(durationInput);
        wrapper.appendChild(row1);

        const row2 = document.createElement("div");
        row2.className = "morph-row";
        if (!state.enabled) {
            row2.hidden = true;
        }
        const easingLabel = document.createElement("label");
        easingLabel.textContent = "Easing:";
        const easings = ["linear", "ease-in", "ease-out", "ease-in-out", "cubic-bezier(0.34, 1.56, 0.64, 1)"];
        const easingSelect = makeDropdown({
            label: "Easing",
            className: "morph-easing",
            options: easings.map(function (e) { return { value: e, label: e }; }),
            value: state.easing,
        });
        easingSelect.dataset.elementId = elId;
        row2.appendChild(easingLabel);
        row2.appendChild(easingSelect);
        wrapper.appendChild(row2);

        return wrapper;
    }

    // refreshAnimationsSection
    // Inputs: none (reads currentSelectionIds + slideAnimations).
    // Output: side-effect; shows the panel only for a single selection and
    // rebuilds the bar stack (one bar per entry of the selected element, in
    // timeline order) plus the count badge. Also renders the morph control
    // for the selected element.
    function refreshAnimationsSection() {
        const single = currentSelectionIds.length === 1;
        document.body.classList.toggle("has-single-selection", single);
        const bars = document.getElementById("anim-bars");
        const morphContainer = document.getElementById("morph-control-container");
        const count = document.getElementById("anim-count");
        if (!bars) {
            return;
        }
        const el = single ? currentSelectionIds[0] : null;
        const mine = el ? slideAnimations.filter(function (a) {
            return a.element_id === el;
        }) : [];
        bars.replaceChildren();
        for (let i = 0; i < mine.length && i < 4096; i++) {
            bars.appendChild(buildAnimBar(mine[i]));
        }
        if (count) {
            count.textContent = String(mine.length);
        }
        let container = morphContainer;
        if (!container) {
            const animSection = document.getElementById("animations-section");
            if (animSection) {
                container = document.createElement("div");
                container.id = "morph-control-container";
                animSection.appendChild(container);
            }
        }
        if (container && el) {
            container.replaceChildren();
            container.appendChild(renderMorphControl(el));
            wireMorphControl(el);
        } else if (container) {
            container.replaceChildren();
        }
    }

    // ---------- slide animation controller (styles pane) ----------
    // Shows the WHOLE slide's timeline, grouped into state changes: a run of
    // consecutive "with previous" entries plays together, so each group is one
    // rounded box. Items are draggable to reorder; dropping near the bottom of
    // an item joins that item's group ("with previous"), dropping higher makes
    // it a separate next step ("after previous").
    let sacDragId = null;

    // groupSlideAnimations — split the timeline into state-change groups.
    function groupSlideAnimations(list) {
        const groups = [];
        for (let i = 0; i < list.length; i++) {
            const a = list[i];
            if (a.trigger === "with_previous" && groups.length > 0) {
                groups[groups.length - 1].push(a);
            } else {
                groups.push([a]);
            }
        }
        return groups;
    }

    // renderSlideAnimations — rebuild #sac-groups from the slide timeline.
    function renderSlideAnimations() {
        const host = document.getElementById("sac-groups");
        if (!host) {
            return;
        }
        host.replaceChildren();
        if (!slideAnimations.length) {
            const empty = document.createElement("div");
            empty.className = "sac__empty";
            empty.textContent = "No animations on this slide.";
            host.appendChild(empty);
            return;
        }
        const groups = groupSlideAnimations(slideAnimations);
        for (let g = 0; g < groups.length && g < 1024; g++) {
            const box = document.createElement("div");
            box.className = "sac-group";
            for (let i = 0; i < groups[g].length && i < 1024; i++) {
                box.appendChild(buildSacItem(groups[g][i]));
            }
            host.appendChild(box);
        }
    }

    // buildSacItem — one timeline entry: a drag-handle head (element · effect ·
    // trigger, expand, remove) plus the shared expanded editor body.
    function buildSacItem(entry) {
        const item = document.createElement("div");
        item.className = "sac-item";
        item.dataset.animId = entry.animation_id;

        const head = document.createElement("div");
        head.className = "sac-item__head";
        head.draggable = true;
        head.dataset.animId = entry.animation_id;
        const icon = document.createElement("span");
        icon.className = "anim-bar__icon";
        icon.textContent = ANIM_CAT_ICON[entry.category] || "•";
        const label = document.createElement("span");
        label.className = "sac-item__label";
        label.textContent = entry.element_id + " · " + animEffectSummary(entry);
        const trig = document.createElement("span");
        trig.className = "anim-bar__trigger";
        trig.textContent = animTriggerLabel(entry);
        const chev = document.createElement("span");
        chev.className = "anim-bar__btn";
        chev.innerHTML = UNIT_CHEVRON;
        chev.classList.add("anim-bar__chev");
        if (!animExpanded[entry.animation_id]) {
            chev.classList.add("anim-bar__chev--collapsed");
        }
        const rm = document.createElement("button");
        rm.type = "button";
        rm.className = "anim-bar__btn";
        rm.dataset.sacRm = "1";
        rm.textContent = "×";
        rm.addEventListener("click", function (e) {
            e.stopPropagation();
            animRemove(entry.animation_id);
        });
        head.append(icon, label, trig, chev, rm);

        // Click anywhere on the head (except the remove button) toggles expand.
        head.addEventListener("click", function (e) {
            if (e.target.closest && e.target.closest("[data-sac-rm]")) {
                return;
            }
            animExpanded[entry.animation_id] = !animExpanded[entry.animation_id];
            renderSlideAnimations();
        });

        // Drag handle AND drop target are the SAME element (the head), exactly
        // like the object panel's rows. When they are split (handle = child,
        // target = parent) WebKit does not deliver the drop event.
        head.addEventListener("dragstart", function (e) {
            sacDragId = entry.animation_id;
            if (e.dataTransfer) {
                e.dataTransfer.setData("text/plain", entry.animation_id);
                e.dataTransfer.effectAllowed = "move";
            }
        });
        head.addEventListener("dragend", function () {
            sacDragId = null;
            clearSacDropHint();
        });
        head.addEventListener("dragover", onSacDragOver);
        head.addEventListener("drop", onSacDrop);
        head.addEventListener("dragleave", function () {
            delete head.dataset.sacDrop;
        });

        item.append(head);
        if (animExpanded[entry.animation_id]) {
            item.appendChild(buildAnimBody(entry));
        }
        return item;
    }

    // onSacDragOver — choose intent by cursor height in the target: the bottom
    // ~35% joins the target's group (with previous), higher makes it the next
    // separate step (after previous). Mirrors the hint on the item.
    function onSacDragOver(e) {
        if (!sacDragId) {
            return;
        }
        const head = e.currentTarget;
        if (head.dataset.animId === sacDragId) {
            return;
        }
        e.preventDefault();
        if (e.dataTransfer) {
            e.dataTransfer.dropEffect = "move";
        }
        const rect = head.getBoundingClientRect();
        const rel = (e.clientY - rect.top) / rect.height;
        head.dataset.sacDrop = (rel > 0.65) ? "join" : "after";
    }

    function clearSacDropHint() {
        const hinted = document.querySelectorAll("#sac-groups [data-sac-drop]");
        for (let i = 0; i < hinted.length; i++) {
            delete hinted[i].dataset.sacDrop;
        }
    }

    // onSacDrop — reorder the dragged entry to just after the target and set its
    // trigger from the drop zone, as one MoveAnimation (single undo).
    function onSacDrop(e) {
        if (!sacDragId) {
            return;
        }
        const head = e.currentTarget;
        const targetId = head.dataset.animId;
        const mode = head.dataset.sacDrop || "after";
        clearSacDropHint();
        if (!targetId || targetId === sacDragId) {
            sacDragId = null;
            return;
        }
        e.preventDefault();
        // Insertion index is computed in the list WITHOUT the dragged entry
        // (ReorderAnimation removes then inserts), placed right after the target.
        const ids = slideAnimations
            .map(function (a) { return a.animation_id; })
            .filter(function (id) { return id !== sacDragId; });
        const at = ids.indexOf(targetId);
        const newIndex = (at < 0) ? ids.length : (at + 1);
        const trigger = (mode === "join") ? "with_previous" : "after_previous";
        window.__deck.send("Interaction", {
            kind: "MoveAnimation",
            animation_id: sacDragId,
            new_index: newIndex,
            trigger: trigger,
        });
        sacDragId = null;
    }

    const FLEX_DIRS = [ { v: "row", t: "Row" }, { v: "column", t: "Column" } ];
    const FLEX_DISTS = [
        { v: "none", t: "Manual" }, { v: "start", t: "Start" }, { v: "center", t: "Center" },
        { v: "end", t: "End" }, { v: "space-between", t: "Between" },
        { v: "space-around", t: "Around" }, { v: "space-evenly", t: "Evenly" },
    ];
    const FLEX_ALIGNS = [
        { v: "none", t: "Manual" }, { v: "start", t: "Start" },
        { v: "center", t: "Center" }, { v: "end", t: "End" },
    ];

    // groupFlexState — read the selected group's current flex props from its DOM
    // data-attrs (set by the serializer). Returns null when not a single group.
    function groupFlexState() {
        if (currentSelectionIds.length !== 1) { return null; }
        const el = findElement(currentSelectionIds[0]);
        if (!el || el.dataset.elementType !== "group") { return null; }
        return {
            direction: el.dataset.flexDir || "row",
            distribution: el.dataset.flexDist || "none",
            alignment: el.dataset.flexAlign || "none",
        };
    }

    // flexSelect — a labelled dropdown that posts SetGroupLayout on change.
    function flexSelect(label, opts, current, field) {
        const dd = makeDropdown({
            label: label,
            options: opts.map(function (o) { return { value: o.v, label: o.t }; }),
            value: current,
            onChange: function (v) {
                if (currentSelectionIds.length !== 1) { return; }
                const body = { kind: "SetGroupLayout", element_id: currentSelectionIds[0],
                    direction: null, distribution: null, alignment: null };
                body[field] = v;
                window.__deck.send("Interaction", body);
            },
        });
        return animField(label, dd);
    }

    // refreshGroupFlexSection — rebuild #flex-controls from the selected group.
    function refreshGroupFlexSection() {
        const host = document.getElementById("flex-controls");
        if (!host) { return; }
        host.replaceChildren();
        const st = groupFlexState();
        if (!st) { return; }
        host.appendChild(flexSelect("Direction", FLEX_DIRS, st.direction, "direction"));
        host.appendChild(flexSelect("Distribute", FLEX_DISTS, st.distribution, "distribution"));
        host.appendChild(flexSelect("Align", FLEX_ALIGNS, st.alignment, "alignment"));
    }

    // buildAnimBar
    // Inputs: a SlideAnimationEntry.
    // Output: a collapsed-or-expanded bar element with its controls wired to
    // the animation IPC events.
    function buildAnimBar(entry) {
        const bar = document.createElement("div");
        bar.className = "anim-bar";
        bar.appendChild(buildAnimHead(entry));
        if (animExpanded[entry.animation_id]) {
            bar.appendChild(buildAnimBody(entry));
        }
        return bar;
    }

    // buildAnimHead — the always-visible collapsed row.
    function buildAnimHead(entry) {
        const head = document.createElement("div");
        head.className = "anim-bar__head";
        const icon = document.createElement("span");
        icon.className = "anim-bar__icon";
        icon.textContent = ANIM_CAT_ICON[entry.category] || "•";
        const label = document.createElement("span");
        label.className = "anim-bar__label";
        label.textContent = animEffectSummary(entry);
        const trig = document.createElement("span");
        trig.className = "anim-bar__trigger";
        trig.textContent = animTriggerLabel(entry);
        const chev = document.createElement("button");
        chev.type = "button";
        chev.className = "anim-bar__btn";
        chev.innerHTML = UNIT_CHEVRON;
        chev.classList.add("anim-bar__chev");
        if (!animExpanded[entry.animation_id]) {
            chev.classList.add("anim-bar__chev--collapsed");
        }
        chev.addEventListener("click", function () {
            animExpanded[entry.animation_id] = !animExpanded[entry.animation_id];
            refreshAnimationsSection();
        });
        const rm = document.createElement("button");
        rm.type = "button";
        rm.className = "anim-bar__btn";
        rm.textContent = "×";
        rm.addEventListener("click", function () { animRemove(entry.animation_id); });
        head.append(icon, label, trig, chev, rm);
        return head;
    }

    // buildAnimBody — the expanded controls (effect/properties/trigger/timing).
    function buildAnimBody(entry) {
        const body = document.createElement("div");
        body.className = "anim-bar__body";
        if (entry.category === "property") {
            body.appendChild(buildAnimPropRows(entry));
        } else {
            body.appendChild(buildAnimEffectRow(entry));
            const dir = animDirectionOf(entry);
            if (dir) {
                body.appendChild(buildAnimDirectionRow(entry, dir));
            }
        }
        body.appendChild(buildAnimTriggerRow(entry));
        body.appendChild(buildAnimTimingRow(entry));
        body.appendChild(buildAnimEasingRow(entry));
        if (entry.category === "emphasis") {
            body.appendChild(buildAnimIterationsRow(entry));
        }
        return body;
    }

    // animField — a labelled control row wrapper. A div (not a <label>) so a
    // dropdown-trigger button inside isn't double-toggled by label forwarding.
    function animField(labelText, control) {
        const row = document.createElement("div");
        row.className = "anim-bar__field";
        const span = document.createElement("span");
        span.textContent = labelText;
        row.append(span, control);
        return row;
    }

    // buildAnimEffectRow — swap the effect within its category (remove + add).
    function buildAnimEffectRow(entry) {
        const current = catalogForEntry(entry);
        const options = animationCatalog.filter(function (i) {
            return i.category === entry.category && i.kind === "named";
        }).map(function (item) {
            return { value: item.id, label: item.label };
        });
        const dd = makeDropdown({
            label: "Effect",
            options: options,
            value: current ? current.id : "",
            onChange: function (v) {
                const item = animationCatalog.find(function (i) { return i.id === v; });
                const dir = item && item.directional ? (animDirectionOf(entry) || "top") : null;
                animReplace(entry.animation_id, v, dir, entry.element_id);
            },
        });
        return animField("Effect", dd);
    }

    // buildAnimDirectionRow — direction picker for a directional effect.
    function buildAnimDirectionRow(entry, dir) {
        const item = catalogForEntry(entry);
        const dd = makeDropdown({
            label: "Direction",
            options: ANIM_DIRECTIONS.map(function (d) { return { value: d.value, label: d.label }; }),
            value: dir,
            onChange: function (v) {
                if (item) {
                    animReplace(entry.animation_id, item.id, v, entry.element_id);
                }
            },
        });
        return animField("Direction", dd);
    }

    // buildAnimTriggerRow — On click / With previous / After previous.
    function buildAnimTriggerRow(entry) {
        const dd = makeDropdown({
            label: "Trigger",
            options: ANIM_TRIGGERS.map(function (t) { return { value: t.value, label: t.label }; }),
            value: entry.trigger,
            onChange: function (v) { animUpdate(entry.animation_id, { trigger: v }); },
        });
        return animField("Trigger", dd);
    }

    // buildAnimTimingRow — duration + delay (ms), committed on change.
    function buildAnimTimingRow(entry) {
        const pair = document.createElement("div");
        pair.className = "anim-bar__pair";
        const dur = animNumberInput(entry.duration_ms, function (v) {
            animUpdate(entry.animation_id, { duration_ms: v });
        });
        const del = animNumberInput(entry.delay_ms, function (v) {
            animUpdate(entry.animation_id, { delay_ms: v });
        });
        pair.append(animField("Duration", dur), animField("Delay", del));
        const wrap = document.createElement("div");
        wrap.appendChild(pair);
        return wrap;
    }

    // animNumberInput — a non-negative integer input firing `onCommit(int)`.
    function animNumberInput(value, onCommit) {
        const input = document.createElement("input");
        input.type = "number";
        input.min = "0";
        input.value = String(value);
        input.addEventListener("change", function () {
            const n = Math.max(0, parseInt(input.value, 10) || 0);
            onCommit(n);
        });
        return input;
    }

    // buildAnimEasingRow — the 4 easing presets as a dropdown of CSS tokens.
    function buildAnimEasingRow(entry) {
        const dd = makeDropdown({
            label: "Easing",
            options: ANIM_EASINGS.map(function (e) { return { value: e.token, label: e.label }; }),
            value: entry.easing,
            onChange: function (v) { animUpdate(entry.animation_id, { easing: v }); },
        });
        return animField("Easing", dd);
    }

    // buildAnimIterationsRow — emphasis count, or ∞ toggle (Infinite).
    function buildAnimIterationsRow(entry) {
        const infinite = entry.iterations === "Infinite";
        const wrap = document.createElement("div");
        wrap.className = "anim-bar__pair";
        const num = document.createElement("input");
        num.type = "number";
        num.min = "1";
        num.value = infinite ? "1" : String((entry.iterations && entry.iterations.Count) || 1);
        num.disabled = infinite;
        num.addEventListener("change", function () {
            const n = Math.max(1, parseInt(num.value, 10) || 1);
            animUpdate(entry.animation_id, { iterations: { Count: n } });
        });
        const inf = document.createElement("label");
        inf.className = "anim-bar__field";
        const box = document.createElement("input");
        box.type = "checkbox";
        box.checked = infinite;
        box.addEventListener("change", function () {
            animUpdate(entry.animation_id, {
                iterations: box.checked ? "Infinite" : { Count: 1 },
            });
        });
        const tag = document.createElement("span");
        tag.textContent = "∞";
        tag.style.width = "auto";
        inf.append(box, tag);
        wrap.append(animField("Repeat", num), inf);
        const outer = document.createElement("div");
        outer.appendChild(wrap);
        return outer;
    }

    // buildAnimPropRows — the property → value editor for a Property entry.
    // Any change re-collects every row into one UpdateAnimation{targets}.
    function buildAnimPropRows(entry) {
        const box = document.createElement("div");
        box.style.display = "flex";
        box.style.flexDirection = "column";
        box.style.gap = "6px";
        const targets = (entry.targets && entry.targets.length)
            ? entry.targets.slice() : [{ property: "opacity", value: "1" }];
        const commit = function () {
            const rows = box.querySelectorAll(".anim-prop-row");
            const out = [];
            for (let i = 0; i < rows.length && i < 256; i++) {
                const ins = rows[i].querySelectorAll("input");
                const p = ins[0].value.trim();
                const v = ins[1].value.trim();
                if (p !== "") {
                    out.push({ property: p, value: v });
                }
            }
            if (out.length > 0) {
                animUpdate(entry.animation_id, { targets: out });
            }
        };
        for (let i = 0; i < targets.length && i < 256; i++) {
            box.appendChild(animPropRow(targets[i], commit));
        }
        const add = document.createElement("button");
        add.type = "button";
        add.className = "anim-prop-add";
        add.textContent = "+ property";
        add.addEventListener("click", function () {
            box.insertBefore(animPropRow({ property: "", value: "" }, commit), add);
        });
        box.appendChild(add);
        return box;
    }

    // animPropRow — one property/value pair with a remove button.
    function animPropRow(target, commit) {
        const row = document.createElement("div");
        row.className = "anim-prop-row";
        const prop = document.createElement("input");
        prop.placeholder = "property";
        prop.value = target.property || "";
        const val = document.createElement("input");
        val.placeholder = "value";
        val.value = target.value || "";
        prop.addEventListener("change", commit);
        val.addEventListener("change", commit);
        const rm = document.createElement("button");
        rm.type = "button";
        rm.className = "anim-bar__btn";
        rm.textContent = "×";
        rm.addEventListener("click", function () {
            row.remove();
            commit();
        });
        row.append(prop, val, rm);
        return row;
    }

    // wireMorphControl
    // Inputs: element id.
    // Output: side-effect; wires change handlers on the checkbox, duration,
    // and easing inputs to dispatch SetMorphTransitionRequested and toggle
    // row visibility.
    function wireMorphControl(elId) {
        const container = document.getElementById("morph-control-container");
        if (!container) {
            return;
        }
        const checkbox = container.querySelector(".morph-enabled");
        const durationInput = container.querySelector(".morph-duration");
        const easingSelect = container.querySelector(".morph-easing");
        const row1 = container.querySelector(".morph-row:nth-of-type(1)");
        const row2 = container.querySelector(".morph-row:nth-of-type(2)");

        if (checkbox) {
            checkbox.addEventListener("change", function (e) {
                const enabled = this.checked;
                if (row1) row1.hidden = !enabled;
                if (row2) row2.hidden = !enabled;
                const duration_ms = parseInt(durationInput ? durationInput.value : "300", 10);
                const easing = easingSelect ? easingSelect.value : "ease-in-out";
                window.__deck.send("Interaction", {
                    kind: "SetMorphTransitionRequested",
                    element_id: elId,
                    enabled: enabled,
                    duration_ms: duration_ms,
                    easing: easing,
                });
            });
        }
        if (durationInput) {
            durationInput.addEventListener("change", function (e) {
                const enabled = checkbox ? checkbox.checked : false;
                const duration_ms = parseInt(this.value || "300", 10);
                const easing = easingSelect ? easingSelect.value : "ease-in-out";
                if (enabled) {
                    window.__deck.send("Interaction", {
                        kind: "SetMorphTransitionRequested",
                        element_id: elId,
                        enabled: enabled,
                        duration_ms: duration_ms,
                        easing: easing,
                    });
                }
            });
        }
        if (easingSelect) {
            easingSelect.addEventListener("change", function (e) {
                const enabled = checkbox ? checkbox.checked : false;
                const duration_ms = parseInt(durationInput ? durationInput.value : "300", 10);
                const easing = this.value;
                if (enabled) {
                    window.__deck.send("Interaction", {
                        kind: "SetMorphTransitionRequested",
                        element_id: elId,
                        enabled: enabled,
                        duration_ms: duration_ms,
                        easing: easing,
                    });
                }
            });
        }
    }

    // wireAnimationsSection
    // Inputs: none (wires the static panel chrome once after load).
    // Output: side-effect; wires the Add menu (built from the catalog), the
    // Play preview button, and a document click-off that closes the menu.
    function wireAnimationsSection() {
        const addBtn = document.getElementById("anim-add-btn");
        const menu = document.getElementById("anim-add-menu");
        const play = document.getElementById("anim-play");
        if (addBtn && menu) {
            addBtn.addEventListener("click", function (e) {
                e.stopPropagation();
                if (menu.hidden) {
                    buildAnimAddMenu(menu);
                }
                menu.hidden = !menu.hidden;
            });
            menu.addEventListener("click", function (e) { e.stopPropagation(); });
            document.addEventListener("click", function () { menu.hidden = true; });
        }
        if (play) {
            play.addEventListener("click", playAnimPreview);
        }
    }

    // buildAnimAddMenu — fill the add dropdown from the catalog, grouped under
    // category headers. Selecting an item appends it to the selected element.
    function buildAnimAddMenu(menu) {
        menu.replaceChildren();
        const cats = ["entrance", "emphasis", "exit", "property"];
        for (let c = 0; c < cats.length; c++) {
            const items = animationCatalog.filter(function (i) { return i.category === cats[c]; });
            if (items.length === 0) {
                continue;
            }
            const h = document.createElement("div");
            h.className = "anim-menu__cat";
            h.textContent = cats[c];
            menu.appendChild(h);
            items.forEach(function (item) {
                const b = document.createElement("button");
                b.type = "button";
                b.className = "anim-menu__item";
                b.textContent = item.label;
                b.addEventListener("click", function () {
                    animAdd(item.id, item.directional ? "top" : null);
                    menu.hidden = true;
                });
                menu.appendChild(b);
            });
        }
    }

    // ---------- animations preview ----------

    // animFindEl — locate an element in the editor's mounted slide shadow root.
    function animFindEl(id) {
        if (!currentShadow || !id) {
            return null;
        }
        const safe = String(id).replace(/"/g, "\\\"");
        return currentShadow.querySelector('[data-element-id="' + safe + '"]');
    }

    // animIterCount — iterations as a positive pacing count (Infinite → 1).
    function animIterCount(iters) {
        if (iters === "Infinite") {
            return 1;
        }
        if (iters && typeof iters.Count === "number") {
            return Math.max(1, iters.Count);
        }
        return 1;
    }

    // animStepGroups — split the timeline into build steps: a new group opens
    // at each OnClick entry (leading non-OnClick entries form the first group).
    function animStepGroups(entries) {
        const groups = [];
        let cur = null;
        for (let i = 0; i < entries.length && i < 4096; i++) {
            if (entries[i].trigger === "on_click" || cur === null) {
                cur = [];
                groups.push(cur);
            }
            cur.push(entries[i]);
        }
        return groups;
    }

    // animPlayOne — play one entry on the editor canvas (keyframe or property
    // transition), mirroring present.js playback. `effDelay` is the resolved ms.
    function animPlayOne(entry, effDelay) {
        const el = animFindEl(entry.element_id);
        if (!el) {
            return;
        }
        if (entry.targets && entry.targets.length > 0) {
            el.style.opacity = "1";
            el.style.transition =
                "all " + entry.duration_ms + "ms " + entry.easing + " " + effDelay + "ms";
            window.requestAnimationFrame(function () {
                for (let i = 0; i < entry.targets.length && i < 256; i++) {
                    el.style.setProperty(entry.targets[i].property, entry.targets[i].value);
                }
            });
            return;
        }
        const iters = entry.iterations === "Infinite"
            ? "infinite" : String(animIterCount(entry.iterations));
        el.style.opacity = "1";
        el.style.animation = entry.keyframe + " " + entry.duration_ms + "ms "
            + entry.easing + " " + effDelay + "ms " + iters + " both";
        const endsHidden = entry.category === "exit";
        const onEnd = function () {
            el.style.animation = "none";
            el.style.opacity = endsHidden ? "0" : "1";
            el.removeEventListener("animationend", onEnd);
        };
        el.addEventListener("animationend", onEnd);
    }

    // animPlayGroup — play one build-step group with chained (after-previous)
    // delays; returns the step-finish time in ms (the longest entry).
    function animPlayGroup(group) {
        let priorSum = 0;
        let finish = 0;
        for (let i = 0; i < group.length && i < 4096; i++) {
            const e = group[i];
            const own = e.delay_ms || 0;
            const eff = e.trigger === "after_previous" ? priorSum + own : own;
            animPlayOne(e, eff);
            const span = eff + (e.duration_ms || 0) * animIterCount(e.iterations);
            if (span > finish) {
                finish = span;
            }
            priorSum += own + (e.duration_ms || 0) * animIterCount(e.iterations);
        }
        return finish;
    }

    // playAnimPreview
    // Inputs: none (reads slideAnimations + currentShadow).
    // Output: side-effect; previews the active slide's full build on the editor
    // canvas (step 0 then auto-advance through every group), then restores the
    // pre-preview inline styles. Re-entry is guarded by animPreviewActive.
    function playAnimPreview() {
        if (!currentShadow || animPreviewActive || slideAnimations.length === 0) {
            return;
        }
        animPreviewActive = true;
        const entries = slideAnimations.slice();
        const snap = {};
        const ids = [];
        for (let i = 0; i < entries.length && i < 4096; i++) {
            const id = entries[i].element_id;
            if (!(id in snap)) {
                const el = animFindEl(id);
                snap[id] = el ? el.style.cssText : null;
                ids.push(id);
            }
        }
        for (let i = 0; i < entries.length && i < 4096; i++) {
            if (entries[i].category === "entrance") {
                const el = animFindEl(entries[i].element_id);
                if (el) {
                    el.style.animation = "none";
                    el.style.opacity = "0";
                }
            }
        }
        const groups = animStepGroups(entries);
        let g = 0;
        const restore = function () {
            for (let i = 0; i < ids.length && i < 4096; i++) {
                const el = animFindEl(ids[i]);
                if (el) {
                    el.style.cssText = snap[ids[i]] || "";
                }
            }
            animPreviewActive = false;
        };
        const runNext = function () {
            if (g >= groups.length) {
                window.setTimeout(restore, 500);
                return;
            }
            const finish = animPlayGroup(groups[g]);
            g += 1;
            window.setTimeout(runNext, finish + 500);
        };
        runNext();
    }

    // ---------- toasts ----------
    // The live toast stack, newest first. Each entry: { el, timer, detail,
    // expanded, offClick, removed }. Capped at TOAST_MAX; a new toast beyond the
    // cap force-dismisses the oldest.
    const toasts = [];
    const TOAST_MAX = 3;
    const TOAST_TTL_MS = 3000;

    // showToast
    // Inputs: a short message (bold) and an optional longer detail. Output:
    // side-effect; drops a frosted toast in at the top of #toast-stack, starts
    // a 3s auto-dismiss timer, and evicts the oldest beyond TOAST_MAX.
    function showToast(message, detail) {
        const stack = document.getElementById("toast-stack");
        if (!stack || !message) {
            return;
        }
        const el = document.createElement("div");
        el.className = "toast";
        el.dataset.msg = String(message);
        const msgSpan = document.createElement("span");
        msgSpan.className = "toast__message";
        msgSpan.textContent = String(message);
        el.appendChild(msgSpan);
        stack.insertBefore(el, stack.firstChild);
        const entry = {
            el: el, timer: null, detail: detail || "", expanded: false,
            offClick: null, removed: false,
        };
        entry.timer = window.setTimeout(function () { dismissToast(entry); }, TOAST_TTL_MS);
        el.addEventListener("click", function (e) {
            e.stopPropagation();
            onToastClick(entry);
        });
        toasts.unshift(entry);
        while (toasts.length > TOAST_MAX) {
            dismissToast(toasts[toasts.length - 1]);
        }
    }

    // onToastClick
    // Inputs: a toast entry. Output: side-effect; expands to show the detail
    // (cancelling the auto-dismiss + arming a click-off listener) when a detail
    // exists and it is collapsed; otherwise dismisses.
    function onToastClick(entry) {
        if (!entry.detail || entry.expanded) {
            dismissToast(entry);
            return;
        }
        entry.expanded = true;
        if (entry.timer) {
            window.clearTimeout(entry.timer);
            entry.timer = null;
        }
        entry.el.classList.add("toast--expanded");
        entry.el.replaceChildren();
        const msgSpan = document.createElement("span");
        msgSpan.className = "toast__message";
        msgSpan.textContent = entry.el.dataset.msg || "";
        const detailSpan = document.createElement("span");
        detailSpan.className = "toast__detail";
        detailSpan.textContent = ": " + entry.detail;
        entry.el.appendChild(msgSpan);
        entry.el.appendChild(detailSpan);
        entry.offClick = function (e) {
            if (!entry.el.contains(e.target)) {
                dismissToast(entry);
            }
        };
        // Defer so the click that expanded it does not immediately dismiss.
        window.setTimeout(function () {
            document.addEventListener("click", entry.offClick, true);
        }, 0);
    }

    // dismissToast
    // Inputs: a toast entry. Output: side-effect; fades it out, removes it from
    // the DOM + the stack, and clears its timer / click-off listener.
    function dismissToast(entry) {
        if (!entry || entry.removed) {
            return;
        }
        entry.removed = true;
        if (entry.timer) {
            window.clearTimeout(entry.timer);
            entry.timer = null;
        }
        if (entry.offClick) {
            document.removeEventListener("click", entry.offClick, true);
            entry.offClick = null;
        }
        const idx = toasts.indexOf(entry);
        if (idx >= 0) {
            toasts.splice(idx, 1);
        }
        entry.el.classList.add("is-leaving");
        window.setTimeout(function () {
            if (entry.el.parentNode) {
                entry.el.parentNode.removeChild(entry.el);
            }
        }, 200);
    }

    // wireLayoutEditorControls
    // Inputs: none (reads the DOM after load).
    // Output: side-effect; wires the mode toggle (Slides ⇄ Layouts) and the
    // globals CSS textarea. The toggle flips to the opposite of the current
    // mode and asks the Rust side to switch; the actual data-mode flip
    // happens when the SetMode echo arrives. The textarea commits its value
    // on blur via GlobalsCssEditRequested.
    function wireLayoutEditorControls() {
        const toggle = document.getElementById("mode-toggle");
        if (toggle) {
            toggle.addEventListener("click", function () {
                const next = (currentMode === "layout") ? "slide" : "layout";
                window.__deck.send("Interaction", {
                    kind: "SetEditorMode",
                    mode: next,
                });
            });
        }
        const presentBtn = document.getElementById("present-btn");
        if (presentBtn) {
            presentBtn.addEventListener("click", function () {
                // Mirrors the Cmd+Return accelerator: start presenting from the
                // active slide. modifiers are irrelevant for a button click.
                window.__deck.send("Interaction", {
                    kind: "KeyPressed",
                    key: "present",
                    modifiers: { shift: false, ctrl: false, alt: false, meta: false },
                });
            });
        }
        const globals = document.getElementById("globals-css");
        if (globals) {
            globals.addEventListener("blur", function () {
                window.__deck.send("Interaction", {
                    kind: "GlobalsCssEditRequested",
                    new_css: globals.value,
                });
            });
        }
        const themeSave = document.getElementById("theme-save-btn");
        if (themeSave) {
            themeSave.addEventListener("click", function () {
                window.__deck.send("Interaction", { kind: "SaveThemeRequested" });
            });
        }
        const themeLoad = document.getElementById("theme-load-btn");
        if (themeLoad) {
            themeLoad.addEventListener("click", function () {
                window.__deck.send("Interaction", { kind: "LoadThemeRequested" });
            });
        }
    }

    // matchUndoRedoShortcut
    // Inputs: a KeyboardEvent.
    // Output: one of "undo", "redo", or null. Detects the canonical undo /
    // redo accelerators across platforms: Cmd+Z / Ctrl+Z for undo;
    // Cmd+Shift+Z / Ctrl+Shift+Z / Cmd+Y / Ctrl+Y for redo. Returns null
    // when the event does not match either.
    // Dataflow: lowercase the key, check meta-or-ctrl, branch on shift +
    // the specific letter. Pure function; no IPC, no DOM.
    // matchGridToggleShortcut
    // Inputs: a KeyboardEvent. Output: true for Cmd/Ctrl + ' (apostrophe),
    // the pixel-grid toggle accelerator. Pure; no DOM, no IPC.
    function matchGridToggleShortcut(e) {
        const meta = !!(e.metaKey || e.ctrlKey);
        return meta && !e.shiftKey && e.key === "'";
    }

    // updateSlideFocusState
    // Inputs: none (reads slideSelected). Output: side-effect; sets
    // data-slide-focus on the thumbnail row. True only when the slide is
    // explicitly selected (thumbnail click) — CSS then shows the accent border
    // on the current thumbnail. Negative-space clicks clear the flag, so
    // nothing is highlighted.
    function updateSlideFocusState() {
        const row = document.getElementById("thumbnail-row");
        if (row) {
            row.dataset.slideFocus = slideSelected ? "true" : "false";
        }
    }

    // setFocusRegion
    // Inputs: "objects" | "preview" | "navigator". Output: side-effect; updates
    // focusRegion and moves the faint .is-focused ring to that pane. No-op when
    // unchanged or unknown.
    function setFocusRegion(region) {
        if (!FOCUS_CONTAINERS[region] || region === focusRegion) {
            return;
        }
        focusRegion = region;
        let key;
        for (key in FOCUS_CONTAINERS) {
            if (Object.prototype.hasOwnProperty.call(FOCUS_CONTAINERS, key)) {
                const el = document.getElementById(FOCUS_CONTAINERS[key]);
                if (el) {
                    el.classList.toggle("is-focused", key === region);
                }
            }
        }
    }

    // setGridEnabled
    // Inputs: a boolean. Output: side-effect; updates module state and the
    // toolbar button's pressed styling. Single source both the shortcut and
    // the button call so UI and state never drift.
    function setGridEnabled(on) {
        gridEnabled = !!on;
        const btn = document.getElementById("grid-toggle");
        if (btn) {
            btn.setAttribute("aria-pressed", gridEnabled ? "true" : "false");
            btn.classList.toggle("is-active", gridEnabled);
        }
    }

    // matchClipboardShortcut
    // Inputs: a KeyboardEvent. Output: "copy" | "cut" | "paste" for
    // Cmd/Ctrl + C / X / V (no Shift), else null. Pure; no DOM, no IPC.
    function matchClipboardShortcut(e) {
        const meta = !!(e.metaKey || e.ctrlKey);
        if (!meta || e.shiftKey) {
            return null;
        }
        const key = (typeof e.key === "string") ? e.key.toLowerCase() : "";
        if (key === "c") { return "copy"; }
        if (key === "x") { return "cut"; }
        if (key === "v") { return "paste"; }
        return null;
    }

    function matchUndoRedoShortcut(e) {
        const meta = !!(e.metaKey || e.ctrlKey);
        if (!meta) {
            return null;
        }
        const key = (typeof e.key === "string") ? e.key.toLowerCase() : "";
        if (key === "z" && !e.shiftKey) {
            return "undo";
        }
        if (key === "z" && e.shiftKey) {
            return "redo";
        }
        if (key === "y") {
            return "redo";
        }
        return null;
    }

    // matchFileShortcut
    // Inputs: a KeyboardEvent.
    // Output: one of "new_deck", "open_deck", "save_deck", "save_as_deck",
    // or null. Stage 7 File-menu accelerators: Cmd/Ctrl+N (New), +O (Open),
    // +S (Save), +Shift+S (Save As). Sibling of matchUndoRedoShortcut and
    // structured the same way so future accelerator groups can follow the
    // pattern.
    // Dataflow: bail unless Cmd/Ctrl is held; lowercase the key; branch
    // on the specific letter and on the Shift state for Save vs Save As.
    function matchFileShortcut(e) {
        const meta = !!(e.metaKey || e.ctrlKey);
        if (!meta) {
            return null;
        }
        const key = (typeof e.key === "string") ? e.key.toLowerCase() : "";
        if (key === "n" && !e.shiftKey) {
            return "new_deck";
        }
        if (key === "o" && !e.shiftKey) {
            return "open_deck";
        }
        if (key === "s" && e.shiftKey) {
            return "save_as_deck";
        }
        if (key === "e" && e.shiftKey) {
            return "export_html";
        }
        if (key === "p" && e.shiftKey) {
            return "export_pdf";
        }
        if (key === "s" && !e.shiftKey) {
            return "save_deck";
        }
        return null;
    }

    // matchPresentShortcut
    // Inputs: a KeyboardEvent.
    // Output: true when the event is the Present accelerator (Cmd+Return /
    // Ctrl+Return, no Shift). Starts presentation from the active slide. The
    // Shift variant is reserved for a future "from the beginning".
    function matchPresentShortcut(e) {
        const meta = !!(e.metaKey || e.ctrlKey);
        return meta && !e.shiftKey && e.key === "Enter";
    }

    // matchAddSlideShortcut
    // Inputs: a KeyboardEvent.
    // Output: true when the event is the New-Slide accelerator
    // (Cmd+Shift+N / Ctrl+Shift+N), false otherwise. Distinct from the
    // File "New deck" accelerator (Cmd/Ctrl+N, no Shift) handled by
    // matchFileShortcut, so the two never collide.
    // Dataflow: require Cmd/Ctrl AND Shift; lowercase the key; match "n".
    function matchAddSlideShortcut(e) {
        const meta = !!(e.metaKey || e.ctrlKey);
        if (!meta || !e.shiftKey) {
            return false;
        }
        const key = (typeof e.key === "string") ? e.key.toLowerCase() : "";
        return key === "n";
    }

    // sendSyntheticKey
    // Inputs: a logical key name ("undo" / "redo" / ...), the original
    // KeyboardEvent (for its modifiers).
    // Output: side-effect; posts an Interaction(KeyPressed) IPC envelope
    // with the synthetic key name. The Rust interpreter pattern-matches on
    // the synthetic name rather than re-decoding modifier combinations.
    function sendSyntheticKey(syntheticKey, e) {
        window.__deck.send("Interaction", {
            kind: "KeyPressed",
            key: syntheticKey,
            modifiers: readModifiers(e),
        });
    }

    // clickAddButton: trigger a toolbar add button by selector, so keyboard
    // shortcuts reuse the existing click wiring (InsertElementRequested / the
    // image file picker). No-op if the button is absent.
    function clickAddButton(selector) {
        const b = document.querySelector(selector);
        if (b) {
            b.click();
        }
    }

    // isEditableFocus
    // Inputs: none (reads document.activeElement).
    // Output: true when the focused element is a text-editing control —
    // <input> of a text-y type, <textarea>, or anything with the
    // contenteditable attribute set. Used to suppress global hotkey
    // forwarding so the inspector and rename inputs receive their
    // keystrokes normally.
    function isEditableFocus() {
        const el = document.activeElement;
        if (!el) {
            return false;
        }
        const tag = (el.tagName || "").toUpperCase();
        if (tag === "TEXTAREA") {
            return true;
        }
        if (tag === "INPUT") {
            // Non-text input types (button, checkbox, range...) should
            // not be treated as editable for our purposes.
            const type = (el.type || "text").toLowerCase();
            const nonText = [
                "button", "submit", "reset", "checkbox", "radio",
                "range", "file", "image", "color",
            ];
            return nonText.indexOf(type) < 0;
        }
        if (el.isContentEditable) {
            return true;
        }
        return false;
    }

    // Keys whose default behavior is dangerous inside a WKWebView /
    // WebView2 host (history navigation on Backspace, tab focus
    // hijacking, etc.) so we always preventDefault them, even when we
    // are about to forward them as Interaction events. Keys NOT in this
    // set are left to bubble: the native key path is now safe because the
    // app installs an empty NSApp main menu (see src/main.rs), so wry's
    // keyDown forwarding no longer null-derefs on unhandled keys.
    const ALWAYS_PREVENT_DEFAULT_KEYS = new Set([
        "Backspace", "Delete", "Tab",
    ]);

    // Keyboard interactions: forwarded for the Stage 4 debug shortcut, the
    // Stage 6 undo/redo accelerators, the Stage 7 file accelerators, the
    // Stage 9 delete shortcut, and any future hot-keys. Each shortcut
    // branch fires first and preventDefault()s so the OS-level browser/
    // webview default (e.g. Cmd+S "save page", Backspace "navigate
    // back") does not also run. While an editable element has focus we
    // suppress unmodified key forwarding so the user can type freely;
    // accelerator-keyed shortcuts (Cmd/Ctrl-…) still fire so that
    // Save / Undo / Redo remain available everywhere.
    document.addEventListener("keydown", function (e) {
        if (cropState) {
            if (e.key === "Enter") { e.preventDefault(); commitCrop(); return; }
            if (e.key === "Escape") { e.preventDefault(); cancelCrop(); return; }
            return;
        }
        if (e.key === "Escape" && focusChain.length > 0 && !textEditState) {
            focusChain = [];
            tableCellSel = null;
            updateSelectionOverlay();
            return;
        }
        // Delete the selected guide (before the element-delete path forwards it).
        if (selectedGuideId !== null && !isEditableFocus()
            && (e.key === "Backspace" || e.key === "Delete")) {
            e.preventDefault();
            deleteGuide(selectedGuideId);
            return;
        }
        // Cmd/Ctrl+R toggles rulers (preventDefault: the host would reload).
        if ((e.metaKey || e.ctrlKey) && !e.shiftKey && !e.altKey
            && typeof e.key === "string" && e.key.toLowerCase() === "r") {
            e.preventDefault();
            toggleRulers();
            return;
        }
        // Zoom: Cmd/Ctrl with +/- steps by 10%, Cmd/Ctrl+0 fits to pane.
        if ((e.metaKey || e.ctrlKey) && !e.shiftKey && !e.altKey) {
            const k = typeof e.key === "string" ? e.key : "";
            if (k === "=" || k === "+" || e.code === "NumpadAdd") {
                e.preventDefault();
                zoomStep(ZOOM_STEP);
                return;
            }
            if (k === "-" || k === "_" || e.code === "NumpadSubtract") {
                e.preventDefault();
                zoomStep(-ZOOM_STEP);
                return;
            }
            if (k === "0" || e.code === "Numpad0") {
                e.preventDefault();
                setZoomFit();
                return;
            }
        }
        // Tool shortcuts: V = select, H = hand (no modifiers, not while typing).
        if (!isEditableFocus() && !e.metaKey && !e.ctrlKey && !e.altKey
            && typeof e.key === "string") {
            const k = e.key.toLowerCase();
            if (k === "v") { e.preventDefault(); setTool("select"); return; }
            if (k === "h") { e.preventDefault(); setTool("hand"); return; }
        }
        // Add-element shortcuts (not while typing). They click the matching
        // toolbar button so behavior — including the image file picker — is
        // identical to a click. T text · ⇧S shape · ⇧I image · ⇧C code ·
        // ⇧T table · ⌘G group.
        if (!isEditableFocus() && !e.altKey && typeof e.key === "string") {
            const lk = e.key.toLowerCase();
            if ((e.metaKey || e.ctrlKey) && !e.shiftKey && lk === "g") {
                e.preventDefault();
                clickAddButton('.objects__add[data-element-type="group"]');
                return;
            }
            if (!e.metaKey && !e.ctrlKey) {
                if (!e.shiftKey && lk === "t") {
                    e.preventDefault();
                    clickAddButton('.objects__add[data-element-type="text"]');
                    return;
                }
                if (e.shiftKey && lk === "s") {
                    e.preventDefault();
                    clickAddButton('.objects__add[data-element-type="shape"]');
                    return;
                }
                if (e.shiftKey && lk === "i") {
                    e.preventDefault();
                    clickAddButton("#tool-add-image");
                    return;
                }
                if (e.shiftKey && lk === "c") {
                    e.preventDefault();
                    clickAddButton('.objects__add[data-element-type="embed"]');
                    return;
                }
                if (e.shiftKey && lk === "t") {
                    e.preventDefault();
                    clickAddButton('.objects__add[data-element-type="table"]');
                    return;
                }
            }
        }
        if (matchGridToggleShortcut(e)) {
            e.preventDefault();
            setGridEnabled(!gridEnabled);
            return;
        }
        if (matchAddSlideShortcut(e)) {
            e.preventDefault();
            window.__deck.send("Interaction", { kind: "AddSlideRequested" });
            return;
        }
        if (matchPresentShortcut(e)) {
            e.preventDefault();
            sendSyntheticKey("present", e);
            return;
        }
        const fileAction = matchFileShortcut(e);
        if (fileAction) {
            e.preventDefault();
            sendSyntheticKey(fileAction, e);
            return;
        }
        const shortcut = matchUndoRedoShortcut(e);
        if (shortcut) {
            e.preventDefault();
            sendSyntheticKey(shortcut, e);
            return;
        }
        if (isEditableFocus()) {
            // Let the focused control handle its own keystroke. We do
            // not preventDefault — inputs need their default behavior.
            return;
        }
        // Cmd+Shift+G / Ctrl+Shift+G — group the current multi-selection.
        if ((e.metaKey || e.ctrlKey) && e.shiftKey
                && typeof e.key === "string" && e.key.toLowerCase() === "g") {
            e.preventDefault();
            if (currentSelectionIds.length >= 2) {
                window.__deck.send("Interaction", {
                    kind: "GroupSelectionRequested",
                    element_ids: currentSelectionIds.slice(),
                });
            }
            return;
        }
        // Element/slide clipboard accelerators. Placed AFTER the editable
        // bail so Cmd+C/V inside a text edit or input keeps native behavior.
        const clip = matchClipboardShortcut(e);
        if (clip) {
            e.preventDefault();
            if (clip === "paste") {
                window.__deck.send("Interaction", { kind: "PasteRequested" });
            } else {
                const scope = (focusRegion === "navigator") ? "Slide" : "Elements";
                const kind = (clip === "copy") ? "CopyRequested" : "CutRequested";
                window.__deck.send("Interaction", { kind: kind, scope: scope });
            }
            return;
        }
        // Delete in the navigator removes the active slide; elsewhere it falls
        // through to the element-delete path below.
        if ((e.key === "Delete" || e.key === "Backspace")
                && focusRegion === "navigator" && activeSlideId) {
            e.preventDefault();
            window.__deck.send("Interaction", {
                kind: "RemoveSlideRequested",
                slide_id: activeSlideId,
            });
            return;
        }
        // Arrow keys (not while typing): nudge the current element selection by
        // 1px, or — with nothing selected and the canvas/navigator focused —
        // step the active slide left/right. Decided here (like the clipboard
        // scope) since the JS side owns focus region + selection.
        const ARROW_DELTA = {
            ArrowLeft: [-1, 0], ArrowRight: [1, 0],
            ArrowUp: [0, -1], ArrowDown: [0, 1],
        };
        if (ARROW_DELTA[e.key]) {
            if (currentSelectionIds.length > 0) {
                e.preventDefault();
                const d = ARROW_DELTA[e.key];
                window.__deck.send("Interaction", {
                    kind: "NudgeSelectionRequested", dx: d[0], dy: d[1],
                });
                return;
            }
            const horizontal = e.key === "ArrowLeft" || e.key === "ArrowRight";
            const navFocus = focusRegion === "preview" || focusRegion === "navigator";
            if (horizontal && navFocus) {
                e.preventDefault();
                window.__deck.send("Interaction", {
                    kind: "NavigateSlideRequested", forward: e.key === "ArrowRight",
                });
                return;
            }
        }
        const isSingleChar = (typeof e.key === "string" && e.key.length === 1);
        const recognizedControl = [
            "ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown",
            "Enter", "Escape", "Tab", "Backspace", "Delete",
        ].indexOf(e.key) >= 0;
        if (!isSingleChar && !recognizedControl) {
            return;
        }
        if (ALWAYS_PREVENT_DEFAULT_KEYS.has(e.key)) {
            e.preventDefault();
        }
        window.__deck.send("Interaction", {
            kind: "KeyPressed",
            key: e.key,
            modifiers: readModifiers(e),
        });
    });
})();
