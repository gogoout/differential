; Definitions. A method has a receiver and is reached through its type, so only
; plain functions and type declarations count.
(type_declaration (type_spec name: (type_identifier) @def))
(source_file (function_declaration name: (identifier) @def))
(source_file (const_declaration (const_spec name: (identifier) @def)))

(call_expression function: (identifier) @call)
(call_expression function: (selector_expression field: (field_identifier) @call))

(type_identifier) @type

; File-local names: short declarations, vars, and methods reached through
; their receiver (ADR 0030).
(short_var_declaration left: (expression_list (identifier) @local_def))
(var_spec name: (identifier) @local_def)
(const_spec name: (identifier) @local_def)
(method_declaration name: (field_identifier) @local_def)

(identifier) @local_ref
(field_identifier) @local_ref
