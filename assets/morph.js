// morph.js — shared FLIP animation engine for slide-element transitions.
//
// Reads data-morph-* attributes (data-morph-next, data-morph-dur,
// data-morph-ease) off elements in the OLD slide root and animates the
// same-id element in the NEW slide root from the old box to the new box.
// The new element itself is transformed in place inside its own shadow root
// (never cloned into the light DOM), so theme CSS and font custom properties
// stay applied; the old element is hidden so no ghost is left behind. Pure
// DOM — no IPC or deck-model knowledge. Both playback engines call run_morph
// on a forward cross-slide advance with the two overlapping shadow roots.
(function () {
    "use strict";

    var MAX_MORPH = 512;

    // stage_scale
    // Output: the #stage CSS scale factor (both engines scale the stage to fit
    // the window). getBoundingClientRect returns post-scale screen pixels, so a
    // transform applied in an element's local space must divide screen deltas by
    // this factor. Defaults to 1 when absent/unparseable.
    function stage_scale() {
        var stage = document.getElementById("stage");
        if (!stage || !stage.style.transform) {
            return 1;
        }
        var match = stage.style.transform.match(/scale\(([0-9.]+)\)/);
        if (!match) {
            return 1;
        }
        var value = parseFloat(match[1]);
        return value > 0 ? value : 1;
    }

    // collect_morph_pairs
    // Inputs: old_root, new_root (shadow roots or DOM nodes).
    // Output: array of {old_el, new_el, duration_ms, easing} for each morph
    // element in old_root that has a matching data-element-id in new_root.
    // Skips any morph element with no match (does not throw).
    function collect_morph_pairs(old_root, new_root) {
        var pairs = [];
        var old_els = old_root.querySelectorAll("[data-morph-next='1']");
        for (var i = 0; i < old_els.length && i < MAX_MORPH; i++) {
            var old_el = old_els[i];
            var old_id = old_el.getAttribute("data-element-id");
            if (!old_id) {
                continue;
            }
            var new_el = new_root.querySelector("[data-element-id=\"" + old_id + "\"]");
            if (!new_el) {
                continue;
            }
            var duration_ms = parseInt(old_el.getAttribute("data-morph-dur"), 10) || 400;
            var easing = old_el.getAttribute("data-morph-ease") || "ease";
            pairs.push({
                old_el: old_el,
                new_el: new_el,
                duration_ms: duration_ms,
                easing: easing,
            });
        }
        return pairs;
    }

    // animate_pair
    // Inputs: pair {old_el, new_el, duration_ms, easing}, done callback.
    // Output: side-effect; hides old_el, parks new_el over old_el's box via an
    // inverse transform, then transitions the transform to identity so new_el
    // slides/scales into its real position. Calls done() on transitionend or a
    // timeout guard. Restores new_el's prior transform state on finish.
    function animate_pair(pair, done) {
        var new_el = pair.new_el;
        var o = pair.old_el.getBoundingClientRect();
        var n = new_el.getBoundingClientRect();
        var scale = stage_scale();
        var dx = (o.left - n.left) / scale;
        var dy = (o.top - n.top) / scale;
        var sx = n.width ? o.width / n.width : 1;
        var sy = n.height ? o.height / n.height : 1;
        var prev_origin = new_el.style.transformOrigin;
        var prev_transform = new_el.style.transform;
        var prev_transition = new_el.style.transition;
        pair.old_el.style.visibility = "hidden";
        new_el.style.transformOrigin = "top left";
        new_el.style.transition = "none";
        new_el.style.transform =
            "translate(" + dx + "px," + dy + "px) scale(" + sx + "," + sy + ")";
        void new_el.offsetWidth;
        new_el.style.transition = "transform " + pair.duration_ms + "ms " + pair.easing;
        new_el.style.transform = "none";
        var finished = false;
        var finish = function () {
            if (finished) {
                return;
            }
            finished = true;
            window.clearTimeout(timer);
            new_el.removeEventListener("transitionend", on_end);
            new_el.style.transition = prev_transition;
            new_el.style.transform = prev_transform;
            new_el.style.transformOrigin = prev_origin;
            done();
        };
        var on_end = function (e) {
            if (e.propertyName === "transform") {
                finish();
            }
        };
        var timer = window.setTimeout(finish, pair.duration_ms + 100);
        new_el.addEventListener("transitionend", on_end);
    }

    // run_morph
    // Inputs: old_root, new_root (overlapping shadow roots — new stacked over
    //         old), on_done (called exactly once when all morphs finish, or
    //         immediately when there are no morph pairs).
    // Output: side-effect; animates each matched pair from old box to new box.
    window.run_morph = function (old_root, new_root, on_done) {
        var pairs = collect_morph_pairs(old_root, new_root);
        var pair_count = pairs.length;
        if (pair_count === 0) {
            on_done();
            return;
        }
        var finished_count = 0;
        var on_pair_done = function () {
            finished_count = finished_count + 1;
            if (finished_count >= pair_count) {
                on_done();
            }
        };
        for (var i = 0; i < pairs.length; i++) {
            animate_pair(pairs[i], on_pair_done);
        }
    };
}());
