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

    const DRAG_THRESHOLD = 3;
    const MAX_BATCH_ITER = 100000;
    const PENDING_TRANSFORM_TIMEOUT_MS = 200;
    // assetBlobCache: asset_id -> { url: blob URL, media_type } so the
    // slide's CSS custom properties can resolve to image URLs.
    // assetVarStyleEl: the <style> node injected into the active shadow
    // root that maps :host { --asset-<id>: url(<blob-url>); }.
    const assetBlobCache = Object.create(null);
    let assetVarStyleEl = null;

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
    function mountSlide(slideId, slideHtml, themeCss) {
        const viewport = document.getElementById("viewport");
        if (!viewport) {
            console.error("mountSlide: #viewport not found");
            return;
        }
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
            window.__deck.send("Interaction", {
                kind: "ElementDragStarted",
                element_id: dragState.element_id,
                position: { x: dragState.start.x, y: dragState.start.y },
            });
        }
        const scale = getViewportScale();
        const dxSlide = dx / scale;
        const dySlide = dy / scale;
        optimisticTransform(dragState.target, dxSlide, dySlide);
        reportDragThrottled(dragState.element_id, { x: dxSlide, y: dySlide }, { x: e.clientX, y: e.clientY });
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
            window.__deck.send("Interaction", {
                kind: "ElementDragEnded",
                element_id: dragState.element_id,
                delta: { x: dx / scale, y: dy / scale },
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
            mountSlide(payload.slide_id, payload.slide_html, payload.theme_css);
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
        },
        ObjectTreeUpdate: function (payload) {
            renderObjectPanel(payload);
        },
        SlideListUpdate: function (payload) {
            renderThumbnailRow(payload);
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
        const rect = computeResizeRect(
            resizeState, dx, dy, !!e.shiftKey, !!e.altKey,
        );
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
        const rect = computeResizeRect(
            resizeState, dx, dy, !!e.shiftKey, !!e.altKey,
        );
        applyOptimisticRect(resizeState.target, rect);
        window.__deck.send("Interaction", {
            kind: "ElementResizeEnded",
            element_id: resizeState.elementId,
            new_position: { x: rect.x, y: rect.y },
            new_size: { width: rect.w, height: rect.h },
        });
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
    const INSPECTOR_SECTIONS = [
        {
            id: "position",
            label: "Position",
            fields: [
                { prop: "x", label: "X", kind: "number", suffix: "px" },
                { prop: "y", label: "Y", kind: "number", suffix: "px" },
            ],
        },
        {
            id: "size",
            label: "Size",
            fields: [
                { prop: "width", label: "Width", kind: "number", suffix: "px" },
                { prop: "height", label: "Height", kind: "number", suffix: "px" },
            ],
        },
        {
            id: "transform",
            label: "Transform",
            fields: [
                { prop: "rotation", label: "Rotation", kind: "rotation-deg", suffix: "°" },
                { prop: "opacity", label: "Opacity", kind: "number", suffix: "" },
            ],
        },
        {
            id: "appearance",
            label: "Appearance",
            fields: [
                { prop: "background-color", label: "Fill", kind: "css", full: true },
                { prop: "border", label: "Border", kind: "css", full: true },
                { prop: "border-radius", label: "Border Radius", kind: "css", suffix: "" },
                { prop: "box-shadow", label: "Shadow", kind: "css", full: true },
            ],
        },
        {
            id: "arrangement",
            label: "Arrangement",
            fields: [
                { prop: "z-index", label: "Z-Index", kind: "css", readonly: true },
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
    // Output: a labelled <input> DOM node, registered in inspectorInputs
    // and wired with the change handler.
    function buildField(field) {
        const wrap = document.createElement("div");
        wrap.className = "inspector__field";
        if (field.full) {
            wrap.classList.add("inspector__field--full");
        }
        const label = document.createElement("label");
        label.className = "inspector__field-label";
        label.textContent = field.label + (field.suffix ? " (" + field.suffix.trim() + ")" : "");
        const input = document.createElement("input");
        input.className = "inspector__input";
        input.dataset.prop = field.prop;
        input.dataset.kind = field.kind;
        input.spellcheck = false;
        if (field.readonly) {
            input.readOnly = true;
            input.tabIndex = -1;
        } else {
            input.addEventListener("change", onInspectorFieldCommit);
            input.addEventListener("keydown", function (e) {
                if (e.key === "Enter") {
                    e.preventDefault();
                    input.blur();
                }
            });
        }
        const id = "inspector-input-" + field.prop.replace(/[^a-z0-9]/gi, "-");
        input.id = id;
        label.setAttribute("for", id);
        wrap.appendChild(label);
        wrap.appendChild(input);
        inspectorInputs[field.prop] = input;
        return wrap;
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
        if (kind === "css") {
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
        if (currentSelectionIds.length === 0) {
            subtitle.textContent = "No selection";
            clearInspectorInputs();
            return;
        }
        if (currentSelectionIds.length > 1) {
            subtitle.textContent = currentSelectionIds.length + " selected";
            clearInspectorInputs();
            return;
        }
        const id = currentSelectionIds[0];
        subtitle.textContent = id;
        const el = findElement(id);
        if (!el) {
            clearInspectorInputs();
            return;
        }
        const decls = parseStyleAttr(el.getAttribute("style") || "");
        populateInspector(decls);
        inspectorPending.clear();
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
        ];
        for (let i = 0; i < cssOnly.length; i++) {
            const key = cssOnly[i];
            setIfNotPending(key, decls[key] || "");
        }
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
    // Inputs: an ObjectTreeNode (id, element_type, display_name, children),
    // the depth (used purely so future styling can target nesting level).
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
        label.textContent = node.display_name;
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
            // Double-click is a faster path to rename than long-click;
            // useful on trackpads where long-click is awkward.
            e.preventDefault();
            beginRename(label, node.id);
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
                beginRename(longClickAnchor.labelNode, longClickAnchor.elementId);
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

    // beginRename
    // Inputs: the label DOM element, the target element id.
    // Output: side-effect; swaps the label span for an inline <input>,
    // focus + select all, wires commit/cancel handlers.
    function beginRename(labelNode, elementId) {
        if (!labelNode || labelNode.querySelector("input")) {
            return; // already editing
        }
        const original = labelNode.textContent || "";
        const input = document.createElement("input");
        input.className = "objects__label-edit";
        input.type = "text";
        input.value = original;
        input.spellcheck = false;
        labelNode.replaceChildren(input);
        input.focus();
        input.select();

        const commit = function () {
            const value = input.value;
            labelNode.textContent = value || elementId;
            window.__deck.send("Interaction", {
                kind: "RenameElementRequested",
                element_id: elementId,
                new_name: value,
            });
        };
        const cancel = function () {
            labelNode.textContent = original;
        };
        let resolved = false;
        input.addEventListener("blur", function () {
            if (resolved) {
                return;
            }
            resolved = true;
            commit();
        });
        input.addEventListener("keydown", function (e) {
            if (e.key === "Enter") {
                e.preventDefault();
                resolved = true;
                commit();
                input.blur();
            } else if (e.key === "Escape") {
                e.preventDefault();
                resolved = true;
                cancel();
                input.blur();
            }
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

    // renderThumbnailRow
    // Inputs: a SlideListData payload.
    // Output: side-effect; rebuilds #thumbnail-row from scratch with
    // one .thumb per slide, each mounting the slide HTML inside its
    // own shadow root at a scaled-down size.
    function renderThumbnailRow(payload) {
        const row = document.getElementById("thumbnail-row");
        if (!row) {
            return;
        }
        thumbnailDims = {
            width: (payload && payload.width) || 1920,
            height: (payload && payload.height) || 1080,
        };
        thumbnailThemeCss = (payload && payload.theme_css) || "";
        // Seed / refresh the HTML cache from the payload.
        if (payload && Array.isArray(payload.slides)) {
            for (let i = 0; i < payload.slides.length; i++) {
                const entry = payload.slides[i];
                if (entry && entry.slide_id) {
                    thumbnailHtmlCache[entry.slide_id] = entry.html || "";
                }
            }
        }
        row.replaceChildren();
        const slides = (payload && Array.isArray(payload.slides))
            ? payload.slides
            : [];
        if (slides.length === 0) {
            const empty = document.createElement("div");
            empty.className = "thumb__empty";
            empty.textContent = "No slides.";
            row.appendChild(empty);
            return;
        }
        const active = (payload && payload.active_slide_id) || activeSlideId;
        if (active) {
            activeSlideId = active;
        }
        for (let i = 0; i < slides.length; i++) {
            row.appendChild(buildThumbnail(slides[i], i, active));
        }
        scrollActiveThumbnailIntoView();
    }

    // buildThumbnail
    // Inputs: a SlideListEntry, the slide's display index (1-based for
    // the badge), and the active slide id.
    // Output: a <button>.thumb DOM node fully wired (click → switches
    // active slide via SlideThumbnailClicked).
    function buildThumbnail(entry, index, activeId) {
        const btn = document.createElement("button");
        btn.type = "button";
        btn.className = "thumb";
        btn.dataset.slideId = entry.slide_id;
        if (entry.slide_id === activeId) {
            btn.setAttribute("aria-current", "true");
        }
        btn.title = entry.title || entry.slide_id;

        const preview = document.createElement("div");
        preview.className = "thumb__preview";
        const idx = document.createElement("span");
        idx.className = "thumb__index";
        idx.textContent = String(index + 1);
        preview.appendChild(idx);

        const mount = document.createElement("div");
        mount.className = "thumb__mount";
        mount.dataset.slideId = entry.slide_id;
        // Mount inside its own shadow root so theme CSS is scoped. The
        // asset-vars block resolves any image elements to blob URLs,
        // mirroring the viewport mount.
        const shadow = mount.attachShadow({ mode: "open" });
        shadow.innerHTML = "<style>" + thumbnailThemeCss + "</style>"
            + "<style class=\"asset-vars\">" + buildAssetVarCss() + "</style>"
            + (entry.html || "");
        preview.appendChild(mount);

        const label = document.createElement("span");
        label.className = "thumb__label";
        label.textContent = entry.title || entry.slide_id;

        btn.appendChild(preview);
        btn.appendChild(label);

        // Defer the scale to next frame so getBoundingClientRect on
        // .thumb__preview is reliable even before the row is in the DOM.
        window.requestAnimationFrame(function () {
            applyThumbnailScale(preview, mount);
        });

        btn.addEventListener("click", function () {
            window.__deck.send("Interaction", {
                kind: "SlideThumbnailClicked",
                slide_id: entry.slide_id,
            });
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
        buildInspectorSections();
        refreshInspector();
        wireObjectsToolbar();
        renderObjectPanel(null);
        window.__deck.send("Ready", null);
    });

    // matchUndoRedoShortcut
    // Inputs: a KeyboardEvent.
    // Output: one of "undo", "redo", or null. Detects the canonical undo /
    // redo accelerators across platforms: Cmd+Z / Ctrl+Z for undo;
    // Cmd+Shift+Z / Ctrl+Shift+Z / Cmd+Y / Ctrl+Y for redo. Returns null
    // when the event does not match either.
    // Dataflow: lowercase the key, check meta-or-ctrl, branch on shift +
    // the specific letter. Pure function; no IPC, no DOM.
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
    // are about to forward them as Interaction events.
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
