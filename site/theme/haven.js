// Haven Terminal — the two bits of motion the stylesheet cannot do alone.
//
// Both are decoration. If this file fails to load, or the reader has asked
// for reduced motion, the page is the same page: the stylesheet's reduced
// motion block already shows every `.reveal` element and flattens every tilt,
// so nothing here is needed to read anything.

(function () {
    "use strict";

    var still = window.matchMedia("(prefers-reduced-motion: reduce)");

    // ---- 1. arrive on scroll ----------------------------------------
    //
    // `.reveal` elements are visible in the stylesheet and only hidden under
    // `html.haven-motion`, which is set here. So the fade is opt-in: if this
    // file never loads, or the browser has no IntersectionObserver, or the
    // reader asked for stillness, the class is never added and the content was
    // never hidden in the first place.
    function reveal() {
        var items = document.querySelectorAll(".reveal");
        if (!items.length) return;
        if (still.matches || !("IntersectionObserver" in window)) return;

        document.documentElement.classList.add("haven-motion");

        var seen = new IntersectionObserver(
            function (entries) {
                entries.forEach(function (entry) {
                    if (!entry.isIntersecting) return;
                    entry.target.classList.add("shown");
                    seen.unobserve(entry.target);
                });
            },
            { rootMargin: "0px 0px -8% 0px", threshold: 0.08 }
        );

        items.forEach(function (item) {
            // Anything already on screen when the page opens should not fade
            // in — that reads as a glitch rather than as an entrance.
            if (item.getBoundingClientRect().top < window.innerHeight * 0.92) {
                item.classList.add("shown");
            } else {
                seen.observe(item);
            }
        });
    }

    // ---- 2. the screenshots follow the pointer -----------------------
    //
    // The angle is written to two custom properties and the transform itself
    // lives in the stylesheet, so there is one place that decides what a tilt
    // looks like.
    function tilt() {
        if (still.matches) return;
        if (!window.matchMedia("(hover: hover) and (pointer: fine)").matches) return;

        var MAX = 5; // degrees; past about six the text stops being readable

        document.querySelectorAll(".shot[data-tilt]").forEach(function (shot) {
            var frame = shot.querySelector(".frame");
            if (!frame) return;
            var queued = false;
            var point = { x: 0.5, y: 0.5 };

            function apply() {
                queued = false;
                shot.style.setProperty("--ry", (point.x - 0.5) * 2 * MAX + "deg");
                shot.style.setProperty("--rx", (0.5 - point.y) * 2 * (MAX * 0.6) + "deg");
            }

            shot.addEventListener("pointermove", function (event) {
                var box = frame.getBoundingClientRect();
                point.x = (event.clientX - box.left) / box.width;
                point.y = (event.clientY - box.top) / box.height;
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
        tilt();
    }

    if (document.readyState === "loading") {
        document.addEventListener("DOMContentLoaded", start);
    } else {
        start();
    }
})();
