# Single-row workspace header design

## Problem

When workspace tabs are enabled, `App::draw` reserves one row for the workspace strip and another for the status bar. Both show a workspace name; the status bar gives `workspace: <directory>` a flexible column, leaving a large empty area between it and the right-hand controls. The extra row reduces space available to panes.

## Decision

Use one top row. Keep the menu and workspace tabs on the left. Keep `workspace: <directory>`, `shell: <name>`, the `LANDSCAPE | VERTICAL` toggle, and the application `[×]` button as a compact, right-aligned group, in that order. Any spare width is a single flexible gap **between the tabs and the right-hand group**, not padding within the group.

```text
≡ rimeterm │ workspace [项目A ×] [项目B ×] [+]       workspace: 项目A │ shell: pwsh │ LANDSCAPE | VERTICAL │ [×]
                                                   ^ right-hand group stays anchored to the edge ^
```

The tab title remains distinct from the workspace directory name: users can rename tabs. Remove the strip's duplicated `rimeterm workspace:` prefix; the menu supplies the app name and the strip supplies the tab context. Keep the `[×]` inside each tab for closing that workspace; the far-right `[×]` continues to quit the application. The active tab remains underlined; the selected layout segment retains its current styling. When workspace tabs are disabled, the same status controls remain in the one top row, with no empty strip reservation.

## Width and interaction rules

- Compute widths from displayed terminal-cell widths, not byte or character counts. Give the right-hand group its content width and anchor its final cell at the right edge. No fixed 18-cell shell column or flexible workspace-name column.
- At comfortable widths, leave all excess cells in the gap between tabs and status. As width shrinks, consume that gap first; then truncate the directory name, shorten inactive tab titles, and compact the toggle labels to `L | V` with the full names shown in the existing hint bar on hover. Only at tighter widths hide the shell label and truncate the active tab. Keep the layout toggle and quit button reachable; do not paint or register hits beyond the tab region.
- Keep the active tab and `[+]` reachable when not all tabs fit, with a visible overflow affordance to reach hidden tabs. Constrain inline rename and caret to the tab region; do not let a long draft overwrite the status controls.
- The viewer's `F9 menu` hint uses space from the middle flexible region when available. If that region vanishes, preserve a discoverable hint without shifting or obscuring the right-hand controls.
- Derive mouse hit rectangles from exactly the clipped regions rendered that frame. Hover, per-tab close, add, menu, layout toggle, and app quit keep their existing actions.

## Verification criteria

At wide terminal widths, tabs appear on the left, all four status elements form a tightly spaced group at the right, and the pane begins on row 1 rather than row 2. At narrow widths, no labels overlap; the active workspace and right-side actions remain usable. Check tab switching, add/close, inline rename, layout switching, viewer `F9 menu`, menu, and quit through the actual TUI at both wide and narrow terminal sizes. Also check the tabs-disabled header.
