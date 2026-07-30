/** @type {import('tailwindcss').Config} */
export default {
  content: ["./index.html", "./src/**/*.{ts,tsx}"],
  darkMode: "class",
  theme: {
    extend: {
      colors: {
        // Semantic surface tokens — see docs/design-system.md. Defined as CSS variables
        // (index.css) so a single place controls the dark theme's near-black hierarchy,
        // referenced here with the rgb(var(...) / <alpha-value>) pattern so Tailwind's
        // opacity modifiers (e.g. `bg-app/60`) keep working.
        app: "rgb(var(--color-app) / <alpha-value>)",
        surface: "rgb(var(--color-surface) / <alpha-value>)",
        "surface-raised": "rgb(var(--color-surface-raised) / <alpha-value>)",
        // Borders/dividers deliberately use plain `border-white/N` opacity utilities in
        // components instead of a dedicated token — low-contrast-on-dark is exactly what
        // white-at-low-alpha gives for free, no extra indirection needed.
        // Helppye's single accent color — a refined violet, deliberately distinct from
        // generic Tailwind "indigo". One accent only; no competing hues (see CLAUDE.md).
        brand: {
          50: "#F2EFFF",
          100: "#E6E0FF",
          200: "#CBC0FF",
          300: "#AB98FF",
          400: "#8C6FFF",
          500: "#7C5CFC",
          600: "#6A46EE",
          700: "#5636C7",
          800: "#452B9E",
          900: "#38257D",
          950: "#231650",
        },
      },
      borderRadius: {
        xl2: "1.25rem",
      },
      boxShadow: {
        soft: "0 1px 2px rgb(0 0 0 / 0.24), 0 8px 24px -12px rgb(0 0 0 / 0.45)",
        raised: "0 2px 4px rgb(0 0 0 / 0.3), 0 16px 40px -16px rgb(0 0 0 / 0.55)",
        "glow-brand": "0 0 0 1px rgb(124 92 252 / 0.4), 0 8px 24px -8px rgb(124 92 252 / 0.35)",
      },
      transitionDuration: {
        DEFAULT: "180ms",
      },
      transitionTimingFunction: {
        DEFAULT: "cubic-bezier(0.4, 0, 0.2, 1)",
      },
      keyframes: {
        "fade-in": { from: { opacity: 0 }, to: { opacity: 1 } },
        "rise-in": {
          from: { opacity: 0, transform: "translateY(4px)" },
          to: { opacity: 1, transform: "translateY(0)" },
        },
        shimmer: {
          "0%": { backgroundPosition: "-200% 0" },
          "100%": { backgroundPosition: "200% 0" },
        },
        "pulse-soft": {
          "0%, 100%": { opacity: 1 },
          "50%": { opacity: 0.55 },
        },
      },
      animation: {
        "fade-in": "fade-in 200ms ease-out",
        "rise-in": "rise-in 220ms cubic-bezier(0.4, 0, 0.2, 1)",
        shimmer: "shimmer 1.8s linear infinite",
        "pulse-soft": "pulse-soft 1.6s ease-in-out infinite",
      },
    },
  },
  plugins: [],
};
