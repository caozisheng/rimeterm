# Todo Tab Mouse Selection Design

## Goal

Add mouse text selection, clipboard copying, and an interactive vertical scrollbar to the embedded Todo tab without changing task editing or selection semantics.

## Decisions

- The Todo content area supports left-button drag selection by rendered row and display column.
- `Ctrl+C` and right-click copy the active text selection through the existing OSC 52 clipboard path. With no text selection, existing task-copy keybindings remain unchanged.
- The scrollbar occupies the right-most content column when the list is taller than its viewport. Wheel events scroll the list; left-button drag on the scrollbar moves the viewport proportionally.
- Scrollbar hit-testing has priority over text selection. Drag and mouse-up continue routing after the pointer leaves the pane through the existing `PaneProvider::scrollbar_dragging` contract.
- `rimeterm-tui` forwards mouse events to `tuxedo::EmbeddedApp`; Tuxedo owns selection and scrolling state.

## Architecture

`EmbeddedApp::on_mouse` will expose Tuxedo's mouse controller and return an `EmbeddedOutcome`. `TodoPane::on_mouse` translates absolute terminal coordinates into the embedded area, forwards the event, and tracks no duplicate UI state. `TodoPane` implements `scrollbar_dragging` by delegating to `EmbeddedApp`.

The list/archive renderers will build the same display lines used for rendering, reserve one column for the scrollbar when needed, render the selected range with a selection background, and publish the current body rectangle/line geometry to `App` for mouse hit-testing. Selection extraction will use the rendered line text, trim the reserved scrollbar column, and preserve line breaks.

## Behavior and Error Handling

- Click-drag starts selection only inside the rendered body text area.
- Empty or out-of-bounds selections are cleared and do not write to the clipboard.
- Copy failures are reported through the existing Tuxedo flash message.
- Scroll offsets clamp to the content's maximum offset after resize or content changes.
- Existing keyboard task selection, edit mode, and `yy`/`yb` behavior remain intact.

## Verification

Add unit tests for selection range normalization, display-column slicing, scrollbar offset mapping, and embedded mouse event forwarding. Run the focused `tuxedo` and `rimeterm-tui` test targets, then exercise the compiled TUI interaction path with a temporary Todo file and ratatui test backend where possible.
