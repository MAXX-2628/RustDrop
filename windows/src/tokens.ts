// Applies shared/design tokens as CSS custom properties. Components never hardcode colours.
import colors from "../../shared/design/colors.json";
import spacing from "../../shared/design/spacing.json";
import radius from "../../shared/design/radius.json";
import typography from "../../shared/design/typography.json";
import elevation from "../../shared/design/elevation.json";
import animation from "../../shared/design/animation.json";

export type ThemePreference = "system" | "dark" | "light";

const kebab = (s: string) => s.replace(/[A-Z]/g, (c) => "-" + c.toLowerCase());
const set = (name: string, value: string) =>
  document.documentElement.style.setProperty(`--${name}`, value);

export function applyTokens() {
  Object.entries(spacing).forEach(([k, v]) => set(`space-${k}`, `${v}px`));
  Object.entries(radius).forEach(([k, v]) => set(`radius-${k}`, `${v}px`));
  Object.entries(elevation).forEach(([k, v]) => set(`shadow-${k}`, v));
  Object.entries(typography.scale).forEach(([k, v]) => {
    set(`text-${kebab(k)}`, `${v.weight} ${v.size}px/${v.lineHeight} var(--font-body)`);
  });
  set("font-body", typography.fontFamily.windows);
  set("font-display", typography.fontFamily.windowsDisplay);
  Object.entries(animation.durationMs).forEach(([k, v]) => set(`duration-${kebab(k)}`, `${v}ms`));
  set("ease-enter", animation.easing.enter);
  set("ease-exit", animation.easing.exit);
}

const media = matchMedia("(prefers-color-scheme: light)");
let preference: ThemePreference = "system";

function paint() {
  const light = preference === "light" || (preference === "system" && media.matches);
  Object.entries(light ? colors.light : colors.dark).forEach(([k, v]) => set(`color-${kebab(k)}`, v));
  document.documentElement.style.colorScheme = light ? "light" : "dark";
}
media.addEventListener("change", paint);

export function setTheme(next: ThemePreference) {
  preference = next;
  try {
    localStorage.setItem("theme", next);
  } catch {
    /* storage unavailable: preference lasts for this session */
  }
  paint();
}

export function savedTheme(): ThemePreference {
  try {
    const saved = localStorage.getItem("theme");
    if (saved === "dark" || saved === "light") return saved;
  } catch {
    /* ignore */
  }
  return "system";
}
