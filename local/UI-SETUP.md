# herdr UI setup (theme, sidebar, font)

Companion to `herdr-config.toml` and `windows-terminal-herdr-profile.json` in
this directory. Set up 2026-08-21, chasing the look of the hero screenshot on
https://herdr.dev.

**Depends on the patched build.** `[ui.sidebar.agents]` uses the `last_active`
token, which only exists in `0.8.0-pinned.bf66c2a1` or later — see
`PINNED-SPACES.md` and `sync-and-install.ps1`. A stock herdr rejects the whole
section as an unknown token, so restore the build before the config.

## Restoring on a fresh machine

1. Build and install the patched herdr (`sync-and-install.ps1`).
2. `winget install --id DEVCOM.JetBrainsMonoNerdFont --exact`
3. Copy `herdr-config.toml` to `%APPDATA%\herdr\config.toml`, fixing the
   absolute path in the `keys.command` entry if the username differs.
4. Merge `windows-terminal-herdr-profile.json` into Windows Terminal's
   `profiles.list`, and set `profiles.defaults.colorScheme` to
   `Catppuccin Mocha`.
5. In herdr, reload with `ctrl+b shift+r`. No restart needed — the reload path
   re-resolves the palette and the sidebar rows, and never touches PTYs, so
   running agents are unaffected.

herdr never writes `config.toml` itself (only its tests do), so the comments and
hand-formatting in that file are safe indefinitely.

## What was changed, and why

Three problems, all variations on "the colour is technically there but you
cannot see it".

### 1. Agent names invisible against the panel

`src/ui/sidebar.rs` hardcodes the secondary agent style as `overlay0` plus the
`DIM` modifier, and that style is **not** reachable from `[theme.custom]` — no
theme name or token override can undo the `DIM`. Windows Terminal implements
`DIM` by blending the foreground toward the background, so catppuccin's
`overlay0` (`#6c7086`) collapsed to near-invisible on the `#1e1e2e` panel.

Fixed with per-token styles in `[ui.sidebar.agents].rows`, where a row entry may
be `{ token = "...", fg = "#rrggbb", bold = ..., dim = ... }` and each key
patches the style the renderer already chose. `dim = false` removes the
modifier; the explicit `fg` then sets the grey directly instead of relying on a
blend.

`state_text` was added to row 2 and deliberately left without an `fg`: the
renderer colours it per agent state (yellow working / red blocked / green done),
and setting `fg` would flatten all three into one colour. That state colouring
is most of what makes the herdr.dev screenshot read the way it does.

### 2. Selection band and separators invisible

The active space row and active agent row are painted with
`bg(p.surface_dim)`, and catppuccin ships `surface_dim = #1e1e2e` — all but
identical to `panel_bg` `#181825`. The highlight was being drawn the whole time,
just indistinguishable. Same cause for the horizontal rule above the `agents`
header and the vertical sidebar edge, which are drawn with `surface_dim` as a
*foreground*.

One override (`surface_dim = "#2a2b3c"`) fixes all of them, because it is a
single token doing double duty. That also means they cannot be tuned
independently from config — a stronger selection band necessarily means heavier
separators. Splitting them would need a source change.

### 3. Font

herdr has no font handling at all — grepping the source for `font_size` /
`font_family` returns nothing. It is a pure TUI, so face and size belong to
Windows Terminal, per profile. This is also why `Ctrl +` never persisted:
runtime zoom is session-only and never written back to `settings.json`.

Face is `JetBrainsMono NFM`, **not** `JetBrainsMono NF`. `NFM` is Nerd Font
*Mono*, with icon glyphs constrained to a single cell. Plain `NF` uses
double-width icons that overhang into the neighbouring cell and smear a
grid-based TUI's column alignment; `NFP` is proportional and wrong outright.

Note that winget carries only the Nerd Font build — plain JetBrains Mono is not
packaged there. The Nerd Font is a superset with identical letterforms, and its
glyph coverage is what stops herdr's box-drawing and the 📌 pins from falling
through to Windows' font fallback.

## Do not naively switch this to a light theme

Every hex in `[theme.custom]` and in the sidebar `fg` keys was chosen against
`#181825`. There is **one** `[theme.custom]` table and it is applied on top of
whichever palette resolves — there is no per-appearance variant. Against
`catppuccin-latte`:

| Override | Set to | Latte's own | Result on light |
| --- | --- | --- | --- |
| `surface_dim` | `#2a2b3c` | `#e6e9ef` | near-black selection band under `p.text` `#4c4f69` — dark on dark, the original bug mirrored |
| `accent` | `#cba6f7` | `#1e66f5` | ~1.5:1 on `#eff1f5`; active tab label all but vanishes |
| agent `fg` | `#9399b2` | — | ~2.6:1, below WCAG — and no theme switch can fix it, because the hex wins |

So switching `theme.name` alone gives a *worse* result than stock latte. A real
light setup means deleting `[theme.custom]` and both `fg` keys, not editing
`theme.name`. The `dim = false` keys stay correct either way, and `state_text`
adapts on its own.

Windows Terminal has to move at the same time. Every herdr palette sets
`sidebar_bg: Color::Reset` to let the terminal background show through, so a
light herdr on a Mocha terminal paints light foregrounds onto a dark sidebar.

`auto_switch` is not the shortcut it looks like: the overrides above poison both
directions, `light_name`/`dark_name` here are solarized rather than catppuccin,
and on Windows the DEC 996 colour-scheme query is compiled out
(`#[cfg(any(not(windows), test))]` in `src/terminal_theme.rs`), leaving
detection to OSC 11 luminance inference with a fallback of Dark.

## Tuning

- Agent name too quiet: `#9399b2` → `#a6adc8` (catppuccin `subtext0`).
- Selection band too weak: `surface_dim` → `#313244`; too strong, or separators
  too heavy: → `#232334`.
- Font too large: JetBrains Mono has a taller x-height and wider advance than
  Cascadia, so 14pt reads larger than Cascadia's 14pt. Drop to 13.
- The vertical sidebar edge turns `accent` in Navigate mode. That is intended
  behaviour, not the override leaking.
