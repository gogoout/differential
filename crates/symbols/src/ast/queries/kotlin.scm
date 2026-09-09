; Kotlin's `call_expression` has NO `function:` field and its
; `navigation_expression` names none of its children, which is why the generic
; field rule found no calls at all here. A query can still see the shape.
(class_declaration name: (identifier) @def)
(object_declaration name: (identifier) @def)
(source_file (function_declaration name: (identifier) @def))
(source_file (property_declaration (variable_declaration (identifier) @def)))

(call_expression (identifier) @call)
(call_expression (navigation_expression (identifier) @call .))

; A type position is `user_type`, whose child is the bare name.
(user_type (identifier) @type)

; File-local names: any property declaration, including the ones inside a
; function or a class body that the file-scope rule above drops (ADR 0030).
(variable_declaration (identifier) @local_def)
(function_declaration name: (identifier) @local_def)

(identifier) @local_ref
