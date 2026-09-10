; TSX only: the JSX nodes, which the plain TypeScript grammar does not have.
; Appended to `typescript.scm` — see `TUNED` in `tuned.rs`.

; Rendering a component consumes it exactly as calling a function does, and it
; is the main consumption relation in a React codebase. The name node here is
; `identifier`, never `type_identifier`, so `(type_identifier) @type` never saw
; one of these.
(jsx_opening_element name: (identifier) @call)
(jsx_self_closing_element name: (identifier) @call)
