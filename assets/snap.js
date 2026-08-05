(function () {
    "use strict";

    var MAX_SNAP_ELEMENTS = 256;
    var EPS = 0.01;

    function assert_rect(r) {
        if (!r || typeof r !== "object") {
            throw new Error("snap: rect must be an object");
        }
        var ok = isFinite(r.x) && isFinite(r.y) && isFinite(r.w) && isFinite(r.h);
        if (!ok) {
            throw new Error("snap: rect fields must be finite");
        }
    }

    function build_targets(rects) {
        if (!Array.isArray(rects)) {
            throw new Error("snap: rects must be an array");
        }
        var xLines = [];
        var yLines = [];
        var n = Math.min(rects.length, MAX_SNAP_ELEMENTS);
        var i = 0;
        for (i = 0; i < n; i = i + 1) {
            var r = rects[i];
            assert_rect(r);
            xLines.push({ pos: r.x, source: "left" });
            xLines.push({ pos: r.x + r.w / 2, source: "centerX" });
            xLines.push({ pos: r.x + r.w, source: "right" });
            yLines.push({ pos: r.y, source: "top" });
            yLines.push({ pos: r.y + r.h / 2, source: "centerY" });
            yLines.push({ pos: r.y + r.h, source: "bottom" });
        }
        return { xLines: xLines, yLines: yLines, rects: rects.slice(0, n) };
    }

    function moving_lines(rect, axis) {
        if (axis === "x") {
            return [rect.x, rect.x + rect.w / 2, rect.x + rect.w];
        }
        return [rect.y, rect.y + rect.h / 2, rect.y + rect.h];
    }

    function best_offset(movPositions, candLines, threshold) {
        var best = null;
        var i = 0;
        var j = 0;
        for (i = 0; i < movPositions.length; i = i + 1) {
            for (j = 0; j < candLines.length; j = j + 1) {
                var delta = candLines[j].pos - movPositions[i];
                var adist = Math.abs(delta);
                if (adist <= threshold && (best === null || adist < best.dist)) {
                    best = { dist: adist, offset: delta };
                }
            }
        }
        return best;
    }

    function coincident_guides(movPositions, candLines, axis) {
        var seen = {};
        var guides = [];
        var i = 0;
        var j = 0;
        for (i = 0; i < movPositions.length; i = i + 1) {
            for (j = 0; j < candLines.length; j = j + 1) {
                if (Math.abs(candLines[j].pos - movPositions[i]) <= EPS) {
                    var key = String(candLines[j].pos);
                    if (!seen[key]) {
                        seen[key] = true;
                        var src = candLines[j].source;
                        guides.push({
                            axis: axis,
                            pos: candLines[j].pos,
                            kind: (src === "centerX" || src === "centerY")
                                ? "center" : "align",
                            span: axis === "x" ? 1080 : 1920,
                        });
                    }
                }
            }
        }
        return guides;
    }

    function snap_axis(rect, candLines, axis, threshold) {
        var mov = moving_lines(rect, axis);
        var best = best_offset(mov, candLines, threshold);
        if (best === null) {
            return { offset: 0, guides: [] };
        }
        var shifted = mov.map(function (p) { return p + best.offset; });
        return {
            offset: best.offset,
            guides: coincident_guides(shifted, candLines, axis),
        };
    }

    function overlaps_perp(a, b, axis) {
        if (axis === "x") {
            return a.y < b.y + b.h && b.y < a.y + a.h;
        }
        return a.x < b.x + b.w && b.x < a.x + a.w;
    }

    function lo_hi(r, axis) {
        if (axis === "x") { return [r.x, r.x + r.w]; }
        return [r.y, r.y + r.h];
    }

    function spacing_offset(rect, rects, axis, threshold) {
        var mLo = lo_hi(rect, axis)[0];
        var mHi = lo_hi(rect, axis)[1];
        var left = null;
        var right = null;
        var i = 0;
        for (i = 0; i < rects.length; i = i + 1) {
            var r = rects[i];
            if (!overlaps_perp(rect, r, axis)) { continue; }
            var rHi = lo_hi(r, axis)[1];
            var rLo = lo_hi(r, axis)[0];
            if (rHi <= mLo && (left === null || rHi > lo_hi(left, axis)[1])) { left = r; }
            if (rLo >= mHi && (right === null || rLo < lo_hi(right, axis)[0])) { right = r; }
        }
        if (left === null || right === null) { return null; }
        var lHi = lo_hi(left, axis)[1];
        var rLo2 = lo_hi(right, axis)[0];
        var size = mHi - mLo;
        var gap = (rLo2 - lHi - size) / 2;
        var offset = (lHi + gap) - mLo;
        if (Math.abs(offset) > threshold) { return null; }
        var perp = axis === "x" ? rect.y + rect.h / 2 : rect.x + rect.w / 2;
        var gaps = [
            { perp: perp, start: lHi, end: lHi + gap },
            { perp: perp, start: mHi + offset, end: rLo2 },
        ];
        return { offset: offset, gaps: gaps };
    }

    function pick_axis(align, space, axis) {
        var alignHit = align.offset !== 0 || align.guides.length > 0;
        if (space === null) {
            return align;
        }
        if (alignHit && Math.abs(align.offset) <= Math.abs(space.offset)) {
            return align;
        }
        return {
            offset: space.offset,
            guides: [{ axis: axis, kind: "spacing", gaps: space.gaps }],
        };
    }

    function forDrag(movingRect, targets, opts) {
        assert_rect(movingRect);
        if (!targets || !opts) {
            throw new Error("snap: forDrag needs targets and opts");
        }
        var rect = {
            x: movingRect.x, y: movingRect.y, w: movingRect.w, h: movingRect.h,
        };
        if (opts.suppress) {
            return { rect: rect, guides: [] };
        }
        var sx = snap_axis(rect, targets.xLines, "x", opts.threshold);
        var sy = snap_axis(rect, targets.yLines, "y", opts.threshold);
        var spaceX = spacing_offset(rect, targets.rects, "x", opts.threshold);
        var spaceY = spacing_offset(rect, targets.rects, "y", opts.threshold);
        var chosenX = pick_axis(sx, spaceX, "x");
        var chosenY = pick_axis(sy, spaceY, "y");
        rect.x = rect.x + chosenX.offset;
        rect.y = rect.y + chosenY.offset;
        if (opts.gridEnabled) {
            if (chosenX.offset === 0) { rect.x = Math.round(rect.x); }
            if (chosenY.offset === 0) { rect.y = Math.round(rect.y); }
        }
        return { rect: rect, guides: chosenX.guides.concat(chosenY.guides) };
    }

    function dim_match_positions(rects, fixedPos, axis, sign) {
        var out = [];
        var i = 0;
        for (i = 0; i < rects.length; i = i + 1) {
            var size = axis === "x" ? rects[i].w : rects[i].h;
            out.push({ pos: fixedPos + sign * size, source: "dimension" });
        }
        return out;
    }

    function snap_edge(edgePos, fixedPos, axis, sign, lines, rects, threshold) {
        var cands = lines.concat(dim_match_positions(rects, fixedPos, axis, sign));
        var best = best_offset([edgePos], cands, threshold);
        if (best === null) {
            return { pos: edgePos, guides: [] };
        }
        var snapped = edgePos + best.offset;
        return { pos: snapped, guides: coincident_guides([snapped], lines, axis) };
    }

    function forResize(movingRect, activeEdges, targets, opts) {
        assert_rect(movingRect);
        if (!activeEdges || !targets || !opts) {
            throw new Error("snap: forResize needs activeEdges, targets, opts");
        }
        var rect = {
            x: movingRect.x, y: movingRect.y, w: movingRect.w, h: movingRect.h,
        };
        if (opts.suppress) {
            return { rect: rect, guides: [] };
        }
        var guides = [];
        var left = rect.x;
        var right = rect.x + rect.w;
        var top = rect.y;
        var bottom = rect.y + rect.h;
        if (activeEdges.east) {
            var e = snap_edge(right, left, "x", 1, targets.xLines, targets.rects, opts.threshold);
            right = e.pos;
            guides = guides.concat(e.guides);
        } else if (activeEdges.west) {
            var w = snap_edge(left, right, "x", -1, targets.xLines, targets.rects, opts.threshold);
            left = w.pos;
            guides = guides.concat(w.guides);
        }
        if (activeEdges.south) {
            var s = snap_edge(bottom, top, "y", 1, targets.yLines, targets.rects, opts.threshold);
            bottom = s.pos;
            guides = guides.concat(s.guides);
        } else if (activeEdges.north) {
            var n = snap_edge(top, bottom, "y", -1, targets.yLines, targets.rects, opts.threshold);
            top = n.pos;
            guides = guides.concat(n.guides);
        }
        rect.x = left;
        rect.w = right - left;
        rect.y = top;
        rect.h = bottom - top;
        if (opts.gridEnabled) {
            rect.x = Math.round(rect.x);
            rect.y = Math.round(rect.y);
            rect.w = Math.round(rect.w);
            rect.h = Math.round(rect.h);
        }
        return { rect: rect, guides: guides };
    }

    function axisLock(dx, dy, shiftHeld) {
        if (!shiftHeld) {
            return { dx: dx, dy: dy, lockedAxis: null };
        }
        if (Math.abs(dx) >= Math.abs(dy)) {
            return { dx: dx, dy: 0, lockedAxis: "y" };
        }
        return { dx: 0, dy: dy, lockedAxis: "x" };
    }

    var snap = {
        forDrag: forDrag,
        forResize: forResize,
        axisLock: axisLock,
        __build_targets: build_targets,
    };

    if (typeof module !== "undefined" && module.exports) {
        module.exports = snap;
    }
    if (typeof window !== "undefined") {
        window.__snap = snap;
    }
}());
