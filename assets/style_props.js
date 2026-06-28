// style_props — pure parse/compose helpers for the structured Fill / Border /
// Shadow inspector sections. No DOM. Dual export: `module.exports` for the
// node test runner and `window.__style` for the host webview (host.js reads it
// the same way it reads `window.__crop` / `window.__snap`).
//
// All numbers cross the boundary as plain strings WITHOUT a unit (the inspector
// number inputs are unitless); compose helpers append "px" where CSS needs it.
(function () {
    "use strict";

    // Minimal named-colour fallback. The native swatch only renders hex, so
    // names seen on legacy elements resolve to a best-effort hex; anything
    // unknown falls back to black rather than throwing.
    const NAMED = {
        black: "#000000", white: "#ffffff", red: "#ff0000", green: "#008000",
        blue: "#0000ff", gray: "#808080", grey: "#808080", transparent: "#000000",
    };

    // clamp01_100
    // Inputs: a number. Output: that number clamped to the inclusive 0..100
    // integer range used by the opacity fields.
    function clamp_alpha(n) {
        if (!isFinite(n)) {
            return 100;
        }
        return Math.max(0, Math.min(100, Math.round(n)));
    }

    // to_hex2
    // Inputs: a 0..255 channel number. Output: a two-digit lowercase hex pair.
    function to_hex2(n) {
        const v = Math.max(0, Math.min(255, Math.round(n)));
        const s = v.toString(16);
        return s.length === 1 ? "0" + s : s;
    }

    // normalize_hex
    // Inputs: a "#rgb" or "#rrggbb" string. Output: a "#rrggbb" lowercase
    // string, or "" when the input is not a hex colour.
    function normalize_hex(s) {
        const m3 = /^#([0-9a-f])([0-9a-f])([0-9a-f])$/i.exec(s);
        if (m3) {
            return ("#" + m3[1] + m3[1] + m3[2] + m3[2] + m3[3] + m3[3]).toLowerCase();
        }
        if (/^#[0-9a-f]{6}$/i.test(s)) {
            return s.toLowerCase();
        }
        return "";
    }

    // parseRgba
    // Inputs: a colour string (hex, rgb(), rgba(), or a known name).
    // Output: { hex: "#rrggbb", alpha: 0..100 }. Unparseable input yields the
    // neutral default { "#000000", 100 }. alpha is the percent the opacity
    // field shows; a missing rgba alpha is 100.
    function parseRgba(str) {
        const s = String(str == null ? "" : str).trim();
        const fallback = { hex: "#000000", alpha: 100 };
        if (s === "" || s === "none") {
            return fallback;
        }
        const hex = normalize_hex(s);
        if (hex !== "") {
            return { hex: hex, alpha: 100 };
        }
        const m = /^rgba?\(([^)]+)\)$/i.exec(s);
        if (m) {
            const parts = m[1].split(",").map(function (p) { return p.trim(); });
            if (parts.length >= 3) {
                const r = Number(parts[0]);
                const g = Number(parts[1]);
                const b = Number(parts[2]);
                const a = parts.length >= 4 ? Number(parts[3]) : 1;
                if (isFinite(r) && isFinite(g) && isFinite(b)) {
                    return {
                        hex: "#" + to_hex2(r) + to_hex2(g) + to_hex2(b),
                        alpha: clamp_alpha((isFinite(a) ? a : 1) * 100),
                    };
                }
            }
        }
        const named = NAMED[s.toLowerCase()];
        return named ? { hex: named, alpha: 100 } : fallback;
    }

    // composeRgba
    // Inputs: a hex colour and a 0..100 alpha. Output: a CSS colour string —
    // the bare hex when alpha is 100 (cleaner inline style), otherwise
    // rgba(r,g,b,a) with a two-decimal alpha. Invalid hex coerces to black.
    function composeRgba(hex, alpha) {
        const h = normalize_hex(String(hex == null ? "" : hex).trim()) || "#000000";
        const a = clamp_alpha(Number(alpha));
        if (a >= 100) {
            return h;
        }
        const r = parseInt(h.slice(1, 3), 16);
        const g = parseInt(h.slice(3, 5), 16);
        const b = parseInt(h.slice(5, 7), 16);
        const af = Math.round(a) / 100;
        return "rgba(" + r + ", " + g + ", " + b + ", " + af + ")";
    }

    // split_lengths_and_color
    // Inputs: a CSS value string. Output: { lengths: [..numeric strings..],
    // color: "<color token or ''>" }. The colour token is the first run that
    // looks like a colour (hex / rgb() / rgba() / known name); every other
    // token is treated as a length with its unit stripped. Used by box-shadow
    // and border-shorthand parsing.
    function split_lengths_and_color(str) {
        // Split on whitespace, but keep rgb()/rgba() groups intact.
        const tokens = String(str).match(/rgba?\([^)]*\)|[^\s]+/gi) || [];
        const lengths = [];
        let color = "";
        for (let i = 0; i < tokens.length; i++) {
            const t = tokens[i];
            const is_color = /^#/.test(t) || /^rgba?\(/i.test(t)
                || Object.prototype.hasOwnProperty.call(NAMED, t.toLowerCase());
            if (is_color && color === "") {
                color = t;
            } else if (/[-+0-9.]/.test(t)) {
                lengths.push(t.replace(/px$/i, ""));
            }
        }
        return { lengths: lengths, color: color };
    }

    // parseBoxShadow
    // Inputs: a box-shadow value string ("x y blur spread color", spread and
    // colour optional; "inset" not supported and is ignored).
    // Output: { x, y, blur, spread, color } — offsets/blur/spread as unitless
    // numeric strings, colour as a CSS colour string. Empty / "none" → zeros
    // with a black colour.
    function parseBoxShadow(str) {
        const s = String(str == null ? "" : str).trim();
        const out = { x: "0", y: "0", blur: "0", spread: "0", color: "#000000" };
        if (s === "" || s === "none") {
            return out;
        }
        const parts = split_lengths_and_color(s);
        const L = parts.lengths;
        if (L.length >= 1) { out.x = L[0]; }
        if (L.length >= 2) { out.y = L[1]; }
        if (L.length >= 3) { out.blur = L[2]; }
        if (L.length >= 4) { out.spread = L[3]; }
        if (parts.color !== "") { out.color = parts.color; }
        return out;
    }

    // composeBoxShadow
    // Inputs: { x, y, blur, spread, color } (unitless numeric strings + colour).
    // Output: a "Xpx Ypx Blurpx Spreadpx color" box-shadow string. Missing
    // numeric parts default to 0; missing colour defaults to black.
    function composeBoxShadow(s) {
        const o = s || {};
        const px = function (v) {
            const n = Number(String(v == null ? "" : v).trim());
            return (isFinite(n) ? n : 0) + "px";
        };
        const color = String(o.color == null || o.color === "" ? "#000000" : o.color);
        return px(o.x) + " " + px(o.y) + " " + px(o.blur) + " " + px(o.spread)
            + " " + color;
    }

    // expand_box
    // Inputs: an array of 1..4 unitless length strings (CSS box shorthand
    // order). Output: { t, r, b, l } following the CSS 1/2/3/4-value rules.
    // Empty input → all "0".
    function expand_box(vals) {
        const v = (vals || []).map(function (x) { return String(x).replace(/px$/i, ""); });
        if (v.length === 0) {
            return { t: "0", r: "0", b: "0", l: "0" };
        }
        if (v.length === 1) {
            return { t: v[0], r: v[0], b: v[0], l: v[0] };
        }
        if (v.length === 2) {
            return { t: v[0], r: v[1], b: v[0], l: v[1] };
        }
        if (v.length === 3) {
            return { t: v[0], r: v[1], b: v[2], l: v[1] };
        }
        return { t: v[0], r: v[1], b: v[2], l: v[3] };
    }

    // parseBorder
    // Inputs: a declaration map (property → value) for the element.
    // Output: { style, widths: { t, r, b, l }, color }. Reads the per-side
    // width longhands first, falling back to the `border-width` shorthand, then
    // to the all-in-one `border` shorthand. style/color likewise prefer their
    // own longhand then the `border` shorthand. Defaults: style "none",
    // widths 0, colour "#000000".
    function parseBorder(decls) {
        const d = decls || {};
        const short = split_border_shorthand(d.border || "");
        const widths = read_side_widths(d, short.width);
        const style = (d["border-style"] || short.style || "none").trim() || "none";
        const color = (d["border-color"] || short.color || "#000000").trim() || "#000000";
        return { style: style, widths: widths, color: color };
    }

    // read_side_widths
    // Inputs: the declaration map and a shorthand-width fallback string.
    // Output: { t, r, b, l } unitless. Prefers the four longhands; when none
    // are present uses the `border-width` shorthand, then the `border`
    // shorthand width.
    function read_side_widths(d, shorthand_width) {
        const has_long = ("border-top-width" in d) || ("border-right-width" in d)
            || ("border-bottom-width" in d) || ("border-left-width" in d);
        if (has_long) {
            return {
                t: strip_px(d["border-top-width"]),
                r: strip_px(d["border-right-width"]),
                b: strip_px(d["border-bottom-width"]),
                l: strip_px(d["border-left-width"]),
            };
        }
        if (d["border-width"]) {
            return expand_box(String(d["border-width"]).trim().split(/\s+/));
        }
        if (shorthand_width !== "") {
            return { t: shorthand_width, r: shorthand_width, b: shorthand_width, l: shorthand_width };
        }
        return { t: "0", r: "0", b: "0", l: "0" };
    }

    // split_border_shorthand
    // Inputs: a `border` shorthand string ("1px solid #ccc", any order).
    // Output: { width, style, color } as unitless width + raw style/color
    // tokens (any missing piece is "").
    function split_border_shorthand(str) {
        const s = String(str || "").trim();
        const out = { width: "", style: "", color: "" };
        if (s === "" || s === "none") {
            return out;
        }
        const STYLES = ["none", "hidden", "solid", "dashed", "dotted", "double",
            "groove", "ridge", "inset", "outset"];
        const parts = split_lengths_and_color(s);
        if (parts.color !== "") { out.color = parts.color; }
        if (parts.lengths.length >= 1) { out.width = parts.lengths[0]; }
        // Style is a bare keyword token (not a length, not a colour).
        const tokens = s.match(/rgba?\([^)]*\)|[^\s]+/gi) || [];
        for (let i = 0; i < tokens.length; i++) {
            if (STYLES.indexOf(tokens[i].toLowerCase()) >= 0) {
                out.style = tokens[i].toLowerCase();
                break;
            }
        }
        return out;
    }

    // parseRadius
    // Inputs: a declaration map. Output: { tl, tr, br, bl } unitless. Prefers
    // the four corner longhands, falling back to the `border-radius` shorthand
    // (CSS 1/2/3/4-value form). Defaults all to "0".
    function parseRadius(decls) {
        const d = decls || {};
        const has_long = ("border-top-left-radius" in d)
            || ("border-top-right-radius" in d)
            || ("border-bottom-right-radius" in d)
            || ("border-bottom-left-radius" in d);
        if (has_long) {
            return {
                tl: strip_px(d["border-top-left-radius"]),
                tr: strip_px(d["border-top-right-radius"]),
                br: strip_px(d["border-bottom-right-radius"]),
                bl: strip_px(d["border-bottom-left-radius"]),
            };
        }
        const short = String(d["border-radius"] || "").trim();
        if (short === "") {
            return { tl: "0", tr: "0", br: "0", bl: "0" };
        }
        // border-radius box order is tl, tr, br, bl (1/2/3/4-value rules).
        const box = expand_box(short.split(/\s+/));
        return { tl: box.t, tr: box.r, br: box.b, bl: box.l };
    }

    // strip_px
    // Inputs: any value. Output: the value as a unitless numeric string ("0"
    // when empty / non-numeric).
    function strip_px(v) {
        if (v == null) {
            return "0";
        }
        const s = String(v).replace(/px$/i, "").trim();
        return s === "" ? "0" : s;
    }

    // hexToRgb
    // Inputs: a "#rgb" / "#rrggbb" string. Output: { r, g, b } in 0..255;
    // unparseable input yields black. Used to seed the HSL edit model.
    function hexToRgb(hex) {
        const h = normalize_hex(String(hex == null ? "" : hex).trim()) || "#000000";
        return {
            r: parseInt(h.slice(1, 3), 16),
            g: parseInt(h.slice(3, 5), 16),
            b: parseInt(h.slice(5, 7), 16),
        };
    }

    // rgbToHex
    // Inputs: r, g, b channels (0..255, clamped/rounded). Output: a
    // "#rrggbb" lowercase string.
    function rgbToHex(r, g, b) {
        return "#" + to_hex2(r) + to_hex2(g) + to_hex2(b);
    }

    // rgbToHsl
    // Inputs: r, g, b in 0..255. Output: { h, s, l } with h in 0..360 and
    // s, l in 0..100. Achromatic inputs report h = 0, s = 0. Standard HSL
    // conversion; no DOM, pure arithmetic.
    function rgbToHsl(r, g, b) {
        const rn = Math.max(0, Math.min(255, r)) / 255;
        const gn = Math.max(0, Math.min(255, g)) / 255;
        const bn = Math.max(0, Math.min(255, b)) / 255;
        const max = Math.max(rn, gn, bn);
        const min = Math.min(rn, gn, bn);
        const l = (max + min) / 2;
        const d = max - min;
        let h = 0;
        let s = 0;
        if (d !== 0) {
            s = d / (1 - Math.abs(2 * l - 1));
            if (max === rn) {
                h = ((gn - bn) / d) % 6;
            } else if (max === gn) {
                h = (bn - rn) / d + 2;
            } else {
                h = (rn - gn) / d + 4;
            }
            h = h * 60;
            if (h < 0) {
                h = h + 360;
            }
        }
        return { h: h, s: s * 100, l: l * 100 };
    }

    // hslToRgb
    // Inputs: h (0..360), s, l (0..100). Output: { r, g, b } in 0..255
    // (rounded). Inverse of rgbToHsl. Inputs are wrapped/clamped so slider
    // values never throw.
    function hslToRgb(h, s, l) {
        const hn = ((h % 360) + 360) % 360;
        const sn = Math.max(0, Math.min(100, s)) / 100;
        const ln = Math.max(0, Math.min(100, l)) / 100;
        const c = (1 - Math.abs(2 * ln - 1)) * sn;
        const x = c * (1 - Math.abs(((hn / 60) % 2) - 1));
        const m = ln - c / 2;
        let rp = 0;
        let gp = 0;
        let bp = 0;
        if (hn < 60) {
            rp = c; gp = x;
        } else if (hn < 120) {
            rp = x; gp = c;
        } else if (hn < 180) {
            gp = c; bp = x;
        } else if (hn < 240) {
            gp = x; bp = c;
        } else if (hn < 300) {
            rp = x; bp = c;
        } else {
            rp = c; bp = x;
        }
        return {
            r: Math.round((rp + m) * 255),
            g: Math.round((gp + m) * 255),
            b: Math.round((bp + m) * 255),
        };
    }

    const style = {
        parseRgba: parseRgba,
        composeRgba: composeRgba,
        hexToRgb: hexToRgb,
        rgbToHex: rgbToHex,
        rgbToHsl: rgbToHsl,
        hslToRgb: hslToRgb,
        parseBoxShadow: parseBoxShadow,
        composeBoxShadow: composeBoxShadow,
        parseBorder: parseBorder,
        parseRadius: parseRadius,
        __expand_box: expand_box,
        __split_border_shorthand: split_border_shorthand,
    };

    if (typeof module !== "undefined" && module.exports) {
        module.exports = style;
    }
    if (typeof window !== "undefined") {
        window.__style = style;
    }
}());
