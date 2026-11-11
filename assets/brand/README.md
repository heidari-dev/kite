# Kite brand

A diamond kite, a curved string and a short tail. It lifts and sways when the link is up and rests
when it is not (see [docs/design/motion.md](../../docs/design/motion.md)).

| File | Use |
| --- | --- |
| `kite.svg` | The mark, in colour |
| `kite-mono.svg` | One colour (`currentColor`), for docs, print and the website footer |
| `app-icon.svg` | Source for the Android, iPhone and desktop icons. Full square with no rounded corners, so each system can apply its own mask. The kite stays inside the central two thirds |
| `notification.svg` | Android status bar icon: white on transparent, no colour |
| `tray/*.svg` | Tray icons: `disconnected`, `connecting`, `connected`, `error` |

## Colours

| Name | Hex |
| --- | --- |
| Accent | `#E4572E` |
| Accent deep (right facet, string, tail) | `#B63E1C` |
| Paper (app icon tile) | `#F6F1E7` |
| Ink | `#1F2630` |
| Neutral (idle tray states) | `#7C828D` |

## Rules

- Keep the mark readable at 16 px. Below that, drop the string and the tail and show the diamond.
- Do not recolour, outline, add gradients or shadows, or rotate the mark. The tray icons are the
  only place the kite is shown lying down.
- Leave clear space around it of half the diamond's width.
- The SVGs are hand-written and have no embedded images. Keep them that way.
