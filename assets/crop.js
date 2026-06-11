// crop.js
// Pure, DOM-free crop math for image elements. Consumes plain slide-coordinate
// rects ({ x, y, w, h }) and natural image dims ({ w, h }); returns crop state
// ({ iw, ih, dx, dy }) and CSS-ready style strings. No DOM, no IPC.
(function () {
    "use strict";

    // assert_dims
    // Inputs: an object expected to be a finite, positive { w, h }, and a
    // label. Output: throws when w/h are not finite or not positive.
    function assert_dims(d, label) {
        if (!d || typeof d !== "object") {
            throw new Error("crop: " + label + " must be an object");
        }
        if (!(isFinite(d.w) && isFinite(d.h) && d.w > 0 && d.h > 0)) {
            throw new Error("crop: " + label + " w/h must be finite and positive");
        }
    }

    // cover_scale
    // Inputs: mask { w, h }, natural { w, h }. Output: the smallest scale at
    // which the natural image fully covers the mask.
    function cover_scale(mask, natural) {
        return Math.max(mask.w / natural.w, mask.h / natural.h);
    }

    // fromCover
    // Inputs: mask rect, natural dims. Output: crop state { iw, ih, dx, dy }
    // reproducing CSS `background-size: cover; background-position: center`.
    function fromCover(mask, natural) {
        assert_dims(mask, "mask");
        assert_dims(natural, "natural");
        var scale = cover_scale(mask, natural);
        var iw = natural.w * scale;
        var ih = natural.h * scale;
        return { iw: iw, ih: ih, dx: (mask.w - iw) / 2, dy: (mask.h - ih) / 2 };
    }

    // toStyles
    // Inputs: crop state. Output: { backgroundSize, backgroundPosition } CSS.
    function toStyles(state) {
        if (!state) {
            throw new Error("crop: toStyles needs state");
        }
        return {
            backgroundSize: state.iw + "px " + state.ih + "px",
            backgroundPosition: state.dx + "px " + state.dy + "px",
        };
    }

    // parse_pair
    // Inputs: a "<a>px <b>px" string. Output: [a, b] numbers, or null when not
    // two explicit px values.
    function parse_pair(s) {
        if (typeof s !== "string") {
            return null;
        }
        var m = s.trim().match(/^(-?\d*\.?\d+)px\s+(-?\d*\.?\d+)px$/);
        if (!m) {
            return null;
        }
        return [parseFloat(m[1]), parseFloat(m[2])];
    }

    // fromStyles
    // Inputs: background-size and background-position strings. Output: crop
    // state, or null when either is not an explicit px pair (i.e. uncropped).
    function fromStyles(bgSize, bgPos) {
        var sz = parse_pair(bgSize);
        var ps = parse_pair(bgPos);
        if (sz === null || ps === null) {
            return null;
        }
        return { iw: sz[0], ih: sz[1], dx: ps[0], dy: ps[1] };
    }

    // clampPan
    // Inputs: crop state, mask rect. Output: state with dx, dy clamped so the
    // image always covers the mask (no exposed gap). Assumes iw>=mask.w,
    // ih>=mask.h.
    function clampPan(state, mask) {
        if (!state) {
            throw new Error("crop: clampPan needs state");
        }
        assert_dims(mask, "mask");
        var minDx = mask.w - state.iw;
        var minDy = mask.h - state.ih;
        var dx = Math.min(0, Math.max(minDx, state.dx));
        var dy = Math.min(0, Math.max(minDy, state.dy));
        return { iw: state.iw, ih: state.ih, dx: dx, dy: dy };
    }

    // pan
    // Inputs: crop state, mask, slide-px pan deltas. Output: panned + clamped
    // state.
    function pan(state, mask, ddx, ddy) {
        if (!isFinite(ddx) || !isFinite(ddy)) {
            throw new Error("crop: pan deltas must be finite");
        }
        return clampPan(
            { iw: state.iw, ih: state.ih, dx: state.dx + ddx, dy: state.dy + ddy },
            mask,
        );
    }

    // zoom
    // Inputs: crop state, mask, natural dims, multiplicative factor. Output:
    // state scaled about the MASK CENTER, aspect locked to natural, never
    // smaller than cover, pan re-clamped.
    function zoom(state, mask, natural, factor) {
        assert_dims(mask, "mask");
        assert_dims(natural, "natural");
        if (!isFinite(factor) || factor <= 0) {
            throw new Error("crop: zoom factor must be positive finite");
        }
        var minIw = cover_scale(mask, natural) * natural.w;
        var newIw = Math.max(minIw, state.iw * factor);
        var newIh = newIw * (natural.h / natural.w);
        var cx = mask.w / 2;
        var cy = mask.h / 2;
        var fx = (cx - state.dx) / state.iw;
        var fy = (cy - state.dy) / state.ih;
        var next = { iw: newIw, ih: newIh, dx: cx - fx * newIw, dy: cy - fy * newIh };
        return clampPan(next, mask);
    }

    // reclampForMask
    // Inputs: crop state, the (possibly resized) mask, natural dims. Output:
    // state zoomed up to the new cover baseline when the mask grew past the
    // image, then pan re-clamped. Aspect locked to natural.
    function reclampForMask(state, mask, natural) {
        assert_dims(mask, "mask");
        assert_dims(natural, "natural");
        var minIw = cover_scale(mask, natural) * natural.w;
        var iw = Math.max(state.iw, minIw);
        var ih = iw * (natural.h / natural.w);
        return clampPan({ iw: iw, ih: ih, dx: state.dx, dy: state.dy }, mask);
    }

    // placeImage
    // Inputs: crop state, the (possibly resized) mask, the image's desired
    // top-left position in CANVAS/slide coordinates, and natural dims. Output:
    // state whose dx/dy place the image at that canvas origin (so resizing the
    // mask does not move the image), zoomed up to cover when the mask grew
    // past the image, then pan re-clamped. Aspect locked to natural.
    function placeImage(state, mask, imgCanvasX, imgCanvasY, natural) {
        assert_dims(mask, "mask");
        assert_dims(natural, "natural");
        if (!isFinite(imgCanvasX) || !isFinite(imgCanvasY)) {
            throw new Error("crop: placeImage origin must be finite");
        }
        var minIw = cover_scale(mask, natural) * natural.w;
        var iw = Math.max(state.iw, minIw);
        var ih = iw * (natural.h / natural.w);
        var dx = imgCanvasX - mask.x;
        var dy = imgCanvasY - mask.y;
        return clampPan({ iw: iw, ih: ih, dx: dx, dy: dy }, mask);
    }

    // scaleForBox
    // Inputs: crop state and the old + new mask box dimensions. Output: state
    // with the image size and pan scaled by the box's per-axis ratios, so the
    // picture scales WITH the element box and the crop framing is preserved
    // (B-proportional resize). Cover is maintained automatically because the
    // image and box scale by the same factors.
    function scaleForBox(state, oldW, oldH, newW, newH) {
        if (!(oldW > 0 && oldH > 0)) {
            throw new Error("crop: scaleForBox old dims must be positive");
        }
        if (!(isFinite(newW) && isFinite(newH))) {
            throw new Error("crop: scaleForBox new dims must be finite");
        }
        var sx = newW / oldW;
        var sy = newH / oldH;
        return {
            iw: state.iw * sx,
            ih: state.ih * sy,
            dx: state.dx * sx,
            dy: state.dy * sy,
        };
    }

    // zoomPercent
    // Inputs: crop state, mask, natural. Output: zoom as a percentage where
    // 100 means the image exactly covers the mask.
    function zoomPercent(state, mask, natural) {
        var minIw = cover_scale(mask, natural) * natural.w;
        return (state.iw / minIw) * 100;
    }

    // setZoomPercent
    // Inputs: target percent (>=100), crop state, mask, natural. Output: state
    // zoomed so iw equals pct% of the cover width, re-clamped.
    function setZoomPercent(pct, state, mask, natural) {
        if (!isFinite(pct) || pct <= 0) {
            throw new Error("crop: percent must be positive finite");
        }
        var minIw = cover_scale(mask, natural) * natural.w;
        var targetIw = minIw * (pct / 100);
        return zoom(state, mask, natural, targetIw / state.iw);
    }

    var crop = {
        fromCover: fromCover,
        toStyles: toStyles,
        fromStyles: fromStyles,
        clampPan: clampPan,
        pan: pan,
        zoom: zoom,
        reclampForMask: reclampForMask,
        placeImage: placeImage,
        scaleForBox: scaleForBox,
        zoomPercent: zoomPercent,
        setZoomPercent: setZoomPercent,
        __cover_scale: cover_scale,
    };

    if (typeof module !== "undefined" && module.exports) {
        module.exports = crop;
    }
    if (typeof window !== "undefined") {
        window.__crop = crop;
    }
}());
