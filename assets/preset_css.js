// preset_css — pure parse/build helpers for style presets stored in the
// layout's globals CSS. No DOM. Dual export: `module.exports` for the node
// test runner and `window.__preset` for the host webview.
//
// A preset is a type-scoped class rule:
//   [data-element-type="text"].heading { font-size:48px; ... }
// The element-type qualifier is what scopes a preset to text vs shape vs … and
// is the only selector form the tooling recognises (every element renders as a
// <div>, so tag selectors like p.x never match). Rules without the qualifier
// are ignored.
(function () {
    "use strict";

    // Matches one tooling preset rule: the element-type attribute, the class,
    // and the declaration block. `g` flag for iteration.
    function presetRuleRegex() {
        return /\[data-element-type=["']([a-z]+)["']\]\s*\.([A-Za-z_-][\w-]*)\s*\{([^}]*)\}/g;
    }

    // parse_decls
    // Inputs: a declaration block body ("k: v; k: v"). Output: an ordered
    // { prop: value } map (insertion order preserved).
    function parse_decls(body) {
        const out = {};
        const parts = String(body).split(";");
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
            if (k !== "" && v !== "") {
                out[k] = v;
            }
        }
        return out;
    }

    // parsePresets
    // Inputs: a globals CSS string. Output: an array of
    // { type, className, declarations } for every type-scoped preset rule, in
    // source order. Non-qualified class rules are skipped (not matched).
    function parsePresets(cssText) {
        const text = String(cssText == null ? "" : cssText);
        const re = presetRuleRegex();
        const out = [];
        let m = re.exec(text);
        let guard = 0;
        while (m !== null && guard < 10000) {
            out.push({
                type: m[1],
                className: m[2],
                declarations: parse_decls(m[3]),
            });
            m = re.exec(text);
            guard += 1;
        }
        return out;
    }

    // buildPresetRule
    // Inputs: an element type, a class name, an ordered declaration map.
    // Output: a formatted rule string (one declaration per line).
    function buildPresetRule(type, className, decls) {
        const keys = Object.keys(decls || {});
        let body = "";
        for (let i = 0; i < keys.length; i++) {
            body += "    " + keys[i] + ": " + decls[keys[i]] + ";\n";
        }
        return '[data-element-type="' + type + '"].' + className + " {\n" + body + "}";
    }

    // escape_regex
    // Inputs: a string. Output: the string with regex metacharacters escaped.
    function escape_regex(s) {
        return String(s).replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
    }

    // upsertPresetRule
    // Inputs: the current globals CSS, an element type, a class name, and the
    // declaration map. Output: new globals CSS with the (type, class) rule
    // replaced in place when it already exists, else appended. Control flow:
    // build the rule, look for an existing same-selector block, splice or
    // append.
    function upsertPresetRule(cssText, type, className, decls) {
        const text = String(cssText == null ? "" : cssText);
        const rule = buildPresetRule(type, className, decls);
        const sel = '\\[data-element-type=["\']' + escape_regex(type) + '["\']\\]\\s*\\.'
            + escape_regex(className) + '\\s*\\{[^}]*\\}';
        const re = new RegExp(sel);
        if (re.test(text)) {
            return text.replace(re, rule);
        }
        const trimmed = text.replace(/\s+$/, "");
        return trimmed === "" ? rule : trimmed + "\n\n" + rule + "\n";
    }

    // slugifyClass
    // Inputs: a display name. Output: a CSS-safe class token (lowercase, dashed,
    // never empty, never digit-initial).
    function slugifyClass(name) {
        let s = String(name == null ? "" : name).toLowerCase()
            .replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "");
        if (s === "") {
            s = "preset";
        }
        if (/^[0-9]/.test(s)) {
            s = "p-" + s;
        }
        return s;
    }

    const preset = {
        parsePresets: parsePresets,
        buildPresetRule: buildPresetRule,
        upsertPresetRule: upsertPresetRule,
        slugifyClass: slugifyClass,
    };

    if (typeof module !== "undefined" && module.exports) {
        module.exports = preset;
    }
    if (typeof window !== "undefined") {
        window.__preset = preset;
    }
}());
