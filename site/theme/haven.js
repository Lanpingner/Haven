// Haven Terminal — the motion the stylesheet cannot do on its own.
//
// Three things: elements arrive as they scroll into view, the hero scene
// turns toward the pointer, and the exploded pane tree settles into one
// window as the hero scrolls away.
//
// All of it is decoration. The stylesheet renders a complete, readable page
// with this file absent — `.reveal` elements are visible until the class
// added here hides them, and every transform has a resting value declared in
// CSS. A failure to load costs the animation, never the content.

(function () {
    "use strict";

    var still = window.matchMedia("(prefers-reduced-motion: reduce)");
    var fine = window.matchMedia("(hover: hover) and (pointer: fine)");
    var wide = window.matchMedia("(min-width: 1080px)");

    function clamp(n, lo, hi) {
        return n < lo ? lo : n > hi ? hi : n;
    }

    // ---- 1. arrive on scroll -----------------------------------------
    function reveal() {
        var items = document.querySelectorAll(".reveal");
        if (!items.length) return;
        if (still.matches || !("IntersectionObserver" in window)) return;

        document.documentElement.classList.add("hv-motion");

        var seen = new IntersectionObserver(
            function (entries) {
                entries.forEach(function (entry) {
                    if (!entry.isIntersecting) return;
                    entry.target.classList.add("shown");
                    seen.unobserve(entry.target);
                });
            },
            { rootMargin: "0px 0px -8% 0px", threshold: 0.06 }
        );

        items.forEach(function (item) {
            // Already on screen at load: show it outright. A fade on something
            // the reader is looking at reads as a glitch, not an entrance.
            if (item.getBoundingClientRect().top < window.innerHeight * 0.92) {
                item.classList.add("shown");
            } else {
                seen.observe(item);
            }
        });
    }

    // ---- 2. the scene turns toward the pointer ------------------------
    //
    // The angle is written to custom properties; the transform that uses them
    // lives in the stylesheet, so there is one place that decides what the
    // scene looks like at a given angle.
    function parallax(scene) {
        if (still.matches || !fine.matches) return;

        var MAX_Y = 9; // degrees of yaw; past this the far pane edge-ons
        var MAX_X = 5; // and of pitch
        var queued = false;
        var at = { x: 0.5, y: 0.5 };

        function apply() {
            queued = false;
            if (!wide.matches) return;
            scene.style.setProperty("--ry", (at.x - 0.5) * 2 * MAX_Y + "deg");
            scene.style.setProperty("--rx", (0.5 - at.y) * 2 * MAX_X + "deg");
        }

        // The pointer is tracked over the whole hero rather than over the
        // scene alone: the scene is mostly empty space between planes, and a
        // tilt that only responds when the cursor is over a pane feels broken.
        var area = scene.closest(".hero") || scene;

        area.addEventListener("pointermove", function (event) {
            var box = area.getBoundingClientRect();
            at.x = (event.clientX - box.left) / box.width;
            at.y = (event.clientY - box.top) / box.height;
            if (queued) return;
            queued = true;
            requestAnimationFrame(apply);
        });

        area.addEventListener("pointerleave", function () {
            scene.style.removeProperty("--rx");
            scene.style.removeProperty("--ry");
        });
    }

    // ---- 3. the panes assemble as the hero scrolls away ---------------
    //
    // At rest the pane tree is exploded: each plane at its own depth, which
    // is what says they are separate programs. Scrolling brings them
    // together into the single window they actually are.
    function assemble(scene) {
        if (still.matches) return;

        var queued = false;

        function apply() {
            queued = false;
            var box = scene.getBoundingClientRect();
            if (!wide.matches) {
                scene.style.removeProperty("--spread");
                scene.style.removeProperty("--settle");
                return;
            }
            // 0 while the scene sits in place, 1 once it has travelled its own
            // height upward.
            var gone = clamp(-box.top / Math.max(box.height, 1), 0, 1);
            scene.style.setProperty("--spread", (1 - gone * 0.82).toFixed(3));
            scene.style.setProperty("--settle", (1 - gone).toFixed(3));
        }

        function schedule() {
            if (queued) return;
            queued = true;
            requestAnimationFrame(apply);
        }

        window.addEventListener("scroll", schedule, { passive: true });
        window.addEventListener("resize", schedule, { passive: true });
        apply();
    }

    // ---- 4. cards lean toward the pointer -----------------------------
    function leaning() {
        if (still.matches || !fine.matches) return;

        var MAX = 4.5;
        document.querySelectorAll(".cards .card").forEach(function (card) {
            var queued = false;
            var at = { x: 0.5, y: 0.5 };

            function apply() {
                queued = false;
                card.style.setProperty("--ry", (at.x - 0.5) * 2 * MAX + "deg");
                card.style.setProperty("--rx", (0.5 - at.y) * 2 * MAX + "deg");
            }

            card.addEventListener("pointermove", function (event) {
                var box = card.getBoundingClientRect();
                at.x = (event.clientX - box.left) / box.width;
                at.y = (event.clientY - box.top) / box.height;
                if (queued) return;
                queued = true;
                requestAnimationFrame(apply);
            });

            card.addEventListener("pointerleave", function () {
                card.style.removeProperty("--rx");
                card.style.removeProperty("--ry");
            });
        });
    }

    // ---- 5. the gallery shots tilt too --------------------------------
    function shots() {
        if (still.matches || !fine.matches) return;

        var MAX = 5;
        document.querySelectorAll(".shot[data-tilt]").forEach(function (shot) {
            var frame = shot.querySelector(".frame");
            if (!frame) return;
            var queued = false;
            var at = { x: 0.5, y: 0.5 };

            function apply() {
                queued = false;
                shot.style.setProperty("--ry", (at.x - 0.5) * 2 * MAX + "deg");
                shot.style.setProperty("--rx", (0.5 - at.y) * 2 * (MAX * 0.6) + "deg");
            }

            shot.addEventListener("pointermove", function (event) {
                var box = frame.getBoundingClientRect();
                at.x = (event.clientX - box.left) / box.width;
                at.y = (event.clientY - box.top) / box.height;
                if (queued) return;
                queued = true;
                requestAnimationFrame(apply);
            });

            shot.addEventListener("pointerleave", function () {
                shot.style.removeProperty("--rx");
                shot.style.removeProperty("--ry");
            });
        });
    }

    function start() {
        reveal();
        leaning();
        shots();

        var scene = document.querySelector("[data-scene]");
        if (scene) {
            parallax(scene);
            assemble(scene);
        }
    }

    if (document.readyState === "loading") {
        document.addEventListener("DOMContentLoaded", start);
    } else {
        start();
    }
})();
