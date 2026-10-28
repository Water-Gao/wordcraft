# Dual-pane workspace specification

> Status: implemented · 2026-10-28 · Rust state tests + real browser split editing/reference checks

## Product rules

- One workspace shows either one document viewport or two side-by-side document viewports.
- Both viewports display and edit the same active document, selection, undo history, dirty state and save channel.
- Scroll offset, zoom, viewport geometry and visible page are view-local. Document revision and page content are shared.
- Only one viewport owns the active caret/selection. Clicking a viewport makes it active; scrolling alone does not.
- The active viewport is visibly identified. Inactive viewports repaint after edits but do not own keyboard input.
- A reference pane is read-only and is not a second document instance. File preview selection and AI inclusion are independent states.

## State ownership

- `Session`: document, caret/selection, revision, commands, undo and open documents.
- `WordApp.canvas`: the viewport state currently loaded for drawing or input.
- `WordApp.secondary_canvas`: the other viewport state. Swapping viewport states never clones or swaps the `Session`.
- `Workspace`: split visibility, active pane and which pane `canvas` currently represents.
- Shell/server: reference files, checked reference IDs and upload/preview APIs.

## Host API

- `workspace.inspect` returns `{ split, activePane, panes: [{ id, viewedPage, scale }] }`.
- `workspace.setSplit { enabled }` enables or disables the second document viewport.
- `view.focusPane { pane: "left" | "right" }` changes the active viewport without changing the document selection.

## Acceptance scenarios

1. Enable split with one document open. Both panes render the same revision and only the active pane owns the caret.
2. Scroll the inactive pane. Its visible page changes while the active pane and selection remain unchanged.
3. Edit in one pane. The other pane reflects the same document revision without a save or reload.
4. Click and edit in the other pane. Focus moves, the same undo stack is used, and a single save contains both edits.
5. Agent inspection reports the active pane and independent viewed pages. Focusing a pane does not duplicate a document.
6. Disable split. The previously active pane remains the single editor and its scroll position is preserved.
7. Reference preview never changes document selection; checked reference IDs are the only implicit AI reference scope.
