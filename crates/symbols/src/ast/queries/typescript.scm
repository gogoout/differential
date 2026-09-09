; Definitions, exported or not — a file-scope name is usable either way.
(class_declaration name: (type_identifier) @def)
(interface_declaration name: (type_identifier) @def)
(type_alias_declaration name: (type_identifier) @def)
(enum_declaration name: (identifier) @def)
(program (function_declaration name: (identifier) @def))
(program (export_statement declaration: (function_declaration name: (identifier) @def)))
(program (export_statement declaration: (class_declaration name: (type_identifier) @def)))
(program (export_statement declaration: (interface_declaration name: (type_identifier) @def)))
(program (export_statement declaration: (type_alias_declaration name: (type_identifier) @def)))

; An EXPORTED file-scope value declaration, which in this language is where the
; components, the hooks and the constants live: `export const Panel = () => …`
; introduces `Panel` exactly as `function Panel()` would. Leaving these out made
; the graph a TYPE graph — on one corpus range every single edge came from an
; interface name.
;
; Exported, and only exported. A definition is a file-scope name OTHERS CAN USE
; (ADR 0023), and in a module system `export` is exactly that predicate. A bare
; top-level `const send = vi.fn()` in a test file is not one, and counting it
; linked every production file that calls `send` to that test — the same
; false-definition failure ADR 0023 was written about. Unexported, it is still
; picked up below as a file-local name, so it keeps every edge it can honestly
; draw.
(program (export_statement declaration:
  (lexical_declaration (variable_declarator name: (identifier) @def))))
(program (export_statement declaration:
  (variable_declaration (variable_declarator name: (identifier) @def))))

(call_expression function: (identifier) @call)
(call_expression function: (member_expression property: (property_identifier) @call))

(type_identifier) @type

; File-local names. A binding declared inside a function reaches only its own
; file, so it may only ever draw an edge to another class in that file — which
; is what makes counting it safe (ADR 0030).
(variable_declarator name: (identifier) @local_def)
(object_pattern (shorthand_property_identifier_pattern) @local_def)
(required_parameter pattern: (identifier) @local_def)
(optional_parameter pattern: (identifier) @local_def)
(import_specifier name: (identifier) @local_def)
(namespace_import (identifier) @local_def)

; Every identifier might be reading one of them. `(identifier)` does not match
; a `property_identifier`, so `data.rule` offers `data` and not `rule`.
(identifier) @local_ref
