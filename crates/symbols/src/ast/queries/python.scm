; Definitions. A method lives in a class body and is reached through its class,
; so only module-level functions count.
(class_definition name: (identifier) @def)
(module (function_definition name: (identifier) @def))
(module (decorated_definition definition: (function_definition name: (identifier) @def)))

(call function: (identifier) @call)
(call function: (attribute attribute: (identifier) @call))

; Python has no type nodes, so the annotation's `type:` field is the signal.
(typed_parameter type: (type (identifier) @type))
(function_definition return_type: (type (identifier) @type))
(typed_parameter type: (type (subscript value: (identifier) @type)))
