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
    let dragState = null;
    let pendingDrag = null;
    let dragRafScheduled = false;
    let currentSelectionIds = [];
    // pendingDragEnd: when mouseup fires, we keep the optimistic
    // transform on the dragged element so there is no visible flash
    // between the transform clearing and the absolute-position patch
    // landing. The transform is removed inside applyOnePatch the moment
    // a SetStyle(left|top) patch for the same element arrives. A safety
    // timeout clears it anyway after PENDING_TRANSFORM_TIMEOUT_MS so the
    // element is never stuck if the patch never arrives.
    let pendingDragEnd = null;
    // textEditState: non-null while a text element is being edited inline
    // (double-click). Holds the element id, the contenteditable DOM node,
    // its text at edit-start (for cancel), and the keydown/blur listeners
    // so they can be detached on finish. See beginTextEdit / finishTextEdit.
    let textEditState = null;

    const DRAG_THRESHOLD = 3;
    const MAX_BATCH_ITER = 100000;
    const PENDING_TRANSFORM_TIMEOUT_MS = 200;
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
    // The active slide's animation timeline (from SlideAnimationsUpdate); the
    // inspector's Appear/Disappear toggles filter this by the selected id.
    let slideAnimations = [];
    // The active slide's inspector data (from SlideInspectorUpdate); rendered in
    // the Slide box when nothing is selected in slide mode.
    let slideInspectorData = null;

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
            + slideHtml;
        viewport.replaceChildren(host);
        currentShadow = shadow;
        currentSlideHost = host;
        assetVarStyleEl = shadow.getElementById("asset-vars");
        refreshAssetVarStyle();
        // Selection from the previous slide does not transfer.
        currentSelectionIds = [];
        clearSelectionOverlay();
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
        assetBlobCache[payload.asset_id] = { url: url, media_type: mediaType };
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
                if (pendingDragEnd &&
                        pendingDragEnd.element_id === patch.element_id &&
                        (patch.property === "left" || patch.property === "top")) {
                    pendingDragEnd.element.style.removeProperty("transform");
                    pendingDragEnd = null;
                }
                break;
            case "RemoveStyle":
                el.style.removeProperty(patch.property);
                break;
            case "SetText":
                el.textContent = patch.text;
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
    const SELECTION_OUTSET_PX = 3;
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
        if (!currentShadow || !currentSlideHost) {
            return;
        }
        if (currentSelectionIds.length === 0) {
            return;
        }
        const overlayRect = overlay.getBoundingClientRect();
        const showHandles = currentSelectionIds.length === 1;
        for (let i = 0; i < currentSelectionIds.length; i++) {
            const id = currentSelectionIds[i];
            const safe = (window.CSS && window.CSS.escape) ? window.CSS.escape(id) : id;
            const el = currentShadow.querySelector('[data-element-id="' + safe + '"]');
            if (!el) {
                continue;
            }
            const rect = el.getBoundingClientRect();
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
            box.style.border = "2px solid var(--theme-accent, #0066ff)";
            box.style.boxShadow = "0 0 0 1px rgba(255,255,255,0.7)";
            box.style.pointerEvents = "none";
            box.style.boxSizing = "border-box";
            overlay.appendChild(box);

            if (showHandles) {
                for (let h = 0; h < SELECTION_HANDLES.length; h++) {
                    const spec = SELECTION_HANDLES[h];
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
        return window.__snap.__build_targets(rects);
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

    // ---------- interaction capture ----------
    // findInteractionTarget
    // Inputs: a DOM Event.
    // Output: the first ancestor along composedPath carrying
    // data-element-id, or null. Skips elements without the attribute and
    // stops at the slide host (so background clicks return null).
    function findInteractionTarget(e) {
        const path = (typeof e.composedPath === "function") ? e.composedPath() : [];
        for (let i = 0; i < path.length; i++) {
            const node = path[i];
            if (!node || !node.dataset) {
                continue;
            }
            if (node.classList && node.classList.contains("slide-host")) {
                return null;
            }
            if (node.dataset.elementId) {
                return node;
            }
        }
        return null;
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
        if (!target || target.dataset.elementType !== "text") {
            return;
        }
        e.preventDefault();
        beginTextEdit(target);
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
        const slideHost = e.target.closest && e.target.closest(".slide-host");
        if (!slideHost) {
            return;
        }
        const target = findInteractionTarget(e);
        if (!target) {
            window.__deck.send("Interaction", {
                kind: "BackgroundClicked",
                position: { x: e.clientX, y: e.clientY },
            });
            return;
        }
        const elementId = target.dataset.elementId;
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
    function onMouseMove(e) {
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
            window.__deck.send("Interaction", {
                kind: "ElementDragStarted",
                element_id: dragState.element_id,
                position: { x: dragState.start.x, y: dragState.start.y },
            });
        }
        const scale = getViewportScale();
        const snapped = snappedDragDelta(dx / scale, dy / scale, scale, e, true);
        optimisticTransform(dragState.target, snapped.x, snapped.y);
        reportDragThrottled(dragState.element_id, { x: snapped.x, y: snapped.y }, { x: e.clientX, y: e.clientY });
    }

    // snappedDragDelta
    // Inputs: the raw slide-space delta (dxSlide, dySlide), the viewport
    // scale, the source MouseEvent (for the Cmd suppress flag), and whether to
    // draw guides. Output: { x, y } snapped slide-space delta. Feeds the raw
    // target rect through the snap engine and returns the corrected delta;
    // renders guides as a side-effect when draw is true. Falls back to the raw
    // delta when no snapshot exists.
    function snappedDragDelta(dxSlide, dySlide, scale, e, draw) {
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
            suppress: !!e.metaKey,
        });
        if (draw) {
            renderGuides(out.guides);
        }
        return {
            x: out.rect.x - dragState.baseRect.x,
            y: out.rect.y - dragState.baseRect.y,
        };
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
        if (!dragState) {
            return;
        }
        if (dragState.started) {
            const dx = e.clientX - dragState.start.x;
            const dy = e.clientY - dragState.start.y;
            const scale = getViewportScale();
            const snapped = snappedDragDelta(dx / scale, dy / scale, scale, e, false);
            window.__deck.send("Interaction", {
                kind: "ElementDragEnded",
                element_id: dragState.element_id,
                delta: { x: snapped.x, y: snapped.y },
            });
            // Hold the optimistic transform so there is no flash between
            // transform clear and the absolute-position patch landing.
            // applyOnePatch clears it when SetStyle(left|top) arrives.
            pendingDragEnd = {
                element: dragState.target,
                element_id: dragState.element_id,
            };
            (function (captured) {
                setTimeout(function () {
                    if (pendingDragEnd && pendingDragEnd.element_id === captured.element_id) {
                        captured.element.style.removeProperty("transform");
                        pendingDragEnd = null;
                    }
                }, PENDING_TRANSFORM_TIMEOUT_MS);
            }(pendingDragEnd));
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
            updateSelectionOverlay();
            refreshInspector();
            updateObjectPanelSelection();
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
            // Keep the globals textarea in sync with the committed value.
            if (payload && typeof payload.globals_css === "string") {
                currentGlobalsCss = payload.globals_css;
                const ta = document.getElementById("globals-css");
                if (ta && document.activeElement !== ta) {
                    ta.value = payload.globals_css;
                }
            }
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
        },
        SlideAnimationsUpdate: function (payload) {
            slideAnimations = (payload && payload.entries) || [];
            refreshAnimationsSection();
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
            showNotice((payload && payload.message) || "");
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

        resizeState = {
            target: target,
            elementId: elementId,
            handle: handle.dataset.handle,
            startMouse: { x: e.clientX, y: e.clientY },
            startRect: startRect,
            aspect: startRect.w / startRect.h,
            savedTransform: target.style.transform || "",
            snapTargets: buildSnapTargets(elementId),
        };
        // Clear any optimistic transform from a prior drag so the
        // resize math operates on the inline left/top/width/height.
        target.style.transform = "none";
        document.body.style.userSelect = "none";

        window.__deck.send("Interaction", {
            kind: "ElementResizeStarted",
            element_id: elementId,
            handle: resizeHandleToRustEnum(handle.dataset.handle),
            position: { x: e.clientX, y: e.clientY },
        });

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
    function onResizeMouseMove(e) {
        if (!resizeState) {
            return;
        }
        const scale = getViewportScale();
        const dx = (e.clientX - resizeState.startMouse.x) / scale;
        const dy = (e.clientY - resizeState.startMouse.y) / scale;
        const rect = snappedResizeRect(computeResizeRect(
            resizeState, dx, dy, !!e.shiftKey, !!e.altKey,
        ), e, scale, true);
        applyOptimisticRect(resizeState.target, rect);
        updateSelectionOverlay();
        scheduleResizeReport(rect, e);
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
        const rect = snappedResizeRect(computeResizeRect(
            resizeState, dx, dy, !!e.shiftKey, !!e.altKey,
        ), e, scale, false);
        applyOptimisticRect(resizeState.target, rect);
        window.__deck.send("Interaction", {
            kind: "ElementResizeEnded",
            element_id: resizeState.elementId,
            new_position: { x: rect.x, y: rect.y },
            new_size: { width: rect.w, height: rect.h },
        });
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
    const BOXY_TYPES = ["text", "image", "shape", "media"];
    const TEXT_TYPES = ["text"];

    const INSPECTOR_SECTIONS = [
        {
            id: "position",
            label: "Position",
            appliesTo: ALL_TYPES,
            fields: [
                { prop: "x", label: "X", kind: "number", suffix: "px" },
                { prop: "y", label: "Y", kind: "number", suffix: "px" },
            ],
        },
        {
            id: "size",
            label: "Size",
            appliesTo: ALL_TYPES,
            fields: [
                { prop: "width", label: "Width", kind: "number", suffix: "px" },
                { prop: "height", label: "Height", kind: "number", suffix: "px" },
            ],
        },
        {
            id: "transform",
            label: "Transform",
            appliesTo: ALL_TYPES,
            fields: [
                { prop: "rotation", label: "Rotation", kind: "rotation-deg", suffix: "°" },
                { prop: "opacity", label: "Opacity", kind: "number", suffix: "" },
            ],
        },
        {
            id: "appearance",
            label: "Appearance",
            appliesTo: BOXY_TYPES,
            fields: [
                { prop: "background-color", label: "Fill", kind: "css", full: true },
                { prop: "border", label: "Border", kind: "css", full: true },
                { prop: "border-radius", label: "Border Radius", kind: "css", suffix: "" },
                { prop: "box-shadow", label: "Shadow", kind: "css", full: true },
            ],
        },
        {
            id: "typography",
            label: "Typography",
            appliesTo: TEXT_TYPES,
            fields: [
                { prop: "font-family", label: "Font", kind: "css", full: true },
                { prop: "font-size", label: "Size", kind: "number", suffix: "px" },
                { prop: "font-weight", label: "Weight", kind: "number", suffix: "" },
                { prop: "color", label: "Color", kind: "color" },
                {
                    prop: "text-align", label: "Justify", kind: "select",
                    options: [
                        { value: "left", label: "Left" },
                        { value: "center", label: "Center" },
                        { value: "right", label: "Right" },
                        { value: "justify", label: "Full" },
                    ],
                },
                {
                    prop: "justify-content", label: "Vertical", kind: "select",
                    options: [
                        { value: "flex-start", label: "Top" },
                        { value: "center", label: "Middle" },
                        { value: "flex-end", label: "Bottom" },
                    ],
                },
                { prop: "line-height", label: "Line Height", kind: "number", suffix: "" },
                { prop: "letter-spacing", label: "Letter Spacing", kind: "number", suffix: "px" },
            ],
        },
    ];

    // Cache of input elements keyed by property name so refreshInspector
    // can fill them in O(1) and the change handlers can be wired once.
    const inspectorInputs = {};
    // Set of properties that the current pending PropertyChanged round
    // trip is waiting on. Used to suppress refresh-from-DOM clobbering
    // the user's in-flight typing.
    const inspectorPending = new Set();

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
        for (let i = 0; i < def.fields.length; i++) {
            body.appendChild(buildField(def.fields[i]));
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
        if (!field.readonly) {
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

    // buildFieldControl
    // Inputs: a field definition.
    // Output: the bare control element for the field's kind — a <select> for
    // "select", a color swatch for "color", otherwise a text <input> (the
    // Enter-to-blur affordance is wired for text inputs only).
    function buildFieldControl(field) {
        if (field.kind === "select") {
            const sel = document.createElement("select");
            sel.className = "inspector__input inspector__select";
            const opts = field.options || [];
            for (let i = 0; i < opts.length; i++) {
                const o = document.createElement("option");
                o.value = opts[i].value;
                o.textContent = opts[i].label;
                sel.appendChild(o);
            }
            return sel;
        }
        if (field.kind === "color") {
            const swatch = document.createElement("input");
            swatch.type = "color";
            swatch.className = "inspector__input inspector__swatch";
            return swatch;
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
        const wire = encodeForWire(kind, raw);
        if (wire === null) {
            // Invalid input — restore the displayed value from DOM and bail.
            refreshInspector();
            return;
        }
        sendPropertyChanged(prop, wire);
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
        // CSS strings, select tokens, and color hexes pass through verbatim.
        if (kind === "css" || kind === "select" || kind === "color") {
            return String(raw);
        }
        const trimmed = String(raw).trim();
        if (trimmed === "") {
            return ""; // empty → clear (only meaningful for CSS in Rust; numeric returns "" → Nothing).
        }
        // Strip optional unit suffixes ("px", "°") so the user can type
        // "200px" or "45°" and it still parses.
        const numeric = trimmed.replace(/(px|deg|rad|°|%)\s*$/i, "").trim();
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
        sendPropertyChanged(prop, value);
        keyInput.value = "";
        valInput.value = "";
    }

    // sendPropertyChanged
    // Inputs: a property name, a wire-formatted value.
    // Output: side-effect; posts a PropertyChanged IPC envelope for the
    // currently-selected element (no-op if no single selection).
    function sendPropertyChanged(prop, value) {
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
        // No selection: in slide mode the pane targets the slide (Slide box);
        // otherwise (layout mode) just blank the element controls.
        if (currentSelectionIds.length === 0) {
            clearInspectorInputs();
            const slideMode = currentMode === "slide";
            subtitle.textContent = slideMode ? "Slide" : "No selection";
            setSlideBoxVisible(slideMode);
            setElementInspectorVisible(false, null);
            if (slideMode) {
                renderSlideBox();
            }
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
        toggleDisplay("inspector-custom", show);
        toggleDisplay("animations-section", show);
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
            el.style.display = show ? "flex" : "none";
        }
    }

    // renderSlideBox
    // Inputs: none (reads slideInspectorData).
    // Output: side-effect; fills the Slide box controls from the latest
    // SlideInspectorUpdate and (once) wires their commit handlers.
    function renderSlideBox() {
        wireSlideBox();
        const data = slideInspectorData;
        const bg = document.getElementById("slide-bg");
        const layout = document.getElementById("slide-layout");
        const title = document.getElementById("slide-title");
        const notes = document.getElementById("slide-notes");
        if (bg && document.activeElement !== bg) {
            bg.value = isHexColor((data && data.background) || "") ? data.background : "#000000";
        }
        if (title && document.activeElement !== title) {
            title.value = (data && data.title) || "";
        }
        if (notes && document.activeElement !== notes) {
            notes.value = (data && data.notes) || "";
        }
        if (layout && document.activeElement !== layout) {
            const layouts = (data && data.layouts) || [];
            layout.replaceChildren();
            for (let i = 0; i < layouts.length; i++) {
                const o = document.createElement("option");
                o.value = layouts[i].id;
                o.textContent = layouts[i].name || layouts[i].id;
                layout.appendChild(o);
            }
            layout.value = (data && data.layout_id) || "";
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
        const bg = document.getElementById("slide-bg");
        if (bg) {
            bg.addEventListener("change", function () {
                window.__deck.send("Interaction", {
                    kind: "SetSlideBackgroundRequested", background: bg.value,
                });
            });
        }
        const layout = document.getElementById("slide-layout");
        if (layout) {
            layout.addEventListener("change", function () {
                window.__deck.send("Interaction", {
                    kind: "SetSlideLayoutRequested", layout_id: layout.value,
                });
            });
        }
        const title = document.getElementById("slide-title");
        if (title) {
            title.addEventListener("blur", function () {
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
            "background-color", "border", "border-radius", "box-shadow",
            // Typography props whose inspector name IS the CSS property, set
            // verbatim (select tokens, color hex, family string, unitless nums).
            "font-family", "font-weight", "color", "text-align",
            "justify-content", "line-height",
        ];
        for (let i = 0; i < cssOnly.length; i++) {
            const key = cssOnly[i];
            setIfNotPending(key, decls[key] || "");
        }
        // Typography numeric-with-px fields strip the unit for the number input.
        setIfNotPending("font-size", stripPx(decls["font-size"]));
        setIfNotPending("letter-spacing", stripPx(decls["letter-spacing"]));
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
        if (node.element_type === "group") {
            disclosure.textContent = "▾";
            disclosure.dataset.role = "disclosure";
        } else {
            disclosure.classList.add("objects__disclosure--empty");
            disclosure.textContent = "•";
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
            window.__deck.send("Interaction", { kind: spec.addKind });
        });
        return btn;
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
        const idx = document.createElement("span");
        idx.className = "thumb__index";
        idx.textContent = String(index + 1);
        preview.appendChild(idx);

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

        btn.appendChild(preview);
        btn.appendChild(label);

        // Defer the scale to next frame so getBoundingClientRect on
        // .thumb__preview is reliable even before the row is in the DOM.
        window.requestAnimationFrame(function () {
            applyThumbnailScale(preview, mount);
        });

        btn.addEventListener("click", function () {
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
    function importImageFile(file, slidePos) {
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
        window.addEventListener("mousemove", onMouseMove);
        window.addEventListener("mouseup", onMouseUp);
        window.addEventListener("resize", function () {
            if (currentSelectionIds.length > 0) {
                updateSelectionOverlay();
            }
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
        buildInspectorSections();
        refreshInspector();
        wireObjectsToolbar();
        wireLayoutEditorControls();
        wireAnimationsSection();
        renderObjectPanel(null);
        window.__deck.send("Ready", null);
    });

    // refreshAnimationsSection
    // Inputs: none (reads currentSelectionIds + slideAnimations).
    // Output: side-effect; shows the Animations group only for a single
    // selection and sets the Appear/Disappear checkboxes from the active
    // slide's timeline.
    function refreshAnimationsSection() {
        const single = currentSelectionIds.length === 1;
        document.body.classList.toggle("has-single-selection", single);
        const appear = document.getElementById("anim-appear");
        const disappear = document.getElementById("anim-disappear");
        if (!appear || !disappear) {
            return;
        }
        const el = single ? currentSelectionIds[0] : null;
        const hasCat = function (cat) {
            return !!el && slideAnimations.some(function (a) {
                return a.element_id === el && a.category === cat;
            });
        };
        appear.checked = hasCat("entrance");
        disappear.checked = hasCat("exit");
    }

    // wireAnimationsSection
    // Inputs: none (wires the two checkboxes once after load).
    // Output: side-effect; a change on either toggle posts SetElementAnimation
    // for the single selected element. Rust maps enabled→Insert / disabled→
    // Remove and broadcasts SlideAnimationsUpdate, which repaints the boxes.
    function wireAnimationsSection() {
        const send = function (category, enabled) {
            if (currentSelectionIds.length !== 1) {
                return;
            }
            window.__deck.send("Interaction", {
                kind: "SetElementAnimation",
                element_id: currentSelectionIds[0],
                category: category,
                enabled: enabled,
            });
        };
        const a = document.getElementById("anim-appear");
        const d = document.getElementById("anim-disappear");
        if (a) {
            a.addEventListener("change", function () { send("entrance", a.checked); });
        }
        if (d) {
            d.addEventListener("change", function () { send("exit", d.checked); });
        }
    }

    // showNotice
    // Inputs: a message string.
    // Output: side-effect; flashes the #notice-banner for ~2.5s.
    let noticeTimer = null;
    function showNotice(message) {
        const banner = document.getElementById("notice-banner");
        if (!banner || !message) {
            return;
        }
        banner.textContent = message;
        banner.classList.add("show");
        if (noticeTimer) {
            window.clearTimeout(noticeTimer);
        }
        noticeTimer = window.setTimeout(function () {
            banner.classList.remove("show");
        }, 2500);
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
