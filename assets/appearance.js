// Appearance — light / dark / system chrome for the editor and landing windows.
//
// Two attributes live on <html>. `data-appearance` is the user's choice, written
// by Rust from the saved config before the document is handed to the webview.
// `data-theme` is that choice resolved against the OS setting, and is the only
// thing the stylesheets key off. This script runs from <head>, so the resolved
// theme is in place before the first paint and a dark session never flashes
// light.
//
// Exposes `window.__appearance.rotate()`, which advances light -> dark -> system
// and returns the new choice; the caller is responsible for persisting it. No
// inputs, no failure modes beyond an unknown stored mode, which falls back to
// "system".
(function () {
    "use strict";

    const MODES = ["light", "dark", "system"];
    const DARK_QUERY = window.matchMedia("(prefers-color-scheme: dark)");
    const root = document.documentElement;

    function resolve(mode) {
        if (mode === "light" || mode === "dark") {
            return mode;
        }
        return DARK_QUERY.matches ? "dark" : "light";
    }

    function apply(mode) {
        const chosen = MODES.indexOf(mode) >= 0 ? mode : "system";
        root.dataset.appearance = chosen;
        root.dataset.theme = resolve(chosen);
        return chosen;
    }

    function rotate() {
        const at = MODES.indexOf(root.dataset.appearance || "system");
        return apply(MODES[(at + 1) % MODES.length]);
    }

    apply(root.dataset.appearance);
    DARK_QUERY.addEventListener("change", function () {
        apply(root.dataset.appearance);
    });

    window.__appearance = { rotate: rotate, apply: apply };
})();
