// The only script: a pause switch for the kite's loops (the hero never stops moving otherwise).
const still = document.getElementById("still");
if (still && !matchMedia("(prefers-reduced-motion: reduce)").matches) {
  still.hidden = false;
  still.addEventListener("click", () => {
    still.setAttribute("aria-pressed", document.documentElement.toggleAttribute("data-still"));
  });
}
