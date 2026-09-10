; Definitions. A method lives in one class body, so its name is declared in one
; place and callers reach it by that name (ADR 0030) — the same argument as an
; inherent `impl` in Rust, though Python is duck-typed and two classes may well
; answer to the same method name. The single-definer rule drops those.
(class_definition name: (identifier) @def)
(module (function_definition name: (identifier) @def))
(module (decorated_definition definition: (function_definition name: (identifier) @def)))
(class_definition body: (block (function_definition name: (identifier) @def)))
(class_definition body: (block (decorated_definition definition:
  (function_definition name: (identifier) @def))))

(call function: (identifier) @call)
(call function: (attribute attribute: (identifier) @call))

; Named without being called: `router.add(views.widgets)`. As in Go, an
; attribute read and a qualified name are one syntax here.
(attribute attribute: (identifier) @ref)

; Python has no type nodes, so the annotation's `type:` field is the signal.
(typed_parameter type: (type (identifier) @type))
(function_definition return_type: (type (identifier) @type))
(typed_parameter type: (type (subscript value: (identifier) @type)))

; File-local names: assignments, parameters and methods — reached through
; their class, or not at all outside this file (ADR 0030).
(assignment left: (identifier) @local_def)
(parameters (identifier) @local_def)
(typed_parameter (identifier) @local_def)

(identifier) @local_ref
