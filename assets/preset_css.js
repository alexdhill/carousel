(function () {
    "use strict";

    function presetRuleRegex() {
        return /\[data-element-type=["']([a-z]+)["']\]\s*\.([A-Za-z_-][\w-]*)\s*\{([^}]*)\}/g;
    }

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

    function buildPresetRule(type, className, decls) {
        const keys = Object.keys(decls || {});
        let body = "";
        for (let i = 0; i < keys.length; i++) {
            body += "    " + keys[i] + ": " + decls[keys[i]] + ";\n";
        }
        return '[data-element-type="' + type + '"].' + className + " {\n" + body + "}";
    }

    function escape_regex(s) {
        return String(s).replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
    }

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
