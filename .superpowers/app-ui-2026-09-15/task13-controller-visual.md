# Code Editor Task 13 controller visual gate

Root re-inspected the accepted Task 10 final default 800×900@2 PNG before Task 13: task10-code-final-20260916/captures/code_editor/large-text.png. Dock is correctly collapsed and the trigger remains visible, but code and gutter lines overlap severely because scaled text still advances on fixed app-local line pitch. The horizontal clipping of a long code line is separate from this vertical overlap; preserve the editor horizontal scrolling model. Task 10 ordinary-size composition remains accepted.

Task 13 must preserve the disclosure/focus fix, use one metric model across paint/caret/selection/hits/scroll/overlays, and repair dock bands. No new IME or per-line accessibility feature. Source preflight: code-metrics-preflight.md. Final captures/interactions and independent review pending; code_editor-corrected stays open.
