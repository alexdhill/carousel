(function () {
    "use strict";

    let currentShadow = null;
    let currentSlideHost = null;

    let gridEnabled = false;

    let focusRegion = "preview";
    const FOCUS_CONTAINERS = {
        objects: "object-panel",
        preview: "viewport-container",
        navigator: "thumbnail-row",
    };

    let cropState = null;
    let cropPan = null;
    let cropResize = null;
    let dragState = null;
    let pendingDrag = null;
    let dragRafScheduled = false;

    let marquee = null;
    let currentSelectionIds = [];

    let slideSelected = false;

    let zoomMode = "fit";
    let zoomManualPct = 100;
    const ZOOM_MIN = 50;
    const ZOOM_MAX = 250;
    const ZOOM_STEP = 10;

    let panX = 0;
    let panY = 0;

    let activeTool = "select";
    let panSession = null;

    let rulersOn = false;
    const RULER = 18;
    let guideOwn = [];
    let guideInherited = [];
    let selectedGuideId = null;
    let guideDragSession = null;
    let guideSeq = 0;

    let focusChain = [];

    function elementChain(node) {
        const out = [];
        let n = node;
        let guard = 0;
        while (n && guard < 1000) {
            guard += 1;
            if (n.classList && n.classList.contains("slide-host")) {
                break;
            }
            if (n.dataset && n.dataset.elementId) {
                out.push(n);
            }
            n = n.parentElement || (n.getRootNode && n.getRootNode().host);
        }
        return out;
    }

    const pendingDragEnds = Object.create(null);

    let textEditState = null;

    const DRAG_THRESHOLD = 3;
    const MAX_BATCH_ITER = 100000;
    const PENDING_TRANSFORM_TIMEOUT_MS = 200;

    let canvasMinW = 0;
    let canvasMinH = 0;
    const PANE_MIN = { objects: 240, inspector: 300, thumbs: 160 };
    const PANE_MAX = { objects: 750, inspector: 750, thumbs: 500 };
    let paneDragSession = null;

    const assetBlobCache = Object.create(null);
    let assetVarStyleEl = null;

    let currentGlobalsCss = "";

    let currentMode = "slide";

    let builtinKeyframesCss = "";

    let animationCatalog = [];

    let slideAnimations = [];

    const animExpanded = {};

    let animPreviewActive = false;

    let slideInspectorData = null;

    let layoutBgData = null;

    function newId() {
        if (window.crypto && typeof window.crypto.randomUUID === "function") {
            return window.crypto.randomUUID();
        }
        return "js_" + Math.random().toString(36).slice(2) + Date.now().toString(36);
    }

    function mountSlide(slideId, slideHtml, themeCss, globalsCss) {
        const prevSlideId = currentSlideHost ? currentSlideHost.dataset.slideId : null;
        if (typeof globalsCss === "string") {
            currentGlobalsCss = globalsCss;
        }
        const viewport = document.getElementById("viewport");
        if (!viewport) {
            console.error("mountSlide: #viewport not found");
            return;
        }

        textEditState = null;
        const host = document.createElement("div");
        host.className = "slide-host";
        host.dataset.slideId = slideId;
        const shadow = host.attachShadow({ mode: "open" });

        shadow.innerHTML =
            "<style>" +
            themeCss +
            "</style>" +
            '<style id="globals-css">' +
            currentGlobalsCss +
            "</style>" +
            '<style id="anim-kf">' +
            builtinKeyframesCss +
            "</style>" +
            '<style id="asset-vars"></style>' +

            '<style id="edit-overflow">.slide{overflow:visible}</style>' +
            slideHtml;
        viewport.replaceChildren(host);
        currentShadow = shadow;
        currentSlideHost = host;
        assetVarStyleEl = shadow.getElementById("asset-vars");
        refreshAssetVarStyle();

        if (prevSlideId === slideId && currentSelectionIds.length > 0) {
            updateSelectionOverlay();
        } else {
            currentSelectionIds = [];
            clearSelectionOverlay();
        }

        applyZoom();
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
        const blob = new Blob([bytes], { type: mediaType });
        const url = URL.createObjectURL(blob);
        const prior = assetBlobCache[payload.asset_id];
        if (prior && prior.url) {
            try {
                URL.revokeObjectURL(prior.url);
            } catch (_e) {

            }
        }
        assetBlobCache[payload.asset_id] = {
            url: url,
            media_type: mediaType,
            original_filename: payload.original_filename || "",
        };
    }

    function assetFilename(assetId) {
        const entry = assetBlobCache[assetId];
        return (entry && entry.original_filename) || "";
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
            console.error("base64 decode failed:", e);
            return null;
        }
    }

    function refreshAssetVarStyle() {
        if (!assetVarStyleEl) {
            return;
        }
        assetVarStyleEl.textContent = buildAssetVarCss();
    }

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

            parts.push("  --asset-" + id + ": url(" + entry.url + ");");
            iter += 1;
        }
        parts.push("}");
        return parts.join("\n");
    }

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
        const parts = m[1].split(",").map(function (s) {
            return parseFloat(s);
        });
        if (parts.length < 4) {
            return 1;
        }
        const a = parts[0];
        if (!isFinite(a) || a === 0) {
            return 1;
        }
        return a;
    }

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

    function effectiveZoomScale() {
        if (zoomMode === "fit") {
            const f = computeFitScale();
            if (f && isFinite(f) && f > 0) {
                return f;
            }
        }
        return zoomManualPct / 100;
    }

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
                "translate(" +
                panX +
                "px," +
                panY +
                "px) scale(" +
                effectiveZoomScale() +
                ")";
        }
        const pct = document.getElementById("zoom-pct");
        if (pct) {
            pct.textContent =
                zoomMode === "fit" ? "Fit" : Math.round(zoomManualPct) + "%";
        }
        if (currentSelectionIds.length > 0) {
            updateSelectionOverlay();
        }
        refreshRulers();
        renderRulerGuides();
        renderCanvasScrim();
    }

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
        if (next < ZOOM_MIN) {
            next = ZOOM_MIN;
        }
        if (next > ZOOM_MAX) {
            next = ZOOM_MAX;
        }
        zoomMode = "manual";
        zoomManualPct = next;
        applyZoom();
    }

    function setTool(name) {
        activeTool = name === "hand" ? "hand" : "select";
        const sel = document.getElementById("tool-select");
        const hand = document.getElementById("tool-hand");
        if (sel) {
            sel.classList.toggle("is-on", activeTool === "select");
        }
        if (hand) {
            hand.classList.toggle("is-on", activeTool === "hand");
        }
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
        if (stage && activeTool === "hand") {
            stage.style.cursor = "grab";
        }
        window.removeEventListener("mousemove", onPanMouseMove);
        window.removeEventListener("mouseup", onPanMouseUp);
    }

    function findElement(id) {
        if (!currentShadow) {
            return null;
        }
        const safe = window.CSS && window.CSS.escape ? window.CSS.escape(id) : id;
        return currentShadow.querySelector('[data-element-id="' + safe + '"]');
    }

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

                if (
                    pendingDragEnds[patch.element_id] &&
                    (patch.property === "left" || patch.property === "top")
                ) {
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

        if (currentSelectionIds.length > 0) {
            updateSelectionOverlay();
        }
    }

    function clearSelectionOverlay() {
        const overlay = document.getElementById("selection-overlay");
        if (overlay) {
            overlay.replaceChildren();
        }
    }

    const SELECTION_OUTSET_PX = 0;

    const SELECTION_HANDLES = [
        { name: "nw", fx: 0, fy: 0 },
        { name: "n", fx: 0.5, fy: 0 },
        { name: "ne", fx: 1, fy: 0 },
        { name: "e", fx: 1, fy: 0.5 },
        { name: "se", fx: 1, fy: 1 },
        { name: "s", fx: 0.5, fy: 1 },
        { name: "sw", fx: 0, fy: 1 },
        { name: "w", fx: 0, fy: 0.5 },
    ];

    function updateSelectionOverlay() {
        const overlay = document.getElementById("selection-overlay");
        if (!overlay) {
            return;
        }
        overlay.replaceChildren();

        if (cropState) {
            return;
        }
        if (!currentShadow || !currentSlideHost) {
            return;
        }

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
        let unionL = Infinity,
            unionT = Infinity,
            unionR = -Infinity,
            unionB = -Infinity;
        for (let i = 0; i < currentSelectionIds.length; i++) {
            const id = currentSelectionIds[i];
            const safe = window.CSS && window.CSS.escape ? window.CSS.escape(id) : id;
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
                    if (isGroup && spec.name.length === 1) {
                        continue;
                    }
                    const handle = document.createElement("div");
                    handle.className = "selection-handle";
                    handle.dataset.handle = spec.name;
                    handle.dataset.elementId = id;
                    handle.style.left = boxLeft + spec.fx * boxWidth + "px";
                    handle.style.top = boxTop + spec.fy * boxHeight + "px";
                    handle.addEventListener("mousedown", onResizeHandleMouseDown);
                    overlay.appendChild(handle);
                }
            }
        }

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
                handle.style.left = bx + c.fx * bw + "px";
                handle.style.top = by + c.fy * bh + "px";
                handle.addEventListener("mousedown", onMultiScaleMouseDown);
                overlay.appendChild(handle);
            }
        }
    }

    let tableCellSel = null;

    function focusedTableId() {
        if (focusChain.length === 0 || !currentShadow) {
            return null;
        }
        const top = focusChain[focusChain.length - 1];
        const safe = window.CSS && window.CSS.escape ? window.CSS.escape(top) : top;
        const el = currentShadow.querySelector('[data-element-id="' + safe + '"]');
        return el && el.dataset.elementType === "table" ? top : null;
    }

    function tableCellGrid(tableId) {
        const safe =
            window.CSS && window.CSS.escape ? window.CSS.escape(tableId) : tableId;
        const wrap =
            currentShadow &&
            currentShadow.querySelector('[data-element-id="' + safe + '"]');
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

    function cellAtPoint(tableId, clientX, clientY) {
        const grid = tableCellGrid(tableId);
        for (let r = 0; r < grid.length; r++) {
            for (let c = 0; c < grid[r].length; c++) {
                const rect = grid[r][c].td.getBoundingClientRect();
                if (
                    clientX >= rect.left &&
                    clientX <= rect.right &&
                    clientY >= rect.top &&
                    clientY <= rect.bottom
                ) {
                    return [r, c];
                }
            }
        }
        return null;
    }

    function cellKey(rc) {
        return rc[0] + "," + rc[1];
    }

    function rangeCells(a, b) {
        const r0 = Math.min(a[0], b[0]),
            r1 = Math.max(a[0], b[0]);
        const c0 = Math.min(a[1], b[1]),
            c1 = Math.max(a[1], b[1]);
        const out = [];
        for (let r = r0; r <= r1; r++) {
            for (let c = c0; c <= c1; c++) {
                out.push([r, c]);
            }
        }
        return out;
    }

    function selectCell(tableId, rc, e) {
        const sameTable = tableCellSel && tableCellSel.elementId === tableId;
        if (sameTable && e && e.shiftKey) {
            tableCellSel.cells = rangeCells(tableCellSel.anchor, rc);
        } else if (sameTable && e && (e.metaKey || e.ctrlKey)) {
            const k = cellKey(rc);
            const idx = tableCellSel.cells.findIndex(function (x) {
                return cellKey(x) === k;
            });
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
            box.style.left = rect.left - overlayRect.left + "px";
            box.style.top = rect.top - overlayRect.top + "px";
            box.style.width = rect.width + "px";
            box.style.height = rect.height + "px";
            box.style.pointerEvents = "none";
            box.style.boxSizing = "border-box";
            overlay.appendChild(box);
        }
    }

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
        function onBlur() {
            finish(true);
        }
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

    function tableContextId() {
        if (tableCellSel) {
            return tableCellSel.elementId;
        }
        if (currentSelectionIds.length === 1 && currentShadow) {
            const id = currentSelectionIds[0];
            const safe = window.CSS && window.CSS.escape ? window.CSS.escape(id) : id;
            const el = currentShadow.querySelector('[data-element-id="' + safe + '"]');
            if (el && el.dataset.elementType === "table") {
                return id;
            }
        }
        return null;
    }

    function tableAnchor() {
        return tableCellSel && tableCellSel.anchor ? tableCellSel.anchor : [0, 0];
    }

    function renderedTable(tableId) {
        const safe =
            window.CSS && window.CSS.escape ? window.CSS.escape(tableId) : tableId;
        return (
            currentShadow &&
            currentShadow.querySelector('[data-element-id="' + safe + '"] table')
        );
    }

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
        const hr = table
            ? parseInt(table.getAttribute("data-header-rows") || "0", 10)
            : 0;
        const hc = table
            ? parseInt(table.getAttribute("data-header-columns") || "0", 10)
            : 0;
        const rowChk = document.getElementById("table-header-row");
        const colChk = document.getElementById("table-header-col");
        if (rowChk) {
            rowChk.checked = hr > 0;
        }
        if (colChk) {
            colChk.checked = hc > 0;
        }
    }

    function tableSend(kind, extra) {
        const tid = tableContextId();
        if (!tid) {
            return;
        }
        window.__deck.send(
            "Interaction",
            Object.assign({ kind: kind, element_id: tid }, extra || {}),
        );
    }

    function wireTableBox() {
        const bind = function (id, fn) {
            const el = document.getElementById(id);
            if (el) {
                el.addEventListener(id.indexOf("header") >= 0 ? "change" : "click", fn);
            }
        };
        bind("table-add-row", function () {
            tableSend("TableInsertRow", { at: tableAnchor()[0] + 1 });
        });
        bind("table-del-row", function () {
            tableSend("TableDeleteRow", { at: tableAnchor()[0] });
        });
        bind("table-add-col", function () {
            tableSend("TableInsertColumn", { at: tableAnchor()[1] + 1 });
        });
        bind("table-del-col", function () {
            tableSend("TableDeleteColumn", { at: tableAnchor()[1] });
        });
        bind("table-header-row", function () {
            const c = document.getElementById("table-header-row");
            tableSend("TableSetHeaderRows", { count: c && c.checked ? 1 : 0 });
        });
        bind("table-header-col", function () {
            const c = document.getElementById("table-header-col");
            tableSend("TableSetHeaderColumns", { count: c && c.checked ? 1 : 0 });
        });
    }

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

    function slideToScreen(layer) {
        const host = currentSlideHost;
        if (!host || !layer) {
            return null;
        }
        const hr = host.getBoundingClientRect();
        const lr = layer.getBoundingClientRect();
        return { ox: hr.left - lr.left, oy: hr.top - lr.top, scale: hr.width / 1920 };
    }

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
            ox: hr.left - sr.left,
            oy: hr.top - sr.top,
            scale: hr.width / w,
            slideW: w,
            slideH: h,
            stageW: sr.width,
            stageH: sr.height,
        };
    }

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

    function ensureRulers() {
        const stage = document.getElementById("viewport-container");
        if (!stage || document.getElementById("ruler-top")) {
            return;
        }
        const top = document.createElement("canvas");
        top.id = "ruler-top";
        top.className = "ruler ruler--top";
        top.addEventListener("mousedown", function (e) {
            startGuideCreate(e, "h");
        });
        const left = document.createElement("canvas");
        left.id = "ruler-left";
        left.className = "ruler ruler--left";
        left.addEventListener("mousedown", function (e) {
            startGuideCreate(e, "v");
        });
        const corner = document.createElement("div");
        corner.id = "ruler-corner";
        corner.className = "ruler-corner";
        stage.append(top, left, corner);
    }

    function toggleRulers() {
        rulersOn = !rulersOn;
        ensureRulers();
        refreshRulers();
        renderRulerGuides();
    }

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

    function rulerStep(scale) {
        const cands = [1, 2, 5, 10, 20, 25, 50, 100, 200, 250, 500, 1000, 2000, 5000];
        for (let i = 0; i < cands.length; i++) {
            if (cands[i] * scale >= 64) {
                return cands[i];
            }
        }
        return cands[cands.length - 1];
    }

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
            const len = major ? RULER : RULER * 0.4;
            if (horiz) {
                ctx.moveTo(s + 0.5, RULER);
                ctx.lineTo(s + 0.5, RULER - len);
            } else {
                ctx.moveTo(RULER, s + 0.5);
                ctx.lineTo(RULER - len, s + 0.5);
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

    function currentGuides() {
        return guideOwn;
    }

    function sendGuideEvent(payload) {
        window.__deck.send("Interaction", payload);
    }

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
            line.style.top = m.oy + g.pos * m.scale + "px";
            line.style.left = m.ox + "px";
            line.style.width = m.slideW * m.scale + "px";
        } else {
            line.classList.add("guide--v");
            line.style.left = m.ox + g.pos * m.scale + "px";
            line.style.top = m.oy + "px";
            line.style.height = m.slideH * m.scale + "px";
        }

        if (!readOnly) {
            line.addEventListener("mousedown", function (e) {
                startGuideDrag(e, g);
            });
        }
        return line;
    }

    function pointerToSlide(e, orient, m) {
        const sr = document.getElementById("viewport-container").getBoundingClientRect();
        if (orient === "h") {
            return Math.round((e.clientY - sr.top - m.oy) / m.scale);
        }
        return Math.round((e.clientX - sr.left - m.ox) / m.scale);
    }

    function clampGuidePos(orient, pos, m) {
        const max = orient === "h" ? m.slideH : m.slideW;
        return Math.max(0, Math.min(max, pos));
    }

    function overRuler(e, orient) {
        const sr = document.getElementById("viewport-container").getBoundingClientRect();
        if (orient === "h") {
            return e.clientY - sr.top < RULER;
        }
        return e.clientX - sr.left < RULER;
    }

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

        const g = {
            id: "gtmp",
            index: -1,
            orient: orient,
            pos: clampGuidePos(orient, pointerToSlide(e, orient, m), m),
        };
        guideOwn.push(g);
        selectedGuideId = g.id;
        renderRulerGuides();
        showGuideInspector();
        beginGuideSession(g, orient, true);
    }

    function startGuideDrag(e, g) {
        if (e.button !== 0) {
            return;
        }
        e.preventDefault();
        e.stopPropagation();
        selectGuide(g.id);
        beginGuideSession(g, g.orient, false);
    }

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

                guideOwn = guideOwn.filter(function (x) {
                    return x !== g;
                });
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

        slideSelected = false;
        if (currentSelectionIds.length > 0) {
            window.__deck.send("Interaction", {
                kind: "SetSelectionFromPanel",
                element_ids: [],
            });
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

    function deleteGuide(id) {
        const g = guideOwn.find(function (x) {
            return x.id === id;
        });
        if (!g || g.index < 0) {
            return;
        }
        if (selectedGuideId === id) {
            selectedGuideId = null;
            hideGuideInspector();
        }
        sendGuideEvent({ kind: "GuideRemoved", index: g.index });
    }

    function showGuideInspector() {
        const box = document.getElementById("guide-box");
        if (!box) {
            return;
        }
        const g = currentGuides().find(function (x) {
            return x.id === selectedGuideId;
        });
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
            setInspectorIcon("guide");
        }
        const lbl = document.getElementById("guide-pos-label");
        if (lbl) {
            lbl.textContent = g.orient === "h" ? "Y" : "X";
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

    function wireGuideInspector() {
        const input = document.getElementById("guide-pos");
        if (!input) {
            return;
        }
        input.addEventListener("change", function () {
            const g = currentGuides().find(function (x) {
                return x.id === selectedGuideId;
            });
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

            sendGuideEvent({ kind: "GuideMoved", index: g.index, pos: pos });
        });
    }

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

    function refitThumbnails() {
        const strip = document.getElementById("thumbnail-row");
        if (!strip) {
            return;
        }
        const cs = getComputedStyle(strip);
        const padV = parseFloat(cs.paddingTop) + parseFloat(cs.paddingBottom);
        const cap = strip.querySelector(".thumb__caption");
        const capH = cap ? cap.offsetHeight : 16;
        const gap = 6;
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
        const thumbs = strip.querySelectorAll(".thumb");
        for (let i = 0; i < thumbs.length; i++) {
            thumbs[i].style.width = pw + "px";
            const caption = thumbs[i].querySelector(".thumb__caption");
            if (caption) {
                caption.style.maxWidth = pw + "px";
            }
        }
        if (thumbDropLine) {
            thumbDropLine.style.height = ph + "px";
        }
        const previews = strip.querySelectorAll(".thumb__preview");
        for (let i = 0; i < previews.length; i++) {
            const mount = previews[i].querySelector(".thumb__mount");
            if (mount) {
                applyThumbnailScale(previews[i], mount);
            }
        }
    }

    function wireWindowControls() {
        const bar = document.getElementById("top-bar");
        const ctls = document.getElementById("window-controls");
        if (!bar || !ctls) {
            return;
        }
        const ua = navigator.userAgent || "";
        document.body.dataset.platform = /Mac|iPhone|iPad/.test(ua)
            ? "mac"
            : /Windows/.test(ua)
              ? "win"
              : "linux";

        const send = function (action) {
            window.__deck.send("WindowControl", { action: action });
        };
        const toggleMaximized = function () {
            const now = ctls.dataset.maximized === "true";
            ctls.dataset.maximized = now ? "false" : "true";
            send("maximize");
        };
        const buttons = {
            "win-close": function () {
                send("close");
            },
            "win-min": function () {
                send("minimize");
            },
            "win-max": toggleMaximized,
        };
        Object.keys(buttons).forEach(function (id) {
            const btn = document.getElementById(id);
            if (btn) {
                btn.addEventListener("click", buttons[id]);
            }
        });

        bar.addEventListener("mousedown", function (e) {
            if (e.button !== 0 || isInteractiveTarget(e.target)) {
                return;
            }
            e.preventDefault();
            send("drag");
        });
        bar.addEventListener("dblclick", function (e) {
            if (isInteractiveTarget(e.target)) {
                return;
            }
            toggleMaximized();
        });
    }

    function isInteractiveTarget(node) {
        if (!node || !node.closest) {
            return false;
        }
        return !!node.closest("button, input, select, textarea, a, [contenteditable='true']");
    }

    function wirePaneResizers() {
        const map = {
            "divider-objects": "objects",
            "divider-inspector": "inspector",
            "divider-thumbs": "thumbs",
        };
        Object.keys(map).forEach(function (id) {
            const el = document.getElementById(id);
            if (el) {
                el.addEventListener("mousedown", function (e) {
                    beginPaneDrag(e, map[id], el);
                });
            }
        });
        positionDividers();
    }

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
            const pane = document.getElementById(
                isObj ? "object-panel" : "inspector-panel",
            );
            const cur = pane.getBoundingClientRect();
            const desired = isObj ? ev.clientX - cur.left : cur.right - ev.clientX;
            const max = Math.min(PANE_MAX[kind], cur.width + (cr.width - canvasMinW));
            const w = Math.max(PANE_MIN[kind], Math.min(desired, max));
            pane.style.width = w + "px";
        }

        if (zoomMode === "fit") {
            applyZoom();
        } else {
            refreshRulers();
            renderRulerGuides();
            renderCanvasScrim();
            updateSelectionOverlay();
        }
    }

    function drawAlignLine(layer, g, m) {
        const line = document.createElement("div");
        line.className = "snap-guide snap-guide--line";
        if (g.axis === "x") {
            line.style.left = m.ox + g.pos * m.scale + "px";
            line.style.top = m.oy + "px";
            line.style.width = "1px";
            line.style.height = 1080 * m.scale + "px";
        } else {
            line.style.top = m.oy + g.pos * m.scale + "px";
            line.style.left = m.ox + "px";
            line.style.height = "1px";
            line.style.width = 1920 * m.scale + "px";
        }
        layer.appendChild(line);
    }

    function drawSpacing(layer, g, m) {
        let i = 0;
        for (i = 0; i < g.gaps.length; i = i + 1) {
            const gap = g.gaps[i];
            const bar = document.createElement("div");
            bar.className =
                "snap-guide snap-guide--space snap-guide--space-" +
                (g.axis === "x" ? "h" : "v");
            if (g.axis === "x") {
                bar.style.left = m.ox + gap.start * m.scale + "px";
                bar.style.width = (gap.end - gap.start) * m.scale + "px";
                bar.style.top = m.oy + gap.perp * m.scale + "px";
            } else {
                bar.style.top = m.oy + gap.start * m.scale + "px";
                bar.style.height = (gap.end - gap.start) * m.scale + "px";
                bar.style.left = m.ox + gap.perp * m.scale + "px";
            }
            layer.appendChild(bar);
        }
    }

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

    function clearGuides() {
        const layer = document.getElementById("snap-guides");
        if (layer) {
            layer.replaceChildren();
        }
    }

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

    function movingRectFromStyle(el) {
        const d = parseStyleAttr(el.getAttribute("style") || "");
        return {
            x: parseFloat(stripPx(d.left)) || 0,
            y: parseFloat(stripPx(d.top)) || 0,
            w: parseFloat(stripPx(d.width)) || 0,
            h: parseFloat(stripPx(d.height)) || 0,
        };
    }

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

    function cropImageUrl(assetId) {
        const entry = assetBlobCache[assetId];
        return entry && entry.url ? entry.url : "";
    }

    function clearCropOverlay() {
        const layer = document.getElementById("crop-overlay");
        if (layer) {
            layer.replaceChildren();
        }
    }

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
            handle.style.left = x + s.fx * w + "px";
            handle.style.top = y + s.fy * h + "px";
            handle.addEventListener("mousedown", onCropHandleMouseDown);
            layer.appendChild(handle);
        }
    }

    function cropDrawToolbar(layer, rightX, topY) {
        const bar = document.createElement("div");
        bar.className = "crop-toolbar";
        bar.style.left = rightX + "px";
        bar.style.top = topY + "px";
        const pct = Math.round(
            window.__crop.zoomPercent(cropState.state, cropState.mask, cropState.natural),
        );
        bar.innerHTML =
            '<input type="range" class="crop-zoom" min="100" max="400" value="' +
            pct +
            '">' +
            '<span class="crop-zoom-pct">' +
            pct +
            "%</span>" +
            '<button type="button" class="crop-btn crop-reset" title="Reset crop">Reset</button>' +
            '<button type="button" class="crop-btn crop-cancel" title="Cancel (Esc)">✕</button>' +
            '<button type="button" class="crop-btn crop-confirm" title="Done (Enter)">✓</button>';
        bar.querySelector(".crop-zoom").addEventListener("input", onCropZoomInput);
        bar.querySelector(".crop-reset").addEventListener("click", resetCrop);
        bar.querySelector(".crop-cancel").addEventListener("click", cancelCrop);
        bar.querySelector(".crop-confirm").addEventListener("click", commitCrop);
        layer.appendChild(bar);
    }

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

        const dim = document.createElement("div");
        dim.className = "crop-img crop-img--dim";
        cropPlaceImg(dim, url, imgX, imgY, imgW, imgH);
        layer.appendChild(dim);

        const bright = document.createElement("div");
        bright.className = "crop-img crop-img--bright";
        cropPlaceImg(bright, url, imgX, imgY, imgW, imgH);
        bright.style.clipPath =
            "inset(" +
            (mY - imgY) +
            "px " +
            (imgX + imgW - (mX + mW)) +
            "px " +
            (imgY + imgH - (mY + mH)) +
            "px " +
            (mX - imgX) +
            "px)";
        layer.appendChild(bright);

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

        cropDrawMaskFrame(layer, mX, mY, mW, mH);
        cropDrawToolbar(layer, mX + mW, mY);
    }

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
                decls["background-size"],
                decls["background-position"],
            );
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

            el.style.visibility = "hidden";
            document.body.dataset.crop = "1";
            updateSelectionOverlay();
            renderCropOverlay();
        };
        img.src = url;
    }

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

    function onCropWheel(e) {
        if (!cropState) {
            return;
        }
        e.preventDefault();
        const factor = e.deltaY < 0 ? 1.05 : 1 / 1.05;
        cropState.state = window.__crop.zoom(
            cropState.state,
            cropState.mask,
            cropState.natural,
            factor,
        );
        renderCropOverlay();
    }

    function onCropZoomInput(e) {
        if (!cropState) {
            return;
        }
        const pct = parseFloat(e.currentTarget.value) || 100;
        cropState.state = window.__crop.setZoomPercent(
            pct,
            cropState.state,
            cropState.mask,
            cropState.natural,
        );
        renderCropOverlay();
    }

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
                x: cropState.mask.x,
                y: cropState.mask.y,
                w: cropState.mask.w,
                h: cropState.mask.h,
            },

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

        const raw = computeResizeRect(
            { handle: cropResize.handle, startRect: cropResize.startMask },
            dx,
            dy,
            !!e.shiftKey,
            !!e.altKey,
        );
        const snapped = window.__snap.forResize(
            raw,
            handleEdges(cropResize.handle),
            cropResize.snapTargets,
            {
                threshold: 3 / scale,
                gridEnabled: gridEnabled,
                suppress: !!e.metaKey,
                shift: !!e.shiftKey,
                alt: !!e.altKey,
                aspect: cropResize.startMask.w / cropResize.startMask.h,
            },
        );
        cropState.mask = {
            x: snapped.rect.x,
            y: snapped.rect.y,
            w: snapped.rect.w,
            h: snapped.rect.h,
        };

        cropState.state = window.__crop.placeImage(
            cropState.state,
            cropState.mask,
            cropResize.imgOrigin.x,
            cropResize.imgOrigin.y,
            cropState.natural,
        );
        renderCropOverlay();
        renderGuides(snapped.guides);
    }
    function onCropHandleMouseUp() {
        cropResize = null;
        clearGuides();
        window.removeEventListener("mousemove", onCropHandleMouseMove);
        window.removeEventListener("mouseup", onCropHandleMouseUp);
    }

    function resetCrop() {
        if (!cropState) {
            return;
        }
        cropState.state = window.__crop.fromCover(cropState.mask, cropState.natural);
        renderCropOverlay();
    }

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

    function cancelCrop() {
        exitCropMode();
    }

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

    function refreshCropBox() {
        const box = document.getElementById("crop-box");
        if (!box) {
            return;
        }
        const el =
            currentSelectionIds.length === 1 ? findElement(currentSelectionIds[0]) : null;
        if (!el || el.dataset.elementType !== "image") {
            box.hidden = true;
            return;
        }
        box.hidden = false;
        const decls = parseStyleAttr(el.getAttribute("style") || "");
        const state = window.__crop.fromStyles(
            decls["background-size"],
            decls["background-position"],
        );
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

    function onCropInspectorEdit() {
        if (currentSelectionIds.length !== 1) {
            return;
        }
        const id = currentSelectionIds[0];
        withImageNatural(id, function (el, mask, natural, decls) {
            let state =
                window.__crop.fromStyles(
                    decls["background-size"],
                    decls["background-position"],
                ) || window.__crop.fromCover(mask, natural);
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

    function inspectorResetCrop(id) {
        withImageNatural(id, function (el, mask, natural) {
            sendCropStyleEdits(
                id,
                window.__crop.toStyles(window.__crop.fromCover(mask, natural)),
            );
        });
    }

    function sendCropStyleEdits(id, css) {
        window.__deck.send("Interaction", {
            kind: "PropertyChanged",
            element_id: id,
            property: "background-size",
            value: css.backgroundSize,
        });
        window.__deck.send("Interaction", {
            kind: "PropertyChanged",
            element_id: id,
            property: "background-position",
            value: css.backgroundPosition,
        });
    }

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

    function findInteractionTarget(e) {
        const path = typeof e.composedPath === "function" ? e.composedPath() : [];
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
        if (!hit) {
            return null;
        }
        const chain = elementChain(hit);
        if (focusChain.length === 0) {
            return chain[chain.length - 1];
        }

        const deep = focusChain[focusChain.length - 1];
        for (let i = 0; i < chain.length; i++) {
            const parent = chain[i].parentElement;
            if (parent && parent.dataset && parent.dataset.elementId === deep) {
                return chain[i];
            }
        }
        return chain[chain.length - 1];
    }

    function readModifiers(e) {
        return {
            shift: !!e.shiftKey,
            ctrl: !!e.ctrlKey,
            alt: !!e.altKey,
            meta: !!e.metaKey,
        };
    }

    function onViewportDblClick(e) {
        const target = findInteractionTarget(e);
        if (!target) {
            return;
        }
        if (target.dataset.elementType === "group") {
            e.preventDefault();
            focusChain.push(target.dataset.elementId);

            const inner = findInteractionTarget(e);
            if (inner && inner.dataset.elementId) {
                window.__deck.send("Interaction", {
                    kind: "ElementClicked",
                    element_id: inner.dataset.elementId,
                    modifiers: readModifiers(e),
                    position: { x: e.clientX, y: e.clientY },
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

            ev.stopPropagation();
            if (ev.key === "Escape") {
                ev.preventDefault();
                cancelTextEdit();
                return;
            }
            const mark = matchTextMarkShortcut(ev);
            if (mark) {
                ev.preventDefault();
                applyMarkInEditor(mark);
            }
        };
        const onBlur = function () {
            commitTextEdit();
        };
        textEditState = {
            elementId: elementId,
            target: target,
            original: target.innerHTML,
            onKeydown: onKeydown,
            onBlur: onBlur,
        };

        // Swapping in the raw token text flattens markup, so only do it for a
        // body that is a bare text node anyway.
        if (
            target.dataset &&
            typeof target.dataset.src === "string" &&
            target.childElementCount === 0
        ) {
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

            window.__deck.send("Interaction", {
                kind: "TextEditEnded",
                element_id: state.elementId,
                content: richTextFromDom(target),
            });
        } else {
            target.innerHTML = state.original;
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

    const TEXT_MARK_TAGS = {
        B: "bold",
        STRONG: "bold",
        I: "italic",
        EM: "italic",
        U: "underline",
        S: "strike",
        STRIKE: "strike",
        DEL: "strike",
    };

    // Mirrors parse_rich_text in src/html/parse.rs: the contenteditable DOM is
    // whatever the browser produced, this flattens it back to plain text plus
    // byte ranges. Rust normalizes and re-serializes, so browser markup never
    // reaches the deck.
    function richTextFromDom(el) {
        const encoder = new TextEncoder();
        const runs = [];
        let plain = "";
        let bytes = 0;
        const stack = [];
        let i;
        for (i = el.childNodes.length - 1; i >= 0; i--) {
            stack.push({ node: el.childNodes[i], marks: {} });
        }
        const MAX_NODES = 100000;
        let visited = 0;
        while (stack.length > 0 && visited < MAX_NODES) {
            visited++;
            const entry = stack.pop();
            const node = entry.node;
            if (node.nodeType === 3) {
                const start = bytes;
                plain += node.nodeValue;
                bytes += encoder.encode(node.nodeValue).length;
                if (Object.keys(entry.marks).length > 0 && bytes > start) {
                    runs.push({ start: start, end: bytes, marks: entry.marks });
                }
                continue;
            }
            if (node.nodeType !== 1) {
                continue;
            }
            if (node.tagName === "BR") {
                plain += "\n";
                bytes += 1;
                continue;
            }
            // The browser starts a new block on Enter; innerText used to turn
            // that into a newline and the model still expects one. Only these
            // tags, never computed display: a mark wrapper inside a flex text
            // box computes as block but must not break the line.
            if (
                (node.tagName === "DIV" || node.tagName === "P") &&
                bytes > 0 &&
                plain.charAt(plain.length - 1) !== "\n"
            ) {
                plain += "\n";
                bytes += 1;
            }
            const marks = mergeDomMarks(node, entry.marks);
            for (i = node.childNodes.length - 1; i >= 0; i--) {
                stack.push({ node: node.childNodes[i], marks: marks });
            }
        }
        return { plain: plain, runs: runs };
    }

    function mergeDomMarks(node, inherited) {
        const marks = Object.assign({}, inherited);
        const named = TEXT_MARK_TAGS[node.tagName];
        if (named) {
            marks[named] = true;
        }
        if (node.tagName === "A" && node.getAttribute("href")) {
            marks.link = node.getAttribute("href");
        }
        const style = node.style;
        if (style) {
            const weight = style.fontWeight;
            if (weight === "bold" || weight === "bolder" || parseInt(weight, 10) >= 600) {
                marks.bold = true;
            }
            if (style.fontStyle === "italic") {
                marks.italic = true;
            }
            const deco = style.textDecoration || style.textDecorationLine || "";
            if (deco.indexOf("underline") >= 0) {
                marks.underline = true;
            }
            if (deco.indexOf("line-through") >= 0) {
                marks.strike = true;
            }
            if (style.color) {
                marks.color = { Literal: style.color };
            }
        }
        return marks;
    }

    const TEXT_MARK_COMMANDS = {
        bold: "bold",
        italic: "italic",
        underline: "underline",
        strike: "strikeThrough",
    };

    // Cmd/Ctrl+B, +I, +U and +Shift+X. Returns the mark name or null.
    function matchTextMarkShortcut(e) {
        if (!(e.metaKey || e.ctrlKey) || e.altKey) {
            return null;
        }
        const key = typeof e.key === "string" ? e.key.toLowerCase() : "";
        if (e.shiftKey) {
            return key === "x" ? "strike" : null;
        }
        if (key === "b") {
            return "bold";
        }
        if (key === "i") {
            return "italic";
        }
        if (key === "u") {
            return "underline";
        }
        return null;
    }

    const TEXT_MARK_DECLS = {
        bold: ["font-weight", "700"],
        italic: ["font-style", "italic"],
        underline: ["text-decoration", "underline"],
        strike: ["text-decoration", "line-through"],
    };

    // Outside the editor the mark applies to the whole box, which is what the
    // inspector's B/I/U/S buttons already do. Drive that same control rather
    // than adding character runs on top of it, or the two stack up.
    function toggleWholeBoxMark(mark) {
        const want = TEXT_MARK_DECLS[mark];
        const box = textStyleControls[0];
        if (!want || !box || currentSelectionIds.length !== 1) {
            return false;
        }
        const el = findElement(currentSelectionIds[0]);
        const type = el ? el.dataset.elementType : "";
        if (type !== "text" && type !== "table") {
            return false;
        }
        let i;
        for (i = 0; i < TEXT_STYLE_BUTTONS.length; i++) {
            const spec = TEXT_STYLE_BUTTONS[i];
            if (spec.prop === want[0] && spec.on === want[1]) {
                toggleTextStyle(box, spec);
                return true;
            }
        }
        return false;
    }

    // Inside the editor the browser already owns both cases the user cares
    // about: a range takes the mark now, a bare caret carries it into the next
    // characters typed. Commit reads the resulting DOM back into runs.
    function applyMarkInEditor(mark) {
        const command = TEXT_MARK_COMMANDS[mark];
        if (!command) {
            return;
        }
        try {
            document.execCommand("styleWithCSS", false, false);
            if (!document.execCommand(command, false, null)) {
                console.warn("text mark command refused:", mark);
            }
        } catch (err) {
            console.warn("text mark command failed:", mark, err);
        }
    }

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

        }
    }

    function onMouseDown(e) {

        if (e.button !== 0) {
            return;
        }

        if (cropState) {
            const inOverlay =
                e.target && e.target.closest && e.target.closest("#crop-overlay");
            if (!inOverlay) {
                commitCrop();
            }
            return;
        }

        if (textEditState) {
            const path = (e.composedPath && e.composedPath()) || [];
            if (path.indexOf(textEditState.target) >= 0) {
                return;
            }
            commitTextEdit();
        }

        if (activeTool === "hand") {
            e.preventDefault();
            panSession = {
                startX: e.clientX,
                startY: e.clientY,
                basePanX: panX,
                basePanY: panY,
            };
            document.body.style.userSelect = "none";
            const stage = document.getElementById("viewport-container");
            if (stage) {
                stage.style.cursor = "grabbing";
            }
            window.addEventListener("mousemove", onPanMouseMove);
            window.addEventListener("mouseup", onPanMouseUp);
            return;
        }

        slideSelected = false;
        deselectGuide();

        const focusSnapshot = focusChain.slice();
        const slideHost = e.target.closest && e.target.closest(".slide-host");
        const target = slideHost ? findInteractionTarget(e) : null;
        if (!target) {
            armMarquee(e, focusSnapshot);
            return;
        }

        const ftid = focusedTableId();
        if (
            ftid &&
            elementChain(target).some(function (n) {
                return n.dataset.elementId === ftid;
            })
        ) {
            const rc = cellAtPoint(ftid, e.clientX, e.clientY);
            if (rc) {
                selectCell(ftid, rc, e);
                document.body.style.userSelect = "none";
                return;
            }
        }

        if (focusChain.length > 0) {
            const deep = focusChain[focusChain.length - 1];
            const insideFocus = elementChain(target).some(function (n) {
                return n.dataset.elementId === deep;
            });
            if (!insideFocus) {
                focusChain = [];
                tableCellSel = null;
            }
        }
        const elementId = target.dataset.elementId;

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

        document.body.style.userSelect = "none";
    }

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
        box.style.left = Math.min(marquee.startX, cx) - sr.left + "px";
        box.style.top = Math.min(marquee.startY, cy) - sr.top + "px";
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
        return !(
            b.right < a.left ||
            b.left > a.right ||
            b.bottom < a.top ||
            b.top > a.bottom
        );
    }

    function marqueeCandidates(focusSnapshot) {
        if (!currentShadow) {
            return [];
        }
        const levelParent = focusSnapshot.length
            ? focusSnapshot[focusSnapshot.length - 1]
            : null;
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

    function sendMarqueeSelection(ids) {
        const key = ids.join(",");
        if (key === marquee.lastSentKey) {
            return;
        }
        marquee.lastSentKey = key;
        window.__deck.send("Interaction", {
            kind: "SetSelectionFromPanel",
            element_ids: ids,
        });
    }

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

    function renderDrag(clientX, clientY, shiftHeld, metaHeld) {
        if (!dragState || !dragState.started) {
            return;
        }
        const scale = getViewportScale();
        dragState.lastMouse = { x: clientX, y: clientY };

        const d = computeDragDelta(clientX, clientY, scale, shiftHeld, metaHeld, true);
        if (dragState.multi) {
            for (let i = 0; i < dragState.targets.length; i++) {
                optimisticTransform(dragState.targets[i].node, d.x, d.y);
            }
        } else {
            optimisticTransform(dragState.target, d.x, d.y);
            reportDragThrottled(
                dragState.element_id,
                { x: d.x, y: d.y },
                { x: clientX, y: clientY },
            );
        }
    }

    function onDragKeyChange(e) {
        if (
            e.key !== "Shift" ||
            !dragState ||
            !dragState.started ||
            !dragState.lastMouse
        ) {
            return;
        }
        renderDrag(dragState.lastMouse.x, dragState.lastMouse.y, e.shiftKey, e.metaKey);
    }

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
                e.clientX,
                e.clientY,
                scale,
                e.shiftKey,
                e.metaKey,
                false,
            );

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
            })(
                held.map(function (h) {
                    return h.id;
                }),
            );
            if (dragState.multi) {
                window.__deck.send("Interaction", {
                    kind: "ElementsDragEnded",
                    element_ids: dragState.targets.map(function (t) {
                        return t.id;
                    }),
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

            window.__deck.send("Interaction", {
                kind: "SetSelectionFromPanel",
                element_ids: [dragState.collapseId],
            });
        }

        document.body.style.userSelect = "";
        clearGuides();
        dragState = null;
    }

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

    const handlers = {
        MountSlide: function (payload) {
            mountSlide(
                payload.slide_id,
                payload.slide_html,
                payload.theme_css,
                payload.globals_css,
            );
            refreshInspector();

            updateThumbnailHtml(payload.slide_id, payload.slide_html, payload.theme_css);
            highlightActiveThumbnail(payload.slide_id);

            selectedGuideId = null;
            refreshRulers();
            renderRulerGuides();
            renderCanvasScrim();
        },
        ApplyPatch: function (payload) {
            applyPatch(payload);

            refreshInspector();
        },
        SetSelection: function (payload) {
            const ids =
                payload && Array.isArray(payload.element_ids) ? payload.element_ids : [];
            currentSelectionIds = ids.slice();

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

            const own = (payload && payload.own) || [];
            const inh = (payload && payload.inherited) || [];
            guideOwn = own.map(function (g, i) {
                return { id: "g" + i, index: i, orient: g.axis, pos: g.pos };
            });
            guideInherited = inh.map(function (g, i) {
                return { id: "gi" + i, index: i, orient: g.axis, pos: g.pos };
            });
            if (
                selectedGuideId !== null &&
                !guideOwn.some(function (x) {
                    return x.id === selectedGuideId;
                })
            ) {
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

            if (currentSelectionIds.length === 0) {
                refreshInspector();
            }
        },
        Notice: function (payload) {
            showToast((payload && payload.message) || "", payload && payload.detail);
        },
        AssetsUpdate: function (payload) {
            const assets = payload && Array.isArray(payload.assets) ? payload.assets : [];
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
            availableFonts =
                payload && Array.isArray(payload.families) ? payload.families : [];
        },
        AgentPanelStateUpdate: function (payload) {
            if (payload) {
                set_panel_state(payload);
            }
        },
        AgentStream: function (payload) {
            if (payload) {
                append_stream_chunk(payload);
            }
        },
        AgentTool: function (payload) {
            if (payload) {
                const log = document.querySelector("#agent-log");
                if (log) {
                    const row = document.createElement("div");
                    row.className = "agent__message agent__message--tool";
                    const kind = payload.kind || "";
                    const summary = payload.summary || "";
                    row.textContent = "[" + kind + "] " + summary;
                    log.appendChild(row);
                    log.scrollTop = log.scrollHeight;
                }
            }
        },
        AgentPermission: function (payload) {
            if (payload) {
                show_permission_ask(payload);
            }
        },
        AgentListUpdate: function (payload) {
            if (payload) {
                populate_agent_select(payload);
            }
        },
        AgentActivityUpdate: function (payload) {
            if (payload) {
                set_activity(payload);
            }
        },
        AgentThoughtUpdate: function (payload) {
            if (payload) {
                append_thought(payload);
            }
        },
        AgentToolStatusUpdate: function (payload) {
            if (payload) {
                upsert_tool_row(payload);
            }
        },
    };

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

    let resizeState = null;
    let resizeRafScheduled = false;
    let pendingResize = null;

    let multiScaleState = null;

    const RESIZE_MIN_PX = 1;
    const RESIZE_THROTTLE_KEY = "kind";

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

        e.stopPropagation();
        e.preventDefault();

        const cropStart =
            target.dataset.elementType === "image"
                ? window.__crop.fromStyles(
                      decls["background-size"],
                      decls["background-position"],
                  )
                : null;

        const isGroup = target.dataset.elementType === "group";
        const priorScale = isGroup ? parseFloat(target.dataset.flexScale || "1") || 1 : 1;
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

            visualRect: {
                x: startRect.x,
                y: startRect.y,
                w: startRect.w * priorScale,
                h: startRect.h * priorScale,
            },
        };
        if (isGroup) {

            // ponytail: scale-only preview; rotated groups re-render correct on commit.
            target.style.transformOrigin = "0 0";
        } else {

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

    function resizeHandleToRustEnum(name) {
        switch (name) {
            case "nw":
                return "TopLeft";
            case "n":
                return "Top";
            case "ne":
                return "TopRight";
            case "e":
                return "Right";
            case "se":
                return "BottomRight";
            case "s":
                return "Bottom";
            case "sw":
                return "BottomLeft";
            case "w":
                return "Left";
            default:
                return "BottomRight";
        }
    }

    function handleEdges(name) {
        return {
            west: name.indexOf("w") >= 0,
            east: name.indexOf("e") >= 0,
            north: name.indexOf("n") >= 0,
            south: name.indexOf("s") >= 0,
        };
    }

    function snappedResizeRect(rect, e, scale, draw) {
        if (!resizeState || !resizeState.snapTargets) {
            return rect;
        }
        const out = window.__snap.forResize(
            rect,
            handleEdges(resizeState.handle),
            resizeState.snapTargets,
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

    function computeResizeRect(state, dx, dy, shift, alt) {
        const handle = state.handle;
        const start = state.startRect;

        let dWest = 0,
            dEast = 0,
            dNorth = 0,
            dSouth = 0;
        if (handle.indexOf("w") >= 0) {
            dWest = -dx;
        }
        if (handle.indexOf("e") >= 0) {
            dEast = dx;
        }
        if (handle.indexOf("n") >= 0) {
            dNorth = -dy;
        }
        if (handle.indexOf("s") >= 0) {
            dSouth = dy;
        }

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
            if (wSign !== 0) {
                dWest = wSign * Math.abs(scaledDW);
            }
            if (eSign !== 0) {
                dEast = eSign * Math.abs(scaledDW);
            }
            if (nSign !== 0) {
                dNorth = nSign * Math.abs(scaledDH);
            }
            if (sSign !== 0) {
                dSouth = sSign * Math.abs(scaledDH);
            }
        }

        if (alt) {
            if (dWest !== 0) {
                dEast = dWest;
            }
            if (dEast !== 0 && dWest === 0) {
                dWest = dEast;
            }
            if (dNorth !== 0) {
                dSouth = dNorth;
            }
            if (dSouth !== 0 && dNorth === 0) {
                dNorth = dSouth;
            }
        }

        let newW = start.w + dWest + dEast;
        let newH = start.h + dNorth + dSouth;
        let newX = start.x - dWest;
        let newY = start.y - dNorth;

        if (newW < RESIZE_MIN_PX) {

            if (handle.indexOf("w") >= 0) {
                newX = start.x + start.w - RESIZE_MIN_PX;
            }
            newW = RESIZE_MIN_PX;
        }
        if (newH < RESIZE_MIN_PX) {
            if (handle.indexOf("n") >= 0) {
                newY = start.y + start.h - RESIZE_MIN_PX;
            }
            newH = RESIZE_MIN_PX;
        }
        return { x: newX, y: newY, w: newW, h: newH };
    }

    function isCornerHandle(name) {
        return name === "nw" || name === "ne" || name === "sw" || name === "se";
    }

    function groupResizeScale(e, scale) {
        const dx = (e.clientX - resizeState.startMouse.x) / scale;
        const dy = (e.clientY - resizeState.startMouse.y) / scale;
        const synthetic = {
            handle: resizeState.handle,
            startRect: resizeState.visualRect,
        };
        const r = computeResizeRect(synthetic, dx, dy, true, false);
        const f = resizeState.visualRect.w > 0 ? r.w / resizeState.visualRect.w : 1;
        return Math.max(0.01, resizeState.priorScale * f);
    }

    function onResizeMouseMove(e) {
        if (!resizeState) {
            return;
        }
        const scale = getViewportScale();
        if (resizeState.isGroup) {

            const s = groupResizeScale(e, scale);
            resizeState.target.style.transform = "scale(" + s + ")";
            updateSelectionOverlay();
            return;
        }
        const dx = (e.clientX - resizeState.startMouse.x) / scale;
        const dy = (e.clientY - resizeState.startMouse.y) / scale;
        const rect = snappedResizeRect(
            computeResizeRect(resizeState, dx, dy, !!e.shiftKey, !!e.altKey),
            e,
            scale,
            true,
        );
        applyOptimisticRect(resizeState.target, rect);
        applyOptimisticCropScale(rect);
        updateSelectionOverlay();
        scheduleResizeReport(rect, e);
    }

    function croppedResizeStyles(rect) {
        if (!resizeState || !resizeState.cropStart) {
            return null;
        }
        const scaled = window.__crop.scaleForBox(
            resizeState.cropStart,
            resizeState.startRect.w,
            resizeState.startRect.h,
            rect.w,
            rect.h,
        );
        return window.__crop.toStyles(scaled);
    }

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
                window.__deck.send(
                    "Interaction",
                    Object.assign({ kind: "ElementResized" }, pendingResize),
                );
                pendingResize = null;
            }
            resizeRafScheduled = false;
        });
    }

    function onResizeMouseUp(e) {
        if (!resizeState) {
            return;
        }
        const scale = getViewportScale();
        const dx = (e.clientX - resizeState.startMouse.x) / scale;
        const dy = (e.clientY - resizeState.startMouse.y) / scale;

        if (resizeState.isGroup) {
            const finalScale = groupResizeScale(e, scale);
            if (resizeState.savedTransform === "") {
                resizeState.target.style.removeProperty("transform");
            } else {
                resizeState.target.style.transform = resizeState.savedTransform;
            }
            window.__deck.send("Interaction", {
                kind: "SetGroupScale",
                element_id: resizeState.elementId,
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
        const rect = snappedResizeRect(
            computeResizeRect(resizeState, dx, dy, !!e.shiftKey, !!e.altKey),
            e,
            scale,
            false,
        );
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

    function onMultiScaleMouseDown(e) {
        if (e.button !== 0 || !currentShadow) {
            return;
        }
        e.preventDefault();
        e.stopPropagation();
        const items = [];
        let ul = Infinity,
            ut = Infinity,
            ur = -Infinity,
            ub = -Infinity;
        for (let i = 0; i < currentSelectionIds.length; i++) {
            const node = findElement(currentSelectionIds[i]);
            if (!node) {
                continue;
            }
            const r = movingRectFromStyle(node);
            items.push({ id: currentSelectionIds[i], node: node, rect: r });
            ul = Math.min(ul, r.x);
            ut = Math.min(ut, r.y);
            ur = Math.max(ur, r.x + r.w);
            ub = Math.max(ub, r.y + r.h);
        }
        if (items.length < 2 || ur <= ul || ub <= ut) {
            return;
        }
        const name = e.currentTarget.dataset.handle;

        const cornerX = name.indexOf("w") >= 0 ? ul : ur;
        const cornerY = name.indexOf("n") >= 0 ? ut : ub;
        const anchor = {
            x: name.indexOf("w") >= 0 ? ur : ul,
            y: name.indexOf("n") >= 0 ? ub : ut,
        };
        multiScaleState = {
            items: items,
            anchor: anchor,
            corner: { x: cornerX, y: cornerY },
        };
        document.body.style.userSelect = "none";
        window.addEventListener("mousemove", onMultiScaleMouseMove);
        window.addEventListener("mouseup", onMultiScaleMouseUp);
    }

    function multiScaleFactor(e) {
        const stage = document
            .getElementById("viewport-container")
            .getBoundingClientRect();
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
            it.node.style.transformOrigin =
                a.x - it.rect.x + "px " + (a.y - it.rect.y) + "px";
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

        for (let i = 0; i < s.items.length; i++) {
            s.items[i].node.style.removeProperty("transform");
            s.items[i].node.style.removeProperty("transform-origin");
        }
        if (Math.abs(f - 1) < 0.001) {
            return;
        }
        window.__deck.send("Interaction", {
            kind: "ScaleElements",
            element_ids: s.items.map(function (it) {
                return it.id;
            }),
            factor: f,
            anchor: { x: s.anchor.x, y: s.anchor.y },
        });
    }

    const ALL_TYPES = ["text", "image", "shape", "media", "group", "table", "embed"];
    const NON_GROUP_TYPES = ["text", "image", "shape", "media", "table", "embed"];

    const BOXY_TYPES = ["text", "image", "shape", "media", "table"];
    const TEXT_TYPES = ["text", "table"];

    function segIcon(d) {
        return (
            '<svg width="15" height="15" viewBox="0 0 24 24" fill="none"' +
            ' stroke="currentColor" stroke-width="1.9" stroke-linecap="round"' +
            ' stroke-linejoin="round"><path d="' +
            d +
            '"/></svg>'
        );
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
            id: "presets",
            label: "Presets",
            appliesTo: ALL_TYPES,
            fields: [
                {
                    prop: "preset",
                    label: "Style preset",
                    kind: "presets",
                    full: true,
                    composite: true,
                },
            ],
        },
        {
            id: "transform",
            label: "Transform",
            appliesTo: NON_GROUP_TYPES,
            fields: [
                { prop: "x", label: "X", kind: "number", suffix: "px" },
                { prop: "y", label: "Y", kind: "number", suffix: "px" },
                {
                    prop: "size",
                    label: "Size",
                    kind: "size-row",
                    full: true,
                    composite: true,
                },
                {
                    prop: "rotation",
                    label: "Rotation",
                    kind: "rotation-deg",
                    suffix: "°",
                    icon: "rotation",
                },
                {
                    prop: "opacity",
                    label: "Opacity",
                    kind: "number",
                    suffix: "%",
                    percent: true,
                    icon: "opacity",
                },
            ],
        },
        {
            id: "fill",
            label: "Fill",
            appliesTo: BOXY_TYPES,
            fields: [
                {
                    prop: "background-color",
                    label: "Fill",
                    kind: "swatch",
                    full: true,
                    composite: true,
                },
                {
                    prop: "background-image",
                    label: "Image",
                    kind: "fill-image",
                    full: true,
                    composite: true,
                },
                {
                    prop: "background-size",
                    label: "Object fit",
                    kind: "object-fit",
                    full: true,
                    composite: true,
                },
            ],
        },
        {
            id: "border",
            label: "Border",
            appliesTo: BOXY_TYPES,
            fields: [
                {
                    prop: "border-style",
                    label: "Style",
                    kind: "border-style",
                    full: true,
                    composite: true,
                },
                {
                    prop: "border-width",
                    label: "Width",
                    kind: "cluster",
                    full: true,
                    composite: true,
                    cluster: "width",
                },
                {
                    prop: "border-color",
                    label: "Color",
                    kind: "swatch",
                    full: true,
                    composite: true,
                },
                {
                    prop: "border-radius",
                    label: "Corner radius",
                    kind: "cluster",
                    full: true,
                    composite: true,
                    cluster: "radius",
                },
            ],
        },
        {
            id: "shadow",
            label: "Shadow",
            appliesTo: BOXY_TYPES,
            fields: [
                {
                    prop: "box-shadow",
                    label: "Shadow",
                    kind: "shadow",
                    full: true,
                    composite: true,
                    noLabel: true,
                },
            ],
        },
        {
            id: "typography",
            label: "Typography",
            appliesTo: TEXT_TYPES,
            fields: [
                {
                    prop: "font-family",
                    label: "Font",
                    kind: "font-combo",
                    full: true,
                    composite: true,
                },
                {
                    prop: "font-size",
                    label: "Size",
                    kind: "number",
                    unit: "px",
                    unitSelect: true,
                    icon: "fontSize",
                },
                {
                    prop: "font-weight",
                    label: "Weight",
                    kind: "number",
                    suffix: "",
                    icon: "fontWeight",
                },
                {
                    prop: "line-height",
                    label: "Line Height",
                    kind: "number",
                    suffix: "",
                    icon: "lineHeight",
                },
                {
                    prop: "letter-spacing",
                    label: "Letter Spacing",
                    kind: "number",
                    unit: "px",
                    unitSelect: true,
                    icon: "letterSpacing",
                },
                {
                    prop: "text-align",
                    label: "Alignment",
                    kind: "segment",
                    full: true,
                    options: [
                        { value: "left", icon: ALIGN_ICONS.left, tip: "Align left" },
                        { value: "center", icon: ALIGN_ICONS.center, tip: "Center" },
                        { value: "right", icon: ALIGN_ICONS.right, tip: "Align right" },
                        { value: "justify", icon: ALIGN_ICONS.justify, tip: "Justify" },
                    ],
                },
                {
                    prop: "justify-content",
                    label: "Vertical",
                    kind: "segment",
                    full: true,
                    options: [
                        { value: "flex-start", icon: VALIGN_ICONS.top, tip: "Top" },
                        { value: "center", icon: VALIGN_ICONS.middle, tip: "Middle" },
                        { value: "flex-end", icon: VALIGN_ICONS.bottom, tip: "Bottom" },
                    ],
                },
                {
                    prop: "text-style",
                    label: "Style",
                    kind: "text-style",
                    full: true,
                    readonly: true,
                },
                { prop: "color", label: "Color", kind: "color", full: true },
            ],
        },

        {
            id: "flexbox",
            label: "Flexbox",
            appliesTo: ["group"],
            custom: "group-flex-section",
        },
        {
            id: "custom-css",
            label: "Custom CSS",
            appliesTo: ALL_TYPES,
            custom: "inspector-custom",
        },
        {
            id: "animations",
            label: "Animations",
            appliesTo: ALL_TYPES,
            custom: "animations-section",
        },
    ];

    const inspectorInputs = {};

    const inspectorPending = new Set();

    const textStyleControls = [];

    const compositeControls = [];

    let sizeRatioLinked = false;

    let availableFonts = [];

    function borderLine(dash) {
        const da = dash ? ' stroke-dasharray="' + dash + '"' : "";
        const cap = dash === "2 4" ? ' stroke-linecap="round"' : "";
        return (
            '<svg width="26" height="2" viewBox="0 0 26 2"><line x1="1" y1="1"' +
            ' x2="25" y2="1" stroke="currentColor" stroke-width="2"' +
            da +
            cap +
            "/></svg>"
        );
    }
    const BORDER_STYLE_OPTIONS = [
        { value: "none", icon: "None", tip: "No border" },
        { value: "solid", icon: borderLine(""), tip: "Solid" },
        { value: "dashed", icon: borderLine("5 4"), tip: "Dashed" },
        { value: "dotted", icon: borderLine("2 4"), tip: "Dotted" },
    ];

    const OBJECT_FIT_OPTIONS = [
        { value: "100% 100%", icon: "Fill", tip: "Stretch to fill" },
        { value: "cover", icon: "Cover", tip: "Cover the box" },
        { value: "contain", icon: "Contain", tip: "Fit inside the box" },
        { value: "auto", icon: "Fit", tip: "Natural size" },
    ];

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
                {
                    prop: "border-top-left-radius",
                    label: "TL",
                    tip: "Top-left",
                    icon: "cornerTL",
                },
                {
                    prop: "border-top-right-radius",
                    label: "TR",
                    tip: "Top-right",
                    icon: "cornerTR",
                },
                {
                    prop: "border-bottom-right-radius",
                    label: "BR",
                    tip: "Bottom-right",
                    icon: "cornerBR",
                },
                {
                    prop: "border-bottom-left-radius",
                    label: "BL",
                    tip: "Bottom-left",
                    icon: "cornerBL",
                },
            ],
            parse: function (decls) {
                const r = window.__style.parseRadius(decls);
                return [r.tl, r.tr, r.br, r.bl];
            },
        },
    };

    const TEXT_STYLE_BUTTONS = [
        { prop: "font-weight", on: "700", min: 600, glyph: "B", cls: "b", tip: "Bold" },
        { prop: "font-style", on: "italic", glyph: "I", cls: "i", tip: "Italic" },
        {
            prop: "text-decoration",
            on: "underline",
            list: true,
            glyph: "U",
            cls: "u",
            tip: "Underline",
        },
        {
            prop: "text-decoration",
            on: "line-through",
            list: true,
            glyph: "S",
            cls: "s",
            tip: "Strikethrough",
        },
    ];

    const KNOWN_PROPS = {
        position: 1,
        display: 1,
        left: 1,
        top: 1,
        right: 1,
        bottom: 1,
        width: 1,
        height: 1,
        transform: 1,
        opacity: 1,
        "z-index": 1,
        "background-color": 1,
        border: 1,
        "border-radius": 1,
        "box-shadow": 1,
        "background-image": 1,
        "background-size": 1,
        "background-repeat": 1,
        "background-position": 1,
        "border-style": 1,
        "border-color": 1,
        "border-width": 1,
        "border-top-width": 1,
        "border-right-width": 1,
        "border-bottom-width": 1,
        "border-left-width": 1,
        "border-top-left-radius": 1,
        "border-top-right-radius": 1,
        "border-bottom-right-radius": 1,
        "border-bottom-left-radius": 1,
        "font-family": 1,
        "font-size": 1,
        "font-weight": 1,
        color: 1,
        "text-align": 1,
        "justify-content": 1,
        "line-height": 1,
        "letter-spacing": 1,
        "font-style": 1,
        "text-decoration": 1,

        "white-space": 1,
    };

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

    function buildField(field) {
        const wrap = document.createElement("div");
        wrap.className = "inspector__field";
        if (field.full) {
            wrap.classList.add("inspector__field--full");
        }
        const label = document.createElement("label");
        label.className = "inspector__field-label";
        label.textContent =
            field.label +
            (field.suffix && !field.icon ? " (" + field.suffix.trim() + ")" : "");
        const control = buildFieldControl(field);
        control.dataset.prop = field.prop;
        control.dataset.kind = field.kind;

        if (field.noLabel) {
            const id = "inspector-input-" + field.prop.replace(/[^a-z0-9]/gi, "-");
            control.id = id;
            inspectorInputs[field.prop] = control;
            wrap.appendChild(control);
            return wrap;
        }

        if (field.unit) {
            control.dataset.unit = field.unit;
        }
        if (field.percent) {
            control.dataset.percent = "1";
        }

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

    const UNIT_CHEVRON =
        '<svg width="9" height="9" viewBox="0 0 24 24" fill="none"' +
        ' stroke="currentColor" stroke-width="3" stroke-linecap="round"' +
        ' stroke-linejoin="round"><path d="M6 9l6 6 6-6"/></svg>';

    const FIELD_ICONS = {
        opacity:
            '<circle cx="12" cy="12" r="8"/>' +
            '<path d="M12 4a8 8 0 0 0 0 16z" fill="currentColor" stroke="none"/>',
        rotation: '<path d="M21 12a9 9 0 1 1-2.64-6.36"/><path d="M21 3v5h-5"/>',
        lineHeight: '<path d="M6 4v16M4 6l2-2 2 2M4 18l2 2 2-2M12 6h8M12 12h8M12 18h8"/>',
        letterSpacing: '<path d="M4 5v14M20 5v14M9 9l-2 3 2 3M15 9l2 3-2 3"/>',
        fontSize:
            '<path d="M3 19l4.5-12 4.5 12M4.6 15h5.8"/>' +
            '<path d="M14 19l3-8 3 8M15 16h4"/>',
        fontWeight: '<path d="M7 5h6a3.5 3.5 0 0 1 0 7H7zM7 12h7a3.5 3.5 0 0 1 0 7H7z"/>',
        cornerTL: '<path d="M19 5h-8a6 6 0 0 0-6 6v8"/>',
        cornerTR: '<path d="M5 5h8a6 6 0 0 1 6 6v8"/>',
        cornerBR: '<path d="M5 19h8a6 6 0 0 0 6-6V5"/>',
        cornerBL: '<path d="M19 19h-8a6 6 0 0 1-6-6V5"/>',
    };

    const FIELD_ICON_STROKE = { fontWeight: 3 };

    function fieldIconSvg(key) {
        const inner = FIELD_ICONS[key];
        if (!inner) {
            return "";
        }
        const sw = FIELD_ICON_STROKE[key] || 1.8;
        return (
            '<svg width="14" height="14" viewBox="0 0 24 24" fill="none"' +
            ' stroke="currentColor" stroke-width="' +
            sw +
            '" stroke-linecap="round"' +
            ' stroke-linejoin="round">' +
            inner +
            "</svg>"
        );
    }

    function makeFieldIcon(key) {
        const span = document.createElement("span");
        span.className = "inspector__field-icon";
        span.innerHTML = fieldIconSvg(key);
        return span;
    }

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
        if (opts.className) {
            trigger.classList.add(opts.className);
        }
        const lab = document.createElement("span");
        lab.className =
            variant === "chip"
                ? "inspector__unitchip-label"
                : "inspector__dropdown-label";
        const caret = document.createElement("span");
        caret.className =
            variant === "chip"
                ? "inspector__unitchip-caret"
                : "inspector__dropdown-caret";
        caret.innerHTML = UNIT_CHEVRON;
        trigger.appendChild(lab);
        trigger.appendChild(caret);
        const menu = document.createElement("div");
        menu.className = "dropdown-menu";

        function labelFor(v) {
            for (let i = 0; i < options.length; i++) {
                if (options[i].value === v) {
                    return options[i].label;
                }
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
                    b.addEventListener("click", function () {
                        select(v);
                        close();
                    });
                })(o.value);
                menu.appendChild(b);
            }
        }
        function select(v) {
            value = String(v);
            relabel();
            if (opts.onChange) {
                opts.onChange(value);
            }
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
            if (!menu.contains(e.target) && !trigger.contains(e.target)) {
                close();
            }
        }
        function onEsc(e) {
            if (e.key === "Escape") {
                e.preventDefault();
                close();
            }
        }
        trigger.addEventListener("click", function (e) {
            e.stopPropagation();
            if (menu.classList.contains("dropdown-menu--open")) {
                close();
            } else {
                open();
            }
        });
        Object.defineProperty(trigger, "value", {
            get: function () {
                return value;
            },
            set: function (v) {
                value = v == null ? "" : String(v);
                relabel();
            },
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

    function makeUnitChip(getUnit, setUnit) {
        console.assert(
            typeof getUnit === "function" && typeof setUnit === "function",
            "unit chip needs get/set",
        );
        const chip = makeDropdown({
            label: "Unit",
            variant: "chip",
            options: UNITS.map(function (u) {
                return { value: u, label: u };
            }),
            value: getUnit() || "px",
            onChange: function (u) {
                setUnit(u);
            },
        });
        chip.sync = function () {
            chip.value = getUnit() || "px";
        };
        return chip;
    }

    function makeNumberField(field) {
        const box = document.createElement("div");
        box.className = "inspector__unitfield";
        const input = document.createElement("input");
        input.className = "inspector__input";
        input.spellcheck = false;
        if (field.icon) {
            box.appendChild(makeFieldIcon(field.icon));
        }
        input.addEventListener("change", function () {
            box.dispatchEvent(new Event("change"));
        });
        input.addEventListener("keydown", function (e) {
            if (e.key === "Enter") {
                e.preventDefault();
                input.blur();
            }
        });
        box.appendChild(input);
        if (field.unitSelect) {
            let unit = field.unit || "px";
            box.dataset.unit = unit;
            const chip = makeUnitChip(
                function () {
                    return unit;
                },
                function (u) {
                    unit = u;
                    box.dataset.unit = u;
                    box.dispatchEvent(new Event("change"));
                },
            );
            box.appendChild(chip);
            box.setUnit = function (u) {
                unit = u || "px";
                box.dataset.unit = unit;
                chip.sync();
            };
        } else if (field.suffix) {
            const sfx = document.createElement("span");
            sfx.className = "inspector__field-suffix";
            sfx.textContent = field.suffix;
            box.appendChild(sfx);
        }
        Object.defineProperty(box, "value", {
            get: function () {
                return input.value;
            },
            set: function (v) {
                input.value = v;
            },
        });
        return box;
    }

    function buildFieldControl(field) {
        if (
            (field.kind === "number" || field.kind === "rotation-deg") &&
            (field.icon || field.unitSelect)
        ) {
            return makeNumberField(field);
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
            get: function () {
                return current;
            },
            set: function (v) {
                current = v === null || v === undefined ? "" : String(v);
                for (let i = 0; i < box.children.length; i++) {
                    const on = box.children[i].dataset.value === current;
                    box.children[i].setAttribute("aria-pressed", on ? "true" : "false");
                }
            },
        });
        return box;
    }

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

        function render() {
            const hex = stateHex();
            const css = window.__style.composeRgba(hex, state.a);
            if (state.none) {
                box.classList.add("inspector__color--none");
                fill.style.background = "";
                if (document.activeElement !== hexInput) {
                    hexInput.value = "None";
                }
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

        function applyColor(hex, alpha, setAlpha) {
            const rgb = window.__style.hexToRgb(hex);
            const hsl = window.__style.rgbToHsl(rgb.r, rgb.g, rgb.b);
            if (hsl.s > 0) {
                state.h = hsl.h;
            }
            state.s = hsl.s;
            state.l = hsl.l;
            if (setAlpha) {
                state.a = alpha;
            }
            state.none = false;
        }
        function currentCss() {
            if (state.none) {
                return "";
            }
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
            if (
                !pop.el.contains(e.target) &&
                e.target !== swatch &&
                !swatch.contains(e.target) &&
                e.target !== gap
            ) {
                closePop();
            }
        }
        function onEsc(e) {
            if (e.key === "Escape") {
                e.preventDefault();
                closePop();
                closeAlpha();
            }
        }
        function openAlpha() {
            if (state.none) {
                return;
            }
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

        function togglePop(e) {
            e.stopPropagation();
            if (pop.el.classList.contains("colorpop--open")) {
                closePop();
            } else {
                openPop();
            }
        }
        swatch.addEventListener("click", togglePop);
        gap.addEventListener("click", togglePop);

        pct.addEventListener("click", function (e) {
            e.stopPropagation();
            if (alphaPop.el.classList.contains("colorpop--open")) {
                closeAlpha();
            } else {
                openAlpha();
            }
        });

        hexInput.addEventListener("change", function () {
            const raw = hexInput.value.trim().toLowerCase();
            if (raw === "" || raw === "none") {
                setNone();
                return;
            }
            const m8 = /^#?([0-9a-f]{6})([0-9a-f]{2})$/.exec(raw);
            if (m8) {
                const alpha = Math.round((parseInt(m8[2], 16) / 255) * 100);
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
            if (e.key === "Enter") {
                e.preventDefault();
                hexInput.blur();
            }
        });

        Object.defineProperty(box, "value", {
            get: currentCss,
            set: function (v) {

                if (
                    pop.el.classList.contains("colorpop--open") ||
                    alphaPop.el.classList.contains("colorpop--open")
                ) {
                    return;
                }
                const s = String(v == null ? "" : v)
                    .trim()
                    .toLowerCase();
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

    function buildColorPopover(state, render, commit, setNone) {
        const el = document.createElement("div");
        el.className = "colorpop";
        const noneBtn = document.createElement("button");
        noneBtn.type = "button";
        noneBtn.className = "colorpop__none";
        noneBtn.textContent = "None";
        noneBtn.addEventListener("click", function () {
            setNone();
        });
        el.appendChild(noneBtn);
        const wheel = document.createElement("div");
        wheel.className = "colorpop__wheel";

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
            if (e.key === "Enter") {
                e.preventDefault();
                hexInput.blur();
            }
        });

        function renderPopover(st, hex, css) {
            setColorAxis(h, st.h, Math.round(st.h));
            setColorAxis(s, st.s, Math.round(st.s));
            setColorAxis(l, st.l, Math.round(st.l));
            setColorAxis(a, st.a, Math.round(st.a));
            h.input.style.background =
                "linear-gradient(to right," +
                " #f00, #ff0, #0f0, #0ff, #00f, #f0f, #f00)";
            s.input.style.background =
                "linear-gradient(to right, hsl(" +
                st.h +
                ", 0%, 50%), hsl(" +
                st.h +
                ", 100%, 50%))";
            l.input.style.background =
                "linear-gradient(to right, #000, hsl(" +
                st.h +
                ", " +
                st.s +
                "%, 50%), #fff)";
            a.input.style.background =
                "linear-gradient(to right," + " transparent, " + hex + ")";
            lum.style.background = st.l >= 50 ? "#fff" : "#000";
            lum.style.opacity = String(Math.abs(st.l - 50) / 50);
            const R = wheel.offsetWidth / 2 || 100;
            const rad = (st.s / 100) * R;
            const theta = (st.h * Math.PI) / 180;
            dot.style.left = R + rad * Math.sin(theta) + "px";
            dot.style.top = R - rad * Math.cos(theta) + "px";
            dot.style.background = css;
            if (document.activeElement !== hexInput) {
                hexInput.value = hex.toUpperCase();
            }
        }
        return { el: el, render: renderPopover };
    }

    function buildAlphaPopover(state, render, commit) {
        const el = document.createElement("div");
        el.className = "colorpop colorpop--alpha";
        const slider = makeColorSlider(
            "A",
            100,
            function (e) {
                state.a = Number(e.target.value);
                render();
            },
            commit,
        );
        el.appendChild(slider.row);
        function renderAlpha(st, hex) {
            setColorAxis(slider, st.a, Math.round(st.a));
            slider.input.style.background =
                "linear-gradient(to right," + " transparent, " + hex + ")";
        }
        return { el: el, render: renderAlpha };
    }

    function setColorAxis(slider, value, shown) {
        if (document.activeElement !== slider.input) {
            slider.input.value = String(Math.round(value));
        }
        slider.readout.textContent = String(shown);
    }

    function wireColorWheel(wheel, state, render, commit) {
        function fromPointer(e) {
            const r = wheel.getBoundingClientRect();
            const R = r.width / 2;
            const dx = e.clientX - r.left - R;
            const dy = e.clientY - r.top - R;
            const dist = Math.min(Math.sqrt(dx * dx + dy * dy), R);
            let deg = (Math.atan2(dx, -dy) * 180) / Math.PI;
            if (deg < 0) {
                deg += 360;
            }
            state.h = deg;
            state.s = R > 0 ? (dist / R) * 100 : 0;
            state.none = false;
            render();
        }
        function onMove(e) {
            fromPointer(e);
        }
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

    const LINK_ICON =
        '<svg width="12" height="12" viewBox="0 0 24 24" fill="none"' +
        ' stroke="currentColor" stroke-width="2" stroke-linecap="round"' +
        ' stroke-linejoin="round"><path d="M10 13a4 4 0 0 0 5.6.5l2.5-2.5a4 4 0 0' +
        ' 0-5.6-5.6L11 7"/><path d="M14 11a4 4 0 0 0-5.6-.5L5.9 13a4 4 0 0 0 5.6' +
        ' 5.6L13 17"/></svg>';

    function numOr0(v) {
        const n = Number(
            String(v == null ? "" : v)
                .replace(/px$/i, "")
                .trim(),
        );
        return isFinite(n) ? n : 0;
    }
    function setIfIdle(input, value) {
        if (input && document.activeElement !== input) {
            input.value = value;
        }
    }

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
        Object.defineProperty(box, "value", {
            get: function () {
                return "";
            },
            set: function () {},
        });
        compositeControls.push(box);
        return box;
    }

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

    function makeClusterCell(cell) {
        const wrap = document.createElement("div");
        wrap.className = "inspector__cluster-cell tt";
        wrap.setAttribute("data-tip", cell.tip || "");
        wrap.setAttribute("data-key", "");
        const lab = document.createElement("span");
        lab.className = "inspector__cluster-cell-label";
        if (cell.icon) {
            lab.innerHTML = fieldIconSvg(cell.icon);
        } else {
            lab.textContent = cell.label;
        }
        const input = document.createElement("input");
        input.className = "inspector__cluster-input";
        input.spellcheck = false;
        wrap.appendChild(lab);
        wrap.appendChild(input);
        return { wrap: wrap, input: input };
    }

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
        const chip = makeUnitChip(
            function () {
                return unit;
            },
            function (u) {
                unit = u;
                recommitAll();
            },
        );
        function emitAll(v) {
            for (let j = 0; j < inputs.length; j++) {
                inputs[j].value = String(v);
                sendPropertyChanged(spec.cells[j].prop, v + unit);
            }
        }

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
                    if (e.key === "Enter") {
                        e.preventDefault();
                        input.blur();
                    }
                });
            })(i, cell.input);
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

            let found = "";
            for (let j = 0; j < spec.cells.length && found === ""; j++) {
                found = window.__style.splitLength(decls[spec.cells[j].prop] || "").unit;
            }
            unit = found || "px";
            chip.sync();
        };
        box.dataset.linked = "true";
        Object.defineProperty(box, "value", {
            get: function () {
                return "";
            },
            set: function () {},
        });
        compositeControls.push(box);
        return box;
    }

    function makeShadowControl() {
        const box = document.createElement("div");
        box.className = "inspector__shadow";
        const grid = document.createElement("div");
        grid.className = "inspector__shadow-grid";
        const order = [
            { key: "x", label: "X" },
            { key: "y", label: "Y" },
            { key: "blur", label: "Blur" },
            { key: "spread", label: "Spread" },
        ];
        const inputs = {};
        const color = makeColorControl();
        color.classList.add("inspector__shadow-color");
        function commit() {
            sendPropertyChanged(
                "box-shadow",
                window.__style.composeBoxShadow({
                    x: numOr0(inputs.x.value),
                    y: numOr0(inputs.y.value),
                    blur: numOr0(inputs.blur.value),
                    spread: numOr0(inputs.spread.value),
                    color: color.value,
                }),
            );
        }
        for (let i = 0; i < order.length; i++) {
            const cell = makeShadowCell(order[i].label);
            inputs[order[i].key] = cell.input;
            grid.appendChild(cell.wrap);
            cell.input.addEventListener("change", commit);
            (function (input) {
                input.addEventListener("keydown", function (e) {
                    if (e.key === "Enter") {
                        e.preventDefault();
                        input.blur();
                    }
                });
            })(cell.input);
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
        Object.defineProperty(box, "value", {
            get: function () {
                return "";
            },
            set: function () {},
        });
        compositeControls.push(box);
        return box;
    }

    function parseAssetId(bg) {
        const m = /var\(--asset-([^)]+)\)/.exec(String(bg == null ? "" : bg));
        return m ? m[1] : "";
    }

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
        clear.innerHTML =
            '<svg width="13" height="13" viewBox="0 0 24 24" fill="none"' +
            ' stroke="currentColor" stroke-width="2.2" stroke-linecap="round">' +
            '<path d="M6 6l12 12M18 6 6 18"/></svg>';
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
        Object.defineProperty(box, "value", {
            get: function () {
                return "";
            },
            set: function () {},
        });
        compositeControls.push(box);
        return box;
    }

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

    function fontUnquote(v) {
        const s = String(v == null ? "" : v).trim();
        const first = s.split(",")[0].trim();
        return first.replace(/^["']|["']$/g, "").trim();
    }

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
                pop.children[i].setAttribute(
                    "aria-selected",
                    i === highlight ? "true" : "false",
                );
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
            if (
                shown < matches.length &&
                pop.scrollTop + pop.clientHeight >= pop.scrollHeight - 48
            ) {
                appendChunk();
            }
        });
        input.addEventListener("input", renderPop);
        input.addEventListener("focus", renderPop);
        input.addEventListener("keydown", function (e) {
            onFontComboKey(
                e,
                pop,
                input,
                commit,
                function (n) {
                    highlight = n;
                    reflectHighlight();
                },
                function () {
                    return highlight;
                },
                renderPop,
                closePop,
            );
        });
        input.addEventListener("blur", function () {
            window.setTimeout(closePop, 120);
            commit(input.value);
        });
        box.syncDecls = function (decls) {
            const raw = String(decls["font-family"] || "").trim();

            const isDefault = raw === "" || raw.indexOf("var(") === 0;
            current = isDefault ? "" : fontUnquote(raw);
            if (document.activeElement !== input) {
                input.value = current;
            }
        };
        Object.defineProperty(box, "value", {
            get: function () {
                return "";
            },
            set: function () {},
        });
        compositeControls.push(box);
        return box;
    }

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

    const PRESET_EXCLUDE = {
        left: 1,
        top: 1,
        width: 1,
        height: 1,
        transform: 1,
        opacity: 1,
        "z-index": 1,
        position: 1,
        display: 1,
        "background-image": 1,
        "background-size": 1,
        "background-repeat": 1,
        "background-position": 1,
    };

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

    function selectedElementType() {
        if (currentSelectionIds.length !== 1) {
            return "";
        }
        const el = findElement(currentSelectionIds[0]);
        return (el && el.dataset.elementType) || "";
    }

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
        currentGlobalsCss = window.__preset.upsertPresetRule(
            currentGlobalsCss,
            type,
            className,
            decls,
        );
        window.__deck.send("Interaction", {
            kind: "GlobalsCssEditRequested",
            new_css: currentGlobalsCss,
        });
        nameInput.value = "";
    }

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
            const presets = window.__preset
                .parsePresets(currentGlobalsCss)
                .filter(function (p) {
                    return p.type === currentType;
                });
            select.setPlaceholder(
                presets.length ? "Apply preset…" : "No presets for this type",
            );
            select.setOptions(
                presets.map(function (p) {
                    return { value: p.className, label: p.className };
                }),
            );
            select.value = "";
        }
        saveBtn.addEventListener("click", function () {
            onSavePreset(nameInput);
        });
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
        Object.defineProperty(box, "value", {
            get: function () {
                return "";
            },
            set: function () {},
        });
        compositeControls.push(box);
        return box;
    }

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
            if (e.key === "Enter") {
                e.preventDefault();
                input.blur();
            }
        });
        wrap.appendChild(lab);
        wrap.appendChild(input);
        return { wrap: wrap, input: input };
    }

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
        Object.defineProperty(box, "value", {
            get: function () {
                return "";
            },
            set: function () {},
        });
        return box;
    }

    function makeTextStyleControl() {
        const box = document.createElement("div");
        box.className = "inspector__tstyle";
        box.setAttribute("role", "group");
        box._decls = {};
        for (let i = 0; i < TEXT_STYLE_BUTTONS.length; i++) {
            const spec = TEXT_STYLE_BUTTONS[i];
            const b = document.createElement("button");
            b.type = "button";
            b.className =
                "inspector__tstyle-btn inspector__tstyle-btn--" + spec.cls + " tt";
            b.textContent = spec.glyph;
            b.setAttribute("aria-pressed", "false");
            b.setAttribute("data-tip", spec.tip);
            b.setAttribute("data-key", "");
            b.addEventListener("click", function () {
                toggleTextStyle(box, spec);
            });
            box.appendChild(b);
        }
        Object.defineProperty(box, "value", {
            get: function () {
                return "";
            },
            set: function () {

            },
        });
        box.syncDecls = function (decls) {
            syncTextStyle(box, decls || {});
        };
        textStyleControls.push(box);
        return box;
    }

    function isTextStyleActive(decls, spec) {
        const cur = String(decls[spec.prop] || "").trim();
        if (spec.list) {
            return cur.split(/\s+/).indexOf(spec.on) >= 0;
        }
        if (spec.min) {
            const n = parseInt(cur, 10);
            return isFinite(n) ? n >= spec.min : cur === spec.on;
        }
        return cur === spec.on;
    }

    // ponytail: writes box._decls optimistically so clicks inside one round trip compose;
    // a rejected commit leaves it wrong until the next syncTextStyle corrects it.
    function toggleTextStyle(box, spec) {
        const decls = box._decls || {};
        const active = isTextStyleActive(decls, spec);
        let next;
        if (spec.list) {
            const cur = String(decls[spec.prop] || "").trim();
            const tokens = cur === "" ? [] : cur.split(/\s+/);
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
        box._decls = Object.assign({}, decls);
        box._decls[spec.prop] = next;
        sendPropertyChanged(spec.prop, next);
    }

    function syncTextStyle(box, decls) {
        box._decls = decls;
        for (let i = 0; i < TEXT_STYLE_BUTTONS.length; i++) {
            const on = isTextStyleActive(decls, TEXT_STYLE_BUTTONS[i]);
            box.children[i].setAttribute("aria-pressed", on ? "true" : "false");
        }
    }

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
        rm.innerHTML =
            '<svg width="13" height="13" viewBox="0 0 24 24" fill="none"' +
            ' stroke="currentColor" stroke-width="2.2" stroke-linecap="round">' +
            '<path d="M6 6l12 12M18 6 6 18"/></svg>';
        rm.addEventListener("click", function () {
            sendPropertyChanged(prop, "");
        });
        row.appendChild(p);
        row.appendChild(colon);
        row.appendChild(v);
        row.appendChild(rm);
        return row;
    }

    function onInspectorFieldCommit(e) {
        const input = e.target;
        if (!input || input.readOnly) {
            return;
        }
        const prop = input.dataset.prop;
        const kind = input.dataset.kind || "css";
        const raw = input.value;
        let wire = encodeForWire(kind, raw);

        if (kind === "number" && input.dataset.unit && wire !== null && wire !== "") {
            wire = wire + input.dataset.unit;
        }

        if (kind === "number" && input.dataset.percent && wire !== null && wire !== "") {
            wire = String(Number(wire) / 100);
        }
        if (wire === null) {

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
        const ratio = isWidth ? curH / curW : curW / curH;
        const other = String(Math.round(next * ratio));
        const otherProp = isWidth ? "height" : "width";
        if (inspectorInputs[otherProp]) {
            inspectorInputs[otherProp].value = other;
        }
        sendPropertyChanged(otherProp, other);
    }

    function encodeForWire(kind, raw) {

        if (
            kind === "css" ||
            kind === "select" ||
            kind === "color" ||
            kind === "segment"
        ) {
            return String(raw);
        }
        const trimmed = String(raw).trim();
        if (trimmed === "") {
            return "";
        }

        const numeric = trimmed
            .replace(/(px|em|rem|pt|in|pc|cm|mm|deg|rad|°|%)\s*$/i, "")
            .trim();
        const n = Number(numeric);
        if (!isFinite(n)) {
            return null;
        }
        if (kind === "rotation-deg") {
            return String((n * Math.PI) / 180);
        }
        return String(n);
    }

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

    function cssValueRejected(property, value) {
        const v = String(value).trim();
        if (v === "") {
            return false;
        }
        if (
            window.CSS &&
            typeof window.CSS.supports === "function" &&
            !window.CSS.supports(property, v)
        ) {
            showToast(
                "Invalid value for " + property,
                property + ": '" + v + "' was rejected",
            );
            return true;
        }
        return false;
    }

    function sendPropertyChanged(prop, value) {

        if (
            tableCellSel &&
            focusedTableId() === tableCellSel.elementId &&
            tableCellSel.cells.length > 0
        ) {
            window.__deck.send("Interaction", {
                kind: "CellStyleChanged",
                element_id: tableCellSel.elementId,
                cells: tableCellSel.cells.map(function (rc) {
                    return [rc[0], rc[1]];
                }),
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

    function refreshInspector() {
        const subtitle = document.getElementById("inspector-target");
        if (!subtitle) {
            return;
        }
        refreshCropBox();
        refreshTableBox();
        setAlignBoxVisible(selectedGuideId === null && currentSelectionIds.length >= 2);

        if (selectedGuideId !== null) {
            clearInspectorInputs();
            showGuideInspector();
            return;
        }
        hideGuideInspector();

        if (currentSelectionIds.length === 0) {
            clearInspectorInputs();
            const slideMode = currentMode === "slide";

            subtitle.textContent = slideMode ? "Slide" : "Layout";
            setInspectorIcon(slideMode ? "slide" : "layout");
            setSlideBoxVisible(true);
            setElementInspectorVisible(false, null);
            renderSlideBox();
            return;
        }
        setSlideBoxVisible(false);
        if (currentSelectionIds.length > 1) {
            subtitle.textContent = currentSelectionIds.length + " selected";
            setInspectorIcon("multi");
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
        setInspectorIcon(type);
        setElementInspectorVisible(true, type);
        const decls = parseStyleAttr(el.getAttribute("style") || "");
        populateInspector(decls);
        refreshGroupFlexSection();
        inspectorPending.clear();
    }

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

    function setSlideBoxVisible(show) {
        const el = document.getElementById("slide-box");
        if (el) {
            el.style.display = show ? "block" : "none";
        }
    }

    function renderSlideBox() {
        wireSlideBox();
        const layoutMode = currentMode === "layout";

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
            bg.value = isHexColor((data && data.background) || "")
                ? data.background
                : "#000000";
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

        const bgImgPick = document.getElementById("slide-bg-image");
        const bgImgClear = document.getElementById("slide-bg-image-clear");
        if (bgImgPick) {
            const raw = (data && data.background_image) || "";
            const m = /var\(--asset-([^)]+)\)/.exec(raw);
            const url = m ? cropImageUrl(m[1]) : "";
            if (url) {
                bgImgPick.style.backgroundImage = 'url("' + url + '")';
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
            layout.setOptions(
                layouts.map(function (l) {
                    return { value: l.id, label: l.name || l.id };
                }),
            );
            layout.value = (data && data.layout_id) || "";
        }

        if (!layoutMode) {
            renderSlideTransition(data);
        }

        if (!layoutMode) {
            renderSlideAnimations();
        }
    }

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

    function wireSlideBox() {
        const box = document.getElementById("slide-box");
        if (!box || box.dataset.wired) {
            return;
        }
        box.dataset.wired = "1";

        const header = document.getElementById("slide-box-header");
        if (header) {
            header.addEventListener("click", function () {
                const collapsed = box.dataset.collapsed === "true";
                box.dataset.collapsed = collapsed ? "false" : "true";
            });
        }

        const mount = document.getElementById("slide-bg-mount");
        const bg = makeColorControl();
        bg.id = "slide-bg";
        if (mount) {
            mount.appendChild(bg);
        }
        bg.addEventListener("change", function () {
            if (bg.value === "") {
                showToast(
                    "Slide background can't be None",
                    "Pick a colour or set a background image",
                );
                renderSlideBox();
                return;
            }
            window.__deck.send("Interaction", {
                kind: "SetSlideBackgroundRequested",
                background: bg.value,
            });
        });

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
                window.__deck.send("Interaction", {
                    kind: "SetSlideBackgroundImageCleared",
                });
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
                    kind: "SetSlideLayoutRequested",
                    layout_id: layout.value,
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
                    kind: "SetSlideNotesRequested",
                    notes: notes.value,
                });
            });
        }
        wireSlideTransition();
    }

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
                options: SLIDE_TRANSITIONS.map(function (t) {
                    return { value: t, label: t };
                }),
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

    function sendSlideTransition() {
        window.__deck.send("Interaction", {
            kind: "SetSlideTransitionRequested",
            transition: readSlideTransition(),
        });
    }

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
                    kind: "SetDeckTitleRequested",
                    title: input.value,
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

            window.requestAnimationFrame(function () {
                input.focus();
                input.select();
            });
        }
    }

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

    function populateInspector(decls) {
        setIfNotPending("x", stripPx(decls.left));
        setIfNotPending("y", stripPx(decls.top));
        setIfNotPending("width", stripPx(decls.width));
        setIfNotPending("height", stripPx(decls.height));
        setPercentNumber("opacity", decls.opacity, 100);
        setIfNotPending(
            "rotation",
            radiansToDegreesStr(extractRotationRad(decls.transform)),
        );
        setIfNotPending("z-index", decls["z-index"] || "");
        const cssOnly = [

            "font-weight",
            "color",
            "text-align",
            "justify-content",
            "line-height",
        ];
        for (let i = 0; i < cssOnly.length; i++) {
            const key = cssOnly[i];
            setIfNotPending(key, decls[key] || "");
        }

        setUnitNumber("font-size", decls["font-size"]);
        setUnitNumber("letter-spacing", decls["letter-spacing"], "0");

        for (let i = 0; i < textStyleControls.length; i++) {
            textStyleControls[i].syncDecls(decls);
        }
        for (let i = 0; i < compositeControls.length; i++) {
            compositeControls[i].syncDecls(decls);
        }
        renderCustomDeclarations(decls);
    }

    function setUnitNumber(prop, raw, defNum) {
        const box = inspectorInputs[prop];
        if (!box || inspectorPending.has(prop)) {
            return;
        }
        const parts = window.__style.splitLength(raw);
        if (box.setUnit) {
            box.setUnit(parts.unit || "px");
        }
        const input = box.querySelector(".inspector__input");
        if (document.activeElement === input) {
            return;
        }
        box.value = parts.num !== "" ? parts.num : defNum || "";
    }

    function setPercentNumber(prop, raw, def) {
        const box = inspectorInputs[prop];
        if (!box || inspectorPending.has(prop)) {
            return;
        }
        const input = box.querySelector(".inspector__input") || box;
        if (document.activeElement === input) {
            return;
        }
        const n = raw === "" || raw == null ? def : Math.round(parseFloat(raw) * 100);
        box.value = isFinite(n) ? String(n) : String(def);
    }

    function setIfNotPending(prop, value) {
        const input = inspectorInputs[prop];
        if (!input) {
            return;
        }

        if (inspectorPending.has(prop)) {
            return;
        }
        if (document.activeElement === input) {
            return;
        }
        input.value = value;
    }

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
        const deg = (rad * 180) / Math.PI;

        return String(Math.round(deg * 100) / 100);
    }

    let lastObjectTree = null;

    const collapsedGroups = new Set();

    const LONG_CLICK_MS = 500;
    const LONG_CLICK_MOVE_PX = 4;
    let longClickTimer = null;
    let longClickAnchor = null;

    const DRAG_TYPE = "application/x-carousel-element-id";

    let panelDragId = null;

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

        for (let i = tree.nodes.length - 1; i >= 0; i--) {
            host.appendChild(buildObjectNode(tree.nodes[i], 0));
        }
        updateObjectPanelSelection();
    }

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

    function toggleGroupCollapsed(id) {
        if (collapsedGroups.has(id)) {
            collapsedGroups.delete(id);
        } else {
            collapsedGroups.add(id);
        }
        renderObjectPanel(lastObjectTree);
    }

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

        row.addEventListener("mousedown", onPanelMouseDown);
        row.addEventListener("dragstart", onPanelDragStart);
        row.addEventListener("dragover", onPanelDragOver);
        row.addEventListener("dragleave", onPanelDragLeave);
        row.addEventListener("drop", onPanelDrop);
        row.addEventListener("dragend", onPanelDragEnd);
        row.addEventListener("dblclick", function (e) {

            e.preventDefault();
            editElementId(label, node.id);
        });

        wrap.appendChild(row);

        if (Array.isArray(node.children) && node.children.length > 0) {
            const kids = document.createElement("div");
            kids.className = "objects__children";
            for (let i = node.children.length - 1; i >= 0; i--) {
                kids.appendChild(buildObjectNode(node.children[i], depth + 1));
            }
            wrap.appendChild(kids);
        }
        return wrap;
    }

    function badgeGlyph(type) {
        switch (type) {
            case "text":
                return "T";
            case "shape":
                return "▭";
            case "group":
                return "▤";
            case "image":
                return "▣";
            case "media":
                return "▶";
            case "table":
                return "▦";
            case "embed":
                return "<>";
            case "slide":
                return "◻";
            case "layout":
                return "▨";
            case "guide":
                return "┼";
            case "multi":
                return "❖";
            default:
                return "?";
        }
    }

    function setInspectorIcon(kind) {
        const icon = document.getElementById("inspector-icon");
        if (!icon) {
            return;
        }
        icon.className = "objects__badge objects__badge--" + (kind || "unknown");
        icon.textContent = badgeGlyph(kind);
    }

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

    function onPanelMouseDown(e) {
        if (e.button !== 0) {
            return;
        }

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

    function floatingEdit(anchorNode, initialValue, commitFn) {
        if (!anchorNode || document.querySelector(".floating-edit")) {
            return;
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

    function editElementId(labelNode, elementId) {
        floatingEdit(labelNode, elementId, function (value) {
            window.__deck.send("Interaction", {
                kind: "ElementIdEditRequested",
                element_id: elementId,
                new_id: value,
            });
        });
    }

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

    function onPanelDragOver(e) {
        if (!panelDragId) {
            return;
        }
        const row = e.currentTarget;
        const targetId = row.dataset.elementId || "";
        if (targetId === panelDragId) {
            return;
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

        if (e.relatedTarget && row.contains(e.relatedTarget)) {
            return;
        }
        row.removeAttribute("data-drop-target");
    }

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

            if (containsDescendant(source.node, targetId)) {
                return null;
            }
            newParentId = targetId;
            displayIndex = target.node.children.length;
        } else {
            newParentId = target.parentId;
            // panel lists top-most first, so a row above the target is a later sibling
            displayIndex = zone === "before" ? target.index + 1 : target.index;
        }
        let position = displayIndex;
        if (source.parentId === newParentId && source.index < displayIndex) {
            position -= 1;
        }
        if (source.parentId === newParentId && source.index === position) {
            return null;
        }
        return { new_parent_id: newParentId, new_position: position };
    }

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

        const addImage = document.getElementById("tool-add-image");
        if (addImage) {
            const picker = document.createElement("input");
            picker.type = "file";
            picker.accept = "image/*";
            picker.style.display = "none";
            addImage.appendChild(picker);
            addImage.addEventListener("click", function () {
                picker.click();
            });
            picker.addEventListener("change", function () {
                const f = picker.files && picker.files[0];
                if (f) {
                    importImageFile(f, null);
                }
                picker.value = "";
            });
        }

        const undoBtn = document.getElementById("undo-btn");
        if (undoBtn) {
            undoBtn.addEventListener("click", function () {
                sendSyntheticKey("undo", {});
            });
        }
        const redoBtn = document.getElementById("redo-btn");
        if (redoBtn) {
            redoBtn.addEventListener("click", function () {
                sendSyntheticKey("redo", {});
            });
        }
    }

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
            ic.innerHTML =
                '<svg width="18" height="18" viewBox="0 0 24 24" fill="none"' +
                ' stroke="currentColor" stroke-width="1.7" stroke-linecap="round"' +
                ' stroke-linejoin="round">' +
                opt.icon +
                "</svg>";
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
            card.addEventListener("click", function () {
                onPick(opt.key);
            });
            menu.appendChild(card);
        }
        return menu;
    }

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
            if (e.key === "Escape") {
                close();
            }
        }
        function close() {
            if (!isOpen) {
                return;
            }
            isOpen = false;
            menu.hidden = true;
            btn.setAttribute("aria-expanded", "false");
            document.removeEventListener("mousedown", onDoc, true);
            document.removeEventListener("keydown", onKey, true);
        }
        function open() {
            const r = btn.getBoundingClientRect();
            menu.style.top = r.bottom + 6 + "px";
            menu.style.right = Math.max(8, window.innerWidth - r.right) + "px";
            menu.hidden = false;
            isOpen = true;
            btn.setAttribute("aria-expanded", "true");
            document.addEventListener("mousedown", onDoc, true);
            document.addEventListener("keydown", onKey, true);
        }
        btn.addEventListener("click", function (e) {
            e.preventDefault();
            if (isOpen) {
                close();
            } else {
                open();
            }
        });
    }

    function showChromiumDownload(received, total) {
        let box = document.getElementById("chromium-download");
        if (!box) {
            box = document.createElement("div");
            box.id = "chromium-download";
            box.className = "chromium-dl";
            box.innerHTML =
                '<div class="chromium-dl__panel">' +
                '<h2 class="chromium-dl__title">Downloading Chromium…</h2>' +
                '<p class="chromium-dl__sub">Needed once to export PDF.</p>' +
                '<div class="chromium-dl__track"><div class="chromium-dl__bar"></div></div>' +
                "</div>";
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

    function showQuitDialog() {
        if (document.getElementById("quit-dialog")) {
            return;
        }
        const box = document.createElement("div");
        box.id = "quit-dialog";
        box.className = "quit-dlg";
        box.innerHTML =
            '<div class="quit-dlg__panel" role="dialog" aria-modal="true">' +
            '<h2 class="quit-dlg__title">Unsaved changes</h2>' +
            '<p class="quit-dlg__sub">Save your work before exiting?</p>' +
            '<div class="quit-dlg__row">' +
            '<button type="button" class="quit-dlg__btn quit-dlg__btn--cancel">Cancel</button>' +
            '<button type="button" class="quit-dlg__btn quit-dlg__btn--discard">Exit without saving</button>' +
            '<button type="button" class="quit-dlg__btn quit-dlg__btn--save">Save and exit</button>' +
            "</div></div>";
        box.querySelector(".quit-dlg__btn--cancel").addEventListener(
            "click",
            function () {
                box.remove();
            },
        );
        box.querySelector(".quit-dlg__btn--discard").addEventListener(
            "click",
            function () {
                window.__deck.send("Interaction", { kind: "QuitConfirmed", save: false });
            },
        );
        box.querySelector(".quit-dlg__btn--save").addEventListener("click", function () {
            window.__deck.send("Interaction", { kind: "QuitConfirmed", save: true });
        });
        document.body.appendChild(box);
    }

    let thumbnailDims = { width: 1920, height: 1080 };
    let thumbnailThemeCss = "";

    const thumbnailHtmlCache = Object.create(null);

    let activeSlideId = null;

    let thumbDragSourceId = null;

    let thumbDragGhost = null;
    let thumbDropLine = null;

    const THUMB_KINDS = {
        slide: {
            listKey: "slides",
            activeKey: "active_slide_id",
            idOf: function (e) {
                return e.slide_id;
            },
            labelOf: function (e) {
                return e.title || e.slide_id;
            },

            editInitial: function (e) {
                return e.title === e.slide_id ? "" : e.title || "";
            },
            clickKind: "SlideThumbnailClicked",
            clickField: "slide_id",
            renameKind: "SlideTitleEditRequested",
            renameField: "new_title",
            addKind: "AddSlideRequested",

            pickerKind: "SlideLayoutPickerRequested",
            emptyText: "No slides.",
            addTitle: "New slide",
        },
        layout: {
            listKey: "layouts",
            activeKey: "active_layout_id",
            idOf: function (e) {
                return e.layout_id;
            },
            labelOf: function (e) {
                return e.name || e.layout_id;
            },
            editInitial: function (e) {
                return e.name || "";
            },
            clickKind: "LayoutThumbnailClicked",
            clickField: "layout_id",
            renameKind: "LayoutNameEditRequested",
            renameField: "new_name",
            addKind: "AddLayoutRequested",
            emptyText: "No layouts.",
            addTitle: "New layout",
        },
    };

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
        const items =
            payload && Array.isArray(payload[spec.listKey]) ? payload[spec.listKey] : [];

        if (kind === "slide") {
            const badge = document.getElementById("thumbs-count");
            if (badge) {
                badge.textContent = String(items.length);
            }
        }

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

    function closeLayoutPicker() {
        const existing = document.getElementById("layout-picker");
        if (existing) {
            existing.remove();
        }
        document.removeEventListener("keydown", onLayoutPickerKey, true);
    }

    function onLayoutPickerKey(e) {
        if (e.key === "Escape") {
            e.preventDefault();
            e.stopPropagation();
            closeLayoutPicker();
        }
    }

    function pickLayoutTile(layoutId, label, html) {
        const btn = document.createElement("button");
        btn.type = "button";
        btn.className = "layout-picker__tile";
        const preview = document.createElement("div");
        preview.className = "thumb__preview layout-picker__preview";
        const mount = document.createElement("div");
        mount.className = "thumb__mount";
        const shadow = mount.attachShadow({ mode: "open" });
        shadow.innerHTML =
            "<style>" +
            thumbnailThemeCss +
            "</style>" +
            '<style class="globals-css">' +
            currentGlobalsCss +
            "</style>" +
            '<style class="anim-kf">' +
            builtinKeyframesCss +
            "</style>" +
            '<style class="asset-vars">' +
            buildAssetVarCss() +
            "</style>" +
            (html || "");
        preview.appendChild(mount);
        const cap = document.createElement("span");
        cap.className = "layout-picker__label";
        cap.textContent = label;
        btn.appendChild(preview);
        btn.appendChild(cap);
        window.requestAnimationFrame(function () {
            applyThumbnailScale(preview, mount);
        });
        btn.addEventListener("click", function () {
            window.__deck.send("Interaction", {
                kind: "AddSlideRequested",
                layout_id: layoutId,
            });
            closeLayoutPicker();
        });
        return btn;
    }

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
        const layouts = payload && Array.isArray(payload.layouts) ? payload.layouts : [];
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

        const shadow = mount.attachShadow({ mode: "open" });
        shadow.innerHTML =
            "<style>" +
            thumbnailThemeCss +
            "</style>" +
            '<style class="globals-css">' +
            currentGlobalsCss +
            "</style>" +
            '<style class="anim-kf">' +
            builtinKeyframesCss +
            "</style>" +
            '<style class="asset-vars">' +
            buildAssetVarCss() +
            "</style>" +
            (entry.html || "");
        preview.appendChild(mount);

        const caption = document.createElement("div");
        caption.className = "thumb__caption";
        const num = document.createElement("span");
        num.className = "thumb__num";
        num.textContent = String(index + 1);
        const label = document.createElement("span");
        label.className = "thumb__label";
        label.textContent = spec.labelOf(entry);
        label.addEventListener("dblclick", function (e) {

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
            wireThumbReorder(btn, preview, itemId);
        }

        window.requestAnimationFrame(function () {
            applyThumbnailScale(preview, mount);
        });

        btn.addEventListener("click", function () {

            slideSelected = true;
            updateSlideFocusState();
            const msg = { kind: spec.clickKind };
            msg[spec.clickField] = itemId;
            window.__deck.send("Interaction", msg);
        });
        return btn;
    }

    function wireThumbReorder(btn, preview, itemId) {
        preview.draggable = true;
        preview.addEventListener("dragstart", function (e) {
            thumbDragSourceId = itemId;
            e.dataTransfer.effectAllowed = "move";
            e.dataTransfer.setData("text/plain", itemId);
            thumbDragGhost = makeThumbDragImage(itemId);
            e.dataTransfer.setDragImage(thumbDragGhost, 80, 45);
            btn.classList.add("is-dragging");
        });
        preview.addEventListener("dragend", function () {
            thumbDragSourceId = null;
            btn.classList.remove("is-dragging");
            hideThumbDropLine();
            if (thumbDragGhost) {
                thumbDragGhost.remove();
                thumbDragGhost = null;
            }
        });
        ensureThumbRowDnd(document.getElementById("thumbnail-row"));
    }

    function makeThumbDragImage(itemId) {
        const w = 160;
        const h = 90;
        const sw = thumbnailDims.width || 1920;
        const sh = thumbnailDims.height || 1080;
        const scale = Math.min(w / sw, h / sh);
        const ghost = document.createElement("div");
        ghost.className = "thumb-drag-ghost";
        const mount = document.createElement("div");
        mount.style.width = sw + "px";
        mount.style.height = sh + "px";
        mount.style.transformOrigin = "top left";
        mount.style.transform = "scale(" + scale + ")";
        const shadow = mount.attachShadow({ mode: "open" });
        shadow.innerHTML =
            "<style>" +
            thumbnailThemeCss +
            "</style>" +
            '<style class="globals-css">' +
            currentGlobalsCss +
            "</style>" +
            '<style class="anim-kf">' +
            builtinKeyframesCss +
            "</style>" +
            '<style class="asset-vars">' +
            buildAssetVarCss() +
            "</style>" +
            (thumbnailHtmlCache[itemId] || "");
        ghost.appendChild(mount);
        document.body.appendChild(ghost);
        return ghost;
    }

    function ensureThumbRowDnd(row) {
        if (!row) {
            return;
        }
        if (!thumbDropLine || thumbDropLine.parentNode !== row) {
            thumbDropLine = document.createElement("div");
            thumbDropLine.className = "thumb-drop-line";
            row.appendChild(thumbDropLine);
        }
        if (row.dataset.dndBound === "1") {
            return;
        }
        row.dataset.dndBound = "1";
        row.addEventListener("dragover", function (e) {
            if (!thumbDragSourceId) {
                return;
            }
            e.preventDefault();
            e.dataTransfer.dropEffect = "move";
            positionThumbDropLine(e, row);
        });
        row.addEventListener("dragleave", function (e) {
            if (e.target === row && !row.contains(e.relatedTarget)) {
                hideThumbDropLine();
            }
        });
        row.addEventListener("drop", function (e) {
            if (!thumbDragSourceId) {
                return;
            }
            e.preventDefault();
            window.__deck.send("Interaction", {
                kind: "SlideThumbnailReordered",
                slide_id: thumbDragSourceId,
                new_index: thumbDropIndex(e, row, thumbDragSourceId),
            });
            hideThumbDropLine();
        });
    }

    function hideThumbDropLine() {
        if (thumbDropLine) {
            thumbDropLine.style.display = "none";
        }
    }

    function positionThumbDropLine(event, row) {
        const thumbs = row.querySelectorAll(".thumb:not(.thumb--add)");
        let target = null;
        for (let i = 0; i < thumbs.length; i++) {
            const r = thumbs[i].getBoundingClientRect();
            if (event.clientX < r.left + r.width / 2) {
                target = thumbs[i];
                break;
            }
        }

        let center = 0;
        if (target) {
            center = target.offsetLeft - 6;
        } else if (thumbs.length > 0) {
            const last = thumbs[thumbs.length - 1];
            center = last.offsetLeft + last.offsetWidth + 6;
        }
        thumbDropLine.style.left = center + "px";
        thumbDropLine.style.display = "block";
    }

    function thumbDropIndex(event, rowEl, sourceId) {
        const thumbs = rowEl.querySelectorAll(".thumb:not(.thumb--add)");
        let idx = 0;
        for (let i = 0; i < thumbs.length; i++) {
            const t = thumbs[i];
            if (t.dataset.slideId === sourceId) {
                continue;
            }
            const r = t.getBoundingClientRect();
            if (event.clientX > r.left + r.width / 2) {
                idx += 1;
            }
        }
        return idx;
    }

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
                    "<style>" +
                    thumbnailThemeCss +
                    "</style>" +
                    '<style class="globals-css">' +
                    currentGlobalsCss +
                    "</style>" +
                    '<style class="asset-vars">' +
                    buildAssetVarCss() +
                    "</style>" +
                    (html || "");
            }
            const preview = mount.parentElement;
            window.requestAnimationFrame(function () {
                applyThumbnailScale(preview, mount);
            });
        }
    }

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
            '.thumb[data-slide-id="' + cssEscape(activeSlideId) + '"]',
        );
        if (!active) {
            return;
        }
        const rRect = row.getBoundingClientRect();
        const aRect = active.getBoundingClientRect();
        if (aRect.left < rRect.left || aRect.right > rRect.right) {
            active.scrollIntoView({
                behavior: "smooth",
                inline: "center",
                block: "nearest",
            });
        }
    }

    function cssEscape(value) {
        if (window.CSS && typeof window.CSS.escape === "function") {
            return window.CSS.escape(value);
        }
        return String(value).replace(/(["\\])/g, "\\$1");
    }

    const IMPORT_MAX_FILES = 32;

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

        const container = document.getElementById("viewport-container");
        if (!container) {
            return;
        }
        if (e.relatedTarget && container.contains(e.relatedTarget)) {
            return;
        }
        container.classList.remove("viewport--drop-active");
    }

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

    document.addEventListener("DOMContentLoaded", function () {

        const viewport = document.getElementById("viewport-container");
        if (viewport) {
            viewport.addEventListener("mousedown", onMouseDown);
            viewport.addEventListener("dblclick", onViewportDblClick);
            viewport.addEventListener("dragover", onViewportDragOver);
            viewport.addEventListener("dragleave", onViewportDragLeave);
            viewport.addEventListener("drop", onViewportDrop);
        }

        const objectsPanel = document.getElementById("object-panel");
        if (objectsPanel) {
            objectsPanel.addEventListener(
                "mousedown",
                function () {
                    setFocusRegion("objects");
                },
                true,
            );
        }
        if (viewport) {
            viewport.addEventListener(
                "mousedown",
                function () {
                    setFocusRegion("preview");
                },
                true,
            );
        }
        const thumbRow = document.getElementById("thumbnail-row");
        if (thumbRow) {
            thumbRow.addEventListener(
                "mousedown",
                function (e) {
                    setFocusRegion("navigator");

                    const onThumb =
                        e.target && e.target.closest && e.target.closest(".thumb");
                    if (!onThumb) {
                        slideSelected = false;
                        updateSlideFocusState();
                        if (currentSelectionIds.length > 0) {
                            window.__deck.send("Interaction", {
                                kind: "SetSelectionFromPanel",
                                element_ids: [],
                            });
                        }
                    }
                },
                true,
            );
        }

        if (viewport) {
            viewport.classList.add("is-focused");
        }

        const zoomOutBtn = document.getElementById("zoom-out");
        if (zoomOutBtn) {
            zoomOutBtn.addEventListener("click", function () {
                zoomStep(-ZOOM_STEP);
            });
        }
        const zoomInBtn = document.getElementById("zoom-in");
        if (zoomInBtn) {
            zoomInBtn.addEventListener("click", function () {
                zoomStep(ZOOM_STEP);
            });
        }
        const zoomFitBtn = document.getElementById("zoom-fit");
        if (zoomFitBtn) {
            zoomFitBtn.addEventListener("click", setZoomFit);
        }
        const toolSelectBtn = document.getElementById("tool-select");
        if (toolSelectBtn) {
            toolSelectBtn.addEventListener("click", function () {
                setTool("select");
            });
        }
        const toolHandBtn = document.getElementById("tool-hand");
        if (toolHandBtn) {
            toolHandBtn.addEventListener("click", function () {
                setTool("hand");
            });
        }
        applyZoom();
        window.addEventListener("mousemove", onMouseMove);
        window.addEventListener("mouseup", onMouseUp);
        window.addEventListener("resize", function () {

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
        init_agent_panel(document.body);
        wireAnimationsSection();
        wirePaneResizers();
        wireWindowControls();
        renderObjectPanel(null);

        window.requestAnimationFrame(function () {
            captureCanvasMin();
            positionDividers();
            refitThumbnails();
            renderCanvasScrim();
        });
        window.__deck.send("Ready", null);
    });

    const SLIDE_TRANSITIONS = [
        "None",
        "Fade",
        "Push",
        "Dissolve",
        "Wipe",
        "Flip",
        "Cube",
    ];

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
        entrance: "→",
        emphasis: "★",
        exit: "←",
        property: "{ }",
    };

    function animSend(kind, body) {
        body.kind = kind;
        window.__deck.send("Interaction", body);
    }
    function animAdd(catalogId, direction, elementId) {
        const el =
            elementId ||
            (currentSelectionIds.length === 1 ? currentSelectionIds[0] : null);
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

    function animReplace(animId, catalogId, direction, elementId) {
        animRemove(animId);
        animAdd(catalogId, direction, elementId);
    }

    function animDirectionOf(entry) {
        const kf = entry && entry.keyframe;
        const m = kf && /-(top|bottom|left|right)$/.exec(kf);
        return m ? m[1] : null;
    }

    function catalogForEntry(entry) {
        if (entry.category === "property") {
            return (
                animationCatalog.find(function (i) {
                    return i.kind === "property";
                }) || null
            );
        }
        const exact = animationCatalog.find(function (i) {
            return i.keyframe === entry.keyframe;
        });
        if (exact) {
            return exact;
        }
        const dir = animDirectionOf(entry);
        if (!dir) {
            return null;
        }
        const base = String(entry.keyframe).replace(/-(top|bottom|left|right)$/, "");
        return (
            animationCatalog.find(function (i) {
                return (
                    i.directional &&
                    String(i.keyframe).replace(/-(top|bottom|left|right)$/, "") === base
                );
            }) || null
        );
    }

    function animEffectLabel(entry) {
        const item = catalogForEntry(entry);
        return item ? item.label : entry.effect_id || "Effect";
    }
    function animTriggerLabel(entry) {
        const t = ANIM_TRIGGERS.find(function (x) {
            return x.value === entry.trigger;
        });
        return t ? t.label : entry.trigger;
    }
    function animEffectSummary(entry) {
        if (entry.category === "property") {
            const ts = entry.targets || [];
            if (ts.length === 0) {
                return "Property change";
            }
            const first = ts[0].property + " → " + ts[0].value;
            return ts.length > 1 ? first + " +" + (ts.length - 1) : first;
        }
        const dir = animDirectionOf(entry);
        return animEffectLabel(entry) + (dir ? " (" + dir + ")" : "");
    }

    function morphStateFromAttrs(elId) {
        const el = currentShadow
            ? currentShadow.querySelector(
                  '[data-element-id="' + String(elId).replace(/"/g, '\\"') + '"]',
              )
            : null;
        if (!el) {
            return { enabled: false, duration_ms: 300, easing: "ease-in-out" };
        }
        const enabled = el.hasAttribute("data-morph-next");
        const duration_ms = parseInt(el.getAttribute("data-morph-dur") || "300", 10);
        const easing = el.getAttribute("data-morph-ease") || "ease-in-out";
        return { enabled, duration_ms, easing };
    }

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
        const easings = [
            "linear",
            "ease-in",
            "ease-out",
            "ease-in-out",
            "cubic-bezier(0.34, 1.56, 0.64, 1)",
        ];
        const easingSelect = makeDropdown({
            label: "Easing",
            className: "morph-easing",
            options: easings.map(function (e) {
                return { value: e, label: e };
            }),
            value: state.easing,
        });
        easingSelect.dataset.elementId = elId;
        row2.appendChild(easingLabel);
        row2.appendChild(easingSelect);
        wrapper.appendChild(row2);

        return wrapper;
    }

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
        const mine = el
            ? slideAnimations.filter(function (a) {
                  return a.element_id === el;
              })
            : [];
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

    let sacDragId = null;

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

        head.addEventListener("click", function (e) {
            if (e.target.closest && e.target.closest("[data-sac-rm]")) {
                return;
            }
            animExpanded[entry.animation_id] = !animExpanded[entry.animation_id];
            renderSlideAnimations();
        });

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
        head.dataset.sacDrop = rel > 0.65 ? "join" : "after";
    }

    function clearSacDropHint() {
        const hinted = document.querySelectorAll("#sac-groups [data-sac-drop]");
        for (let i = 0; i < hinted.length; i++) {
            delete hinted[i].dataset.sacDrop;
        }
    }

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

        const ids = slideAnimations
            .map(function (a) {
                return a.animation_id;
            })
            .filter(function (id) {
                return id !== sacDragId;
            });
        const at = ids.indexOf(targetId);
        const newIndex = at < 0 ? ids.length : at + 1;
        const trigger = mode === "join" ? "with_previous" : "after_previous";
        window.__deck.send("Interaction", {
            kind: "MoveAnimation",
            animation_id: sacDragId,
            new_index: newIndex,
            trigger: trigger,
        });
        sacDragId = null;
    }

    const FLEX_DIRS = [
        { v: "row", t: "Row" },
        { v: "column", t: "Column" },
    ];
    const FLEX_DISTS = [
        { v: "none", t: "Manual" },
        { v: "start", t: "Start" },
        { v: "center", t: "Center" },
        { v: "end", t: "End" },
        { v: "space-between", t: "Between" },
        { v: "space-around", t: "Around" },
        { v: "space-evenly", t: "Evenly" },
    ];
    const FLEX_ALIGNS = [
        { v: "none", t: "Manual" },
        { v: "start", t: "Start" },
        { v: "center", t: "Center" },
        { v: "end", t: "End" },
    ];

    function groupFlexState() {
        if (currentSelectionIds.length !== 1) {
            return null;
        }
        const el = findElement(currentSelectionIds[0]);
        if (!el || el.dataset.elementType !== "group") {
            return null;
        }
        return {
            direction: el.dataset.flexDir || "row",
            distribution: el.dataset.flexDist || "none",
            alignment: el.dataset.flexAlign || "none",
        };
    }

    function flexSelect(label, opts, current, field) {
        const dd = makeDropdown({
            label: label,
            options: opts.map(function (o) {
                return { value: o.v, label: o.t };
            }),
            value: current,
            onChange: function (v) {
                if (currentSelectionIds.length !== 1) {
                    return;
                }
                const body = {
                    kind: "SetGroupLayout",
                    element_id: currentSelectionIds[0],
                    direction: null,
                    distribution: null,
                    alignment: null,
                };
                body[field] = v;
                window.__deck.send("Interaction", body);
            },
        });
        return animField(label, dd);
    }

    const ALIGN_OPS = [
        { op: "left", tip: "Align left", min: 2, d: "M4 4v16M4 8h12M4 15h8" },
        { op: "h-center", tip: "Align centers", min: 2, d: "M12 4v16M6 8h12M8 15h8" },
        { op: "right", tip: "Align right", min: 2, d: "M20 4v16M8 8h12M12 15h8" },
        { op: "top", tip: "Align top", min: 2, d: "M4 4h16M8 4v12M15 4v8" },
        { op: "v-center", tip: "Align middles", min: 2, d: "M4 12h16M8 6v12M15 8v8" },
        { op: "bottom", tip: "Align bottom", min: 2, d: "M4 20h16M8 8v12M15 12v8" },
        {
            op: "distribute-h",
            tip: "Distribute horizontally",
            min: 3,
            d: "M4 4v16M20 4v16M10 8h4v8h-4z",
        },
        {
            op: "distribute-v",
            tip: "Distribute vertically",
            min: 3,
            d: "M4 4h16M4 20h16M8 10h8v4H8z",
        },
    ];

    function buildAlignControls(host) {
        console.assert(host, "align controls host missing");
        for (let i = 0; i < ALIGN_OPS.length; i++) {
            const spec = ALIGN_OPS[i];
            const b = document.createElement("button");
            b.type = "button";
            b.className = "inspector__segment-btn tt";
            b.dataset.alignOp = spec.op;
            b.dataset.alignMin = String(spec.min);
            b.setAttribute("data-tip", spec.tip);
            b.setAttribute("data-key", "");
            b.setAttribute("aria-label", spec.tip);
            b.innerHTML = segIcon(spec.d);
            host.appendChild(b);
        }
        host.addEventListener("click", onAlignClick);
    }

    function onAlignClick(e) {
        const btn = e.target.closest("[data-align-op]");
        if (!btn || btn.disabled) {
            return;
        }
        const min = Number(btn.dataset.alignMin);
        if (currentSelectionIds.length < min) {
            return;
        }
        window.__deck.send("Interaction", {
            kind: "AlignSelectionRequested",
            element_ids: currentSelectionIds.slice(),
            op: btn.dataset.alignOp,
        });
    }

    function setAlignBoxVisible(show) {
        const box = document.getElementById("align-box");
        const host = document.getElementById("align-controls");
        if (!box || !host) {
            return;
        }
        if (host.childElementCount === 0) {
            buildAlignControls(host);
        }
        box.style.display = show ? "" : "none";
        const count = currentSelectionIds.length;
        for (let i = 0; i < host.children.length; i++) {
            const btn = host.children[i];
            btn.disabled = count < Number(btn.dataset.alignMin);
        }
    }

    function refreshGroupFlexSection() {
        const host = document.getElementById("flex-controls");
        if (!host) {
            return;
        }
        host.replaceChildren();
        const st = groupFlexState();
        if (!st) {
            return;
        }
        host.appendChild(flexSelect("Direction", FLEX_DIRS, st.direction, "direction"));
        host.appendChild(
            flexSelect("Distribute", FLEX_DISTS, st.distribution, "distribution"),
        );
        host.appendChild(flexSelect("Align", FLEX_ALIGNS, st.alignment, "alignment"));
    }

    function buildAnimBar(entry) {
        const bar = document.createElement("div");
        bar.className = "anim-bar";
        bar.appendChild(buildAnimHead(entry));
        if (animExpanded[entry.animation_id]) {
            bar.appendChild(buildAnimBody(entry));
        }
        return bar;
    }

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
        rm.addEventListener("click", function () {
            animRemove(entry.animation_id);
        });
        head.append(icon, label, trig, chev, rm);
        return head;
    }

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

    function animField(labelText, control) {
        const row = document.createElement("div");
        row.className = "anim-bar__field";
        const span = document.createElement("span");
        span.textContent = labelText;
        row.append(span, control);
        return row;
    }

    function buildAnimEffectRow(entry) {
        const current = catalogForEntry(entry);
        const options = animationCatalog
            .filter(function (i) {
                return i.category === entry.category && i.kind === "named";
            })
            .map(function (item) {
                return { value: item.id, label: item.label };
            });
        const dd = makeDropdown({
            label: "Effect",
            options: options,
            value: current ? current.id : "",
            onChange: function (v) {
                const item = animationCatalog.find(function (i) {
                    return i.id === v;
                });
                const dir =
                    item && item.directional ? animDirectionOf(entry) || "top" : null;
                animReplace(entry.animation_id, v, dir, entry.element_id);
            },
        });
        return animField("Effect", dd);
    }

    function buildAnimDirectionRow(entry, dir) {
        const item = catalogForEntry(entry);
        const dd = makeDropdown({
            label: "Direction",
            options: ANIM_DIRECTIONS.map(function (d) {
                return { value: d.value, label: d.label };
            }),
            value: dir,
            onChange: function (v) {
                if (item) {
                    animReplace(entry.animation_id, item.id, v, entry.element_id);
                }
            },
        });
        return animField("Direction", dd);
    }

    function buildAnimTriggerRow(entry) {
        const dd = makeDropdown({
            label: "Trigger",
            options: ANIM_TRIGGERS.map(function (t) {
                return { value: t.value, label: t.label };
            }),
            value: entry.trigger,
            onChange: function (v) {
                animUpdate(entry.animation_id, { trigger: v });
            },
        });
        return animField("Trigger", dd);
    }

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

    function buildAnimEasingRow(entry) {
        const dd = makeDropdown({
            label: "Easing",
            options: ANIM_EASINGS.map(function (e) {
                return { value: e.token, label: e.label };
            }),
            value: entry.easing,
            onChange: function (v) {
                animUpdate(entry.animation_id, { easing: v });
            },
        });
        return animField("Easing", dd);
    }

    function buildAnimIterationsRow(entry) {
        const infinite = entry.iterations === "Infinite";
        const wrap = document.createElement("div");
        wrap.className = "anim-bar__pair";
        const num = document.createElement("input");
        num.type = "number";
        num.min = "1";
        num.value = infinite
            ? "1"
            : String((entry.iterations && entry.iterations.Count) || 1);
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

    function buildAnimPropRows(entry) {
        const box = document.createElement("div");
        box.style.display = "flex";
        box.style.flexDirection = "column";
        box.style.gap = "6px";
        const targets =
            entry.targets && entry.targets.length
                ? entry.targets.slice()
                : [{ property: "opacity", value: "1" }];
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
                const duration_ms = parseInt(
                    durationInput ? durationInput.value : "300",
                    10,
                );
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
                const duration_ms = parseInt(
                    durationInput ? durationInput.value : "300",
                    10,
                );
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
            menu.addEventListener("click", function (e) {
                e.stopPropagation();
            });
            document.addEventListener("click", function () {
                menu.hidden = true;
            });
        }
        if (play) {
            play.addEventListener("click", playAnimPreview);
        }
    }

    function buildAnimAddMenu(menu) {
        menu.replaceChildren();
        const cats = ["entrance", "emphasis", "exit", "property"];
        for (let c = 0; c < cats.length; c++) {
            const items = animationCatalog.filter(function (i) {
                return i.category === cats[c];
            });
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

    function animFindEl(id) {
        if (!currentShadow || !id) {
            return null;
        }
        const safe = String(id).replace(/"/g, '\\"');
        return currentShadow.querySelector('[data-element-id="' + safe + '"]');
    }

    function animIterCount(iters) {
        if (iters === "Infinite") {
            return 1;
        }
        if (iters && typeof iters.Count === "number") {
            return Math.max(1, iters.Count);
        }
        return 1;
    }

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
                    el.style.setProperty(
                        entry.targets[i].property,
                        entry.targets[i].value,
                    );
                }
            });
            return;
        }
        const iters =
            entry.iterations === "Infinite"
                ? "infinite"
                : String(animIterCount(entry.iterations));
        el.style.opacity = "1";
        el.style.animation =
            entry.keyframe +
            " " +
            entry.duration_ms +
            "ms " +
            entry.easing +
            " " +
            effDelay +
            "ms " +
            iters +
            " both";
        const endsHidden = entry.category === "exit";
        const onEnd = function () {
            el.style.animation = "none";
            el.style.opacity = endsHidden ? "0" : "1";
            el.removeEventListener("animationend", onEnd);
        };
        el.addEventListener("animationend", onEnd);
    }

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

    const toasts = [];
    const TOAST_MAX = 3;
    const TOAST_TTL_MS = 3000;

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
            el: el,
            timer: null,
            detail: detail || "",
            expanded: false,
            offClick: null,
            removed: false,
        };
        entry.timer = window.setTimeout(function () {
            dismissToast(entry);
        }, TOAST_TTL_MS);
        el.addEventListener("click", function (e) {
            e.stopPropagation();
            onToastClick(entry);
        });
        toasts.unshift(entry);
        while (toasts.length > TOAST_MAX) {
            dismissToast(toasts[toasts.length - 1]);
        }
    }

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

        window.setTimeout(function () {
            document.addEventListener("click", entry.offClick, true);
        }, 0);
    }

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

    function wireLayoutEditorControls() {
        const toggle = document.getElementById("mode-toggle");
        if (toggle) {
            toggle.addEventListener("click", function () {
                const next = currentMode === "layout" ? "slide" : "layout";
                window.__deck.send("Interaction", {
                    kind: "SetEditorMode",
                    mode: next,
                });
            });
        }
        const presentBtn = document.getElementById("present-btn");
        if (presentBtn) {
            presentBtn.addEventListener("click", function () {

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

    let agent_panel_open = false;
    let agent_current_stream_id = null;
    let agent_running = false;

    let agent_last_selection = "";

    let agent_pending_select = null;

    const AGENT_ADD_SENTINEL = "__add_agent__";

    let activity_heartbeat_timer = null;

    let current_activity_phase = "idle";

    let thinking_collapsed = true;

    function init_agent_panel(root) {
        if (!root) {
            return;
        }
        const toggle = root.querySelector("#agent-toggle");
        const input = root.querySelector("#agent-input");
        const send_btn = root.querySelector("#agent-send-btn");
        if (toggle) {
            toggle.addEventListener("click", function () {
                agent_panel_open = !agent_panel_open;
                toggle.setAttribute("aria-pressed", agent_panel_open ? "true" : "false");
                const col = document.querySelector(".left-col");
                if (col) {
                    col.dataset.agentVisible = agent_panel_open ? "true" : "false";
                }
                if (agent_panel_open) {
                    toggle_left_layout("agent");
                }
                window.__deck.send("AgentPanelToggled", { open: agent_panel_open });
            });
        }
        if (send_btn) {
            send_btn.addEventListener("click", function () {
                if (agent_running) {
                    window.__deck.send("AgentCancelRequested", null);
                } else {
                    send_agent_prompt(input ? input.value.trim() : "");
                }
            });
        }
        if (input) {
            input.addEventListener("keydown", function (e) {
                if (e.key === "Enter" && !e.shiftKey) {
                    e.preventDefault();
                    send_agent_prompt(input.value.trim());
                }
            });
        }
        const select = root.querySelector("#agent-select");
        if (select) {
            select.addEventListener("change", function () {
                if (select.value === AGENT_ADD_SENTINEL) {
                    select.value = agent_last_selection;
                    open_add_agent_modal();
                } else {
                    agent_last_selection = select.value;
                }
            });
        }
        const thinking_toggle = root.querySelector(".agent-thinking__toggle");
        if (thinking_toggle) {
            thinking_toggle.addEventListener("click", function () {
                const thinking_el = root.querySelector("#agent-thinking");
                if (thinking_el) {
                    thinking_collapsed = !thinking_collapsed;
                    thinking_el.dataset.collapsed = thinking_collapsed ? "true" : "false";
                }
            });
        }
        root.querySelectorAll("[data-pane-collapse]").forEach(function (btn) {
            btn.addEventListener("click", function () {
                const which = btn.dataset.paneCollapse;
                const col = document.querySelector(".left-col");
                if (!col) {
                    return;
                }
                const other = which === "agent" ? "objects" : "agent";
                toggle_left_layout(col.dataset.agent === which ? other : which);
            });
        });
    }

    function send_agent_prompt(text) {
        if (!text || text.length === 0) {
            return;
        }
        const select = document.querySelector("#agent-select");
        const agent = select ? select.value : "";
        window.__deck.send("AgentPromptSubmitted", { text: text, agent: agent });
        const input = document.querySelector("#agent-input");
        if (input) {
            input.value = "";
        }
        const log = document.querySelector("#agent-log");
        if (log) {
            const row = document.createElement("div");
            row.className = "agent__message agent__message--user";
            row.textContent = text;
            log.appendChild(row);
            log.scrollTop = log.scrollHeight;
        }
    }

    function populate_agent_select(payload) {
        if (!payload || !Array.isArray(payload.agents)) {
            return;
        }
        const select = document.querySelector("#agent-select");
        if (!select) {
            return;
        }
        const previous = select.value;
        select.textContent = "";
        payload.agents.forEach(function (name) {
            const opt = document.createElement("option");
            opt.value = name;
            opt.textContent = name;
            select.appendChild(opt);
        });
        const add_opt = document.createElement("option");
        add_opt.value = AGENT_ADD_SENTINEL;
        add_opt.textContent = "+ Add agent…";
        select.appendChild(add_opt);
        let chosen = "";
        if (agent_pending_select && payload.agents.indexOf(agent_pending_select) >= 0) {
            chosen = agent_pending_select;
        } else if (payload.agents.indexOf(previous) >= 0) {
            chosen = previous;
        } else if (payload.agents.length > 0) {
            chosen = payload.agents[0];
        }
        agent_pending_select = null;
        select.value = chosen;
        agent_last_selection = chosen;
    }

    function open_add_agent_modal() {
        if (document.querySelector("#agent-modal")) {
            return;
        }
        const backdrop = document.createElement("div");
        backdrop.id = "agent-modal";
        backdrop.className = "agent-modal";
        const panel = document.createElement("div");
        panel.className = "agent-modal__panel";
        panel.innerHTML =
            '<h3 class="agent-modal__title">Add agent</h3>' +
            '<label class="agent-modal__label">Name' +
            '<input class="agent-modal__input" data-field="name" placeholder="Claude"></label>' +
            '<label class="agent-modal__label">Command' +
            '<input class="agent-modal__input" data-field="command" placeholder="claude-code-acp"></label>' +
            '<label class="agent-modal__label">Args (space-separated)' +
            '<input class="agent-modal__input" data-field="args" placeholder="--flag value"></label>' +
            '<div class="agent-modal__buttons">' +
            '<button type="button" class="agent__btn agent__btn--deny" data-action="cancel">Cancel</button>' +
            '<button type="button" class="agent__btn agent__btn--approve" data-action="save">Save</button>' +
            "</div>";
        backdrop.appendChild(panel);
        document.body.appendChild(backdrop);
        const close = function () {
            backdrop.remove();
        };
        backdrop.addEventListener("mousedown", function (e) {
            if (e.target === backdrop) {
                close();
            }
        });
        panel.querySelector('[data-action="cancel"]').addEventListener("click", close);
        panel
            .querySelector('[data-action="save"]')
            .addEventListener("click", function () {
                const name = panel.querySelector('[data-field="name"]').value.trim();
                const command = panel
                    .querySelector('[data-field="command"]')
                    .value.trim();
                const args_raw = panel.querySelector('[data-field="args"]').value.trim();
                if (name.length === 0 || command.length === 0) {
                    return;
                }
                const args = args_raw.length > 0 ? args_raw.split(/\s+/) : [];
                agent_pending_select = name;
                window.__deck.send("AgentAddRequested", {
                    name: name,
                    command: command,
                    args: args,
                });
                close();
            });
        const name_input = panel.querySelector('[data-field="name"]');
        if (name_input) {
            name_input.focus();
        }
    }

    function append_stream_chunk(chunk) {
        if (!chunk || typeof chunk !== "object") {
            return;
        }
        const log = document.querySelector("#agent-log");
        if (!log) {
            return;
        }
        let row = document.querySelector("#agent-log .agent__message--stream");
        if (!row || row.dataset.final !== "true") {
            if (!row) {
                row = document.createElement("div");
                row.className = "agent__message agent__message--stream";
                row.dataset.final = "false";
                log.appendChild(row);
            }
            const old_text = row.textContent || "";
            row.textContent = old_text + (chunk.text || "");
        }
        if (chunk.final_chunk) {
            row.dataset.final = "true";
            agent_current_stream_id = null;
        }
        log.scrollTop = log.scrollHeight;
        note_activity();
    }

    function show_permission_ask(ask) {
        if (!ask || typeof ask !== "object") {
            return;
        }
        const log = document.querySelector("#agent-log");
        const template = document.querySelector("#agent-permission");
        if (!log || !template) {
            return;
        }
        const row = template.content.cloneNode(true);
        const row_el = row.querySelector(".agent__permission-row");
        if (row_el) {
            row_el.dataset.requestId = ask.request_id;
            const text_el = row_el.querySelector(".agent__permission-text");
            if (text_el) {
                text_el.textContent = ask.summary || "";
            }
            const approve_btn = row_el.querySelector('[data-action="approve"]');
            const deny_btn = row_el.querySelector('[data-action="deny"]');
            if (approve_btn) {
                approve_btn.addEventListener("click", function () {
                    window.__deck.send("AgentPermissionReply", {
                        request_id: ask.request_id,
                        allow: true,
                    });
                    row_el.remove();
                });
            }
            if (deny_btn) {
                deny_btn.addEventListener("click", function () {
                    window.__deck.send("AgentPermissionReply", {
                        request_id: ask.request_id,
                        allow: false,
                    });
                    row_el.remove();
                });
            }
        }
        log.appendChild(row);
        log.scrollTop = log.scrollHeight;
    }

    function set_panel_state(state) {
        if (!state || typeof state !== "object") {
            return;
        }
        agent_running = state.running || false;
        const input = document.querySelector("#agent-input");
        const send_btn = document.querySelector("#agent-send-btn");
        const select = document.querySelector("#agent-select");
        if (input) {
            input.disabled = agent_running;
        }
        if (select) {
            select.disabled = agent_running;
        }
        if (send_btn) {
            send_btn.classList.toggle("agent__btn--is-stop", agent_running);
            send_btn.title = agent_running ? "Stop agent" : "Send prompt";
            send_btn.setAttribute("aria-label", agent_running ? "Stop" : "Send");
        }
        if (state.error && state.error.length > 0) {
            const log = document.querySelector("#agent-log");
            if (log) {
                const err_row = document.createElement("div");
                err_row.className = "agent__message agent__message--error";
                err_row.textContent = "Error: " + state.error;
                log.appendChild(err_row);
                log.scrollTop = log.scrollHeight;
            }
        }
    }

    function note_activity() {
        if (activity_heartbeat_timer) {
            clearTimeout(activity_heartbeat_timer);
        }
        if (current_activity_phase !== "idle") {
            activity_heartbeat_timer = setTimeout(function () {
                const status_label = document.querySelector(".agent-status__label");
                if (status_label) {
                    status_label.textContent = "Still working…";
                }
            }, 8000);
        }
    }

    function set_activity(payload) {
        if (!payload || typeof payload !== "object") {
            return;
        }
        const phase = payload.phase || "idle";
        const label = payload.label || "";
        const status_el = document.querySelector("#agent-status");
        const thinking_el = document.querySelector("#agent-thinking");
        const status_label = document.querySelector(".agent-status__label");
        current_activity_phase = phase;
        const phase_labels = {
            idle: "",
            starting: "Starting agent…",
            thinking: "Thinking…",
            streaming: "Streaming…",
            tool: "Using tool…",
            awaiting_approval: "Awaiting approval…",
            error: "Error",
        };
        if (phase === "idle" || phase === "") {
            if (status_el) {
                status_el.hidden = true;
            }
            if (thinking_el) {
                thinking_el.hidden = true;
                const thinking_body = thinking_el.querySelector(".agent-thinking__body");
                if (thinking_body) {
                    thinking_body.textContent = "";
                }
            }
            finalize_tool_rows("completed");
            const stream_row = document.querySelector(
                "#agent-log .agent__message--stream",
            );
            if (stream_row) {
                stream_row.classList.remove("agent__message--stream");
            }
            if (activity_heartbeat_timer) {
                clearTimeout(activity_heartbeat_timer);
                activity_heartbeat_timer = null;
            }
        } else {
            if (status_el) {
                status_el.hidden = false;
                if (status_label) {
                    status_label.textContent =
                        label || phase_labels[phase] || "Agent active…";
                }
            }
            if (phase === "streaming") {
                if (thinking_el) {
                    thinking_el.dataset.collapsed = "true";
                    thinking_collapsed = true;
                }
            }
            if (phase === "error") {
                finalize_tool_rows("failed");
            }
            note_activity();
        }
    }

    function finalize_tool_rows(status) {
        const rows = document.querySelectorAll("#agent-log .agent__tool-row");
        rows.forEach(function (row) {
            const current = row.dataset.status;
            if (current === "pending" || current === "in_progress") {
                row.dataset.status = status;
            }
        });
    }

    function append_thought(payload) {
        if (!payload || typeof payload !== "object") {
            return;
        }
        const text = payload.text || "";
        if (!text) {
            return;
        }
        const thinking_el = document.querySelector("#agent-thinking");
        if (!thinking_el) {
            return;
        }
        thinking_el.hidden = false;
        const body = thinking_el.querySelector(".agent-thinking__body");
        if (body) {
            body.textContent = (body.textContent || "") + text;
            body.scrollTop = body.scrollHeight;
        }
        note_activity();
    }

    function upsert_tool_row(payload) {
        if (!payload || typeof payload !== "object") {
            return;
        }
        const tool_id = payload.id || "";
        const title = payload.title || "";
        const status = payload.status || "pending";
        if (!tool_id) {
            return;
        }
        const log = document.querySelector("#agent-log");
        if (!log) {
            return;
        }
        let row = log.querySelector('[data-tool-id="' + tool_id + '"]');
        if (!row) {
            row = document.createElement("div");
            row.className = "agent__message agent__tool-row";
            row.dataset.toolId = tool_id;
            log.appendChild(row);
        }
        if (title) {
            row.textContent = title;
        }
        row.dataset.status = status;
        log.scrollTop = log.scrollHeight;
        note_activity();
    }

    function toggle_left_layout(expand) {
        if (expand !== "agent" && expand !== "objects") {
            return;
        }
        const left_col = document.querySelector(".left-col");
        const agent_panel = document.querySelector("#agent-panel");
        const objects_panel = document.querySelector("#object-panel");
        if (!left_col) {
            return;
        }
        left_col.dataset.agent = expand;
        if (agent_panel) {
            agent_panel.classList.toggle("panel--collapsed", expand === "objects");
        }
        if (objects_panel) {
            objects_panel.classList.toggle("panel--collapsed", expand === "agent");
        }
    }

    function matchGridToggleShortcut(e) {
        const meta = !!(e.metaKey || e.ctrlKey);
        return meta && !e.shiftKey && e.key === "'";
    }

    function updateSlideFocusState() {
        const row = document.getElementById("thumbnail-row");
        if (row) {
            row.dataset.slideFocus = slideSelected ? "true" : "false";
        }
    }

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

    function setGridEnabled(on) {
        gridEnabled = !!on;
        const btn = document.getElementById("grid-toggle");
        if (btn) {
            btn.setAttribute("aria-pressed", gridEnabled ? "true" : "false");
            btn.classList.toggle("is-active", gridEnabled);
        }
    }

    function matchClipboardShortcut(e) {
        const meta = !!(e.metaKey || e.ctrlKey);
        if (!meta || e.shiftKey) {
            return null;
        }
        const key = typeof e.key === "string" ? e.key.toLowerCase() : "";
        if (key === "c") {
            return "copy";
        }
        if (key === "x") {
            return "cut";
        }
        if (key === "v") {
            return "paste";
        }
        return null;
    }

    function matchUndoRedoShortcut(e) {
        const meta = !!(e.metaKey || e.ctrlKey);
        if (!meta) {
            return null;
        }
        const key = typeof e.key === "string" ? e.key.toLowerCase() : "";
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

    function matchFileShortcut(e) {
        const meta = !!(e.metaKey || e.ctrlKey);
        if (!meta) {
            return null;
        }
        const key = typeof e.key === "string" ? e.key.toLowerCase() : "";
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

    function matchPresentShortcut(e) {
        const meta = !!(e.metaKey || e.ctrlKey);
        return meta && !e.shiftKey && e.key === "Enter";
    }

    function matchAddSlideShortcut(e) {
        const meta = !!(e.metaKey || e.ctrlKey);
        if (!meta || !e.shiftKey) {
            return false;
        }
        const key = typeof e.key === "string" ? e.key.toLowerCase() : "";
        return key === "n";
    }

    function sendSyntheticKey(syntheticKey, e) {
        window.__deck.send("Interaction", {
            kind: "KeyPressed",
            key: syntheticKey,
            modifiers: readModifiers(e),
        });
    }

    function clickAddButton(selector) {
        const b = document.querySelector(selector);
        if (b) {
            b.click();
        }
    }

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

            const type = (el.type || "text").toLowerCase();
            const nonText = [
                "button",
                "submit",
                "reset",
                "checkbox",
                "radio",
                "range",
                "file",
                "image",
                "color",
            ];
            return nonText.indexOf(type) < 0;
        }
        if (el.isContentEditable) {
            return true;
        }
        return false;
    }

    const ALWAYS_PREVENT_DEFAULT_KEYS = new Set(["Backspace", "Delete", "Tab"]);

    document.addEventListener("keydown", function (e) {
        if (cropState) {
            if (e.key === "Enter") {
                e.preventDefault();
                commitCrop();
                return;
            }
            if (e.key === "Escape") {
                e.preventDefault();
                cancelCrop();
                return;
            }
            return;
        }
        if (e.key === "Escape" && focusChain.length > 0 && !textEditState) {
            focusChain = [];
            tableCellSel = null;
            updateSelectionOverlay();
            return;
        }

        if (
            selectedGuideId !== null &&
            !isEditableFocus() &&
            (e.key === "Backspace" || e.key === "Delete")
        ) {
            e.preventDefault();
            deleteGuide(selectedGuideId);
            return;
        }

        if (
            (e.metaKey || e.ctrlKey) &&
            e.shiftKey &&
            !e.altKey &&
            typeof e.key === "string" &&
            e.key.toLowerCase() === "d"
        ) {
            e.preventDefault();
            window.__deck.send("SetAppearance", {
                mode: window.__appearance.rotate(),
            });
            return;
        }

        if (
            (e.metaKey || e.ctrlKey) &&
            !e.shiftKey &&
            !e.altKey &&
            typeof e.key === "string" &&
            e.key.toLowerCase() === "r"
        ) {
            e.preventDefault();
            toggleRulers();
            return;
        }

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

        if (
            !isEditableFocus() &&
            !e.metaKey &&
            !e.ctrlKey &&
            !e.altKey &&
            typeof e.key === "string"
        ) {
            const k = e.key.toLowerCase();
            if (k === "v") {
                e.preventDefault();
                setTool("select");
                return;
            }
            if (k === "h") {
                e.preventDefault();
                setTool("hand");
                return;
            }
        }

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

            return;
        }

        const markShortcut = matchTextMarkShortcut(e);
        if (markShortcut && !textEditState && toggleWholeBoxMark(markShortcut)) {
            e.preventDefault();
            return;
        }

        if (
            (e.metaKey || e.ctrlKey) &&
            e.shiftKey &&
            typeof e.key === "string" &&
            e.key.toLowerCase() === "g"
        ) {
            e.preventDefault();
            if (currentSelectionIds.length >= 2) {
                window.__deck.send("Interaction", {
                    kind: "GroupSelectionRequested",
                    element_ids: currentSelectionIds.slice(),
                });
            }
            return;
        }

        const clip = matchClipboardShortcut(e);
        if (clip) {
            e.preventDefault();
            if (clip === "paste") {
                window.__deck.send("Interaction", { kind: "PasteRequested" });
            } else {
                const scope = focusRegion === "navigator" ? "Slide" : "Elements";
                const kind = clip === "copy" ? "CopyRequested" : "CutRequested";
                window.__deck.send("Interaction", { kind: kind, scope: scope });
            }
            return;
        }

        if (
            (e.key === "Delete" || e.key === "Backspace") &&
            focusRegion === "navigator" &&
            activeSlideId
        ) {
            e.preventDefault();
            window.__deck.send("Interaction", {
                kind: "RemoveSlideRequested",
                slide_id: activeSlideId,
            });
            return;
        }

        const ARROW_DELTA = {
            ArrowLeft: [-1, 0],
            ArrowRight: [1, 0],
            ArrowUp: [0, -1],
            ArrowDown: [0, 1],
        };
        if (ARROW_DELTA[e.key]) {
            if (currentSelectionIds.length > 0) {
                e.preventDefault();
                const d = ARROW_DELTA[e.key];
                window.__deck.send("Interaction", {
                    kind: "NudgeSelectionRequested",
                    dx: d[0],
                    dy: d[1],
                });
                return;
            }
            const horizontal = e.key === "ArrowLeft" || e.key === "ArrowRight";
            const navFocus = focusRegion === "preview" || focusRegion === "navigator";
            if (horizontal && navFocus) {
                e.preventDefault();
                window.__deck.send("Interaction", {
                    kind: "NavigateSlideRequested",
                    forward: e.key === "ArrowRight",
                });
                return;
            }
        }
        const isSingleChar = typeof e.key === "string" && e.key.length === 1;
        const recognizedControl =
            [
                "ArrowLeft",
                "ArrowRight",
                "ArrowUp",
                "ArrowDown",
                "Enter",
                "Escape",
                "Tab",
                "Backspace",
                "Delete",
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
