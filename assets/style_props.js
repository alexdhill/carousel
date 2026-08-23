(function () {
    "use strict";

    const NAMED = {
        black: "#000000", white: "#ffffff", red: "#ff0000", green: "#008000",
        blue: "#0000ff", gray: "#808080", grey: "#808080", transparent: "#000000",
    };

    function clamp_alpha(n) {
        if (!isFinite(n)) {
            return 100;
        }
        return Math.max(0, Math.min(100, Math.round(n)));
    }

    function to_hex2(n) {
        const v = Math.max(0, Math.min(255, Math.round(n)));
        const s = v.toString(16);
        return s.length === 1 ? "0" + s : s;
    }

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

    function split_lengths_and_color(str) {

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

    function parseBorder(decls) {
        const d = decls || {};
        const short = split_border_shorthand(d.border || "");
        const widths = read_side_widths(d, short.width);
        const style = (d["border-style"] || short.style || "none").trim() || "none";
        const color = (d["border-color"] || short.color || "#000000").trim() || "#000000";
        return { style: style, widths: widths, color: color };
    }

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

        const tokens = s.match(/rgba?\([^)]*\)|[^\s]+/gi) || [];
        for (let i = 0; i < tokens.length; i++) {
            if (STYLES.indexOf(tokens[i].toLowerCase()) >= 0) {
                out.style = tokens[i].toLowerCase();
                break;
            }
        }
        return out;
    }

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

        const box = expand_box(short.split(/\s+/));
        return { tl: box.t, tr: box.r, br: box.b, bl: box.l };
    }

    function strip_px(v) {
        if (v == null) {
            return "0";
        }
        const s = String(v).replace(/px$/i, "").trim();
        return s === "" ? "0" : s;
    }

    function hexToRgb(hex) {
        const h = normalize_hex(String(hex == null ? "" : hex).trim()) || "#000000";
        return {
            r: parseInt(h.slice(1, 3), 16),
            g: parseInt(h.slice(3, 5), 16),
            b: parseInt(h.slice(5, 7), 16),
        };
    }

    function rgbToHex(r, g, b) {
        return "#" + to_hex2(r) + to_hex2(g) + to_hex2(b);
    }

    function splitLength(s) {
        const str = String(s == null ? "" : s).trim();
        const m = /^([-+]?[0-9]*\.?[0-9]+)\s*(px|em|rem|pt|in|pc|cm|mm)?$/i.exec(str);
        if (!m) {
            return { num: "", unit: "" };
        }
        return { num: m[1], unit: (m[2] || "").toLowerCase() };
    }

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
        splitLength: splitLength,
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
