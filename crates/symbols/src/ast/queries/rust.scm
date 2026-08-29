; Definitions: names this file introduces that other files can use.
; NOT `mod x;` — that names a module, not a usable symbol. NOT a method inside
; an `impl` — that is reached through its type. Both were counted by the regex
; this replaces, and `mod template;` alone produced 21% of one range's edges.
(struct_item name: (type_identifier) @def)
(enum_item name: (type_identifier) @def)
(union_item name: (type_identifier) @def)
(trait_item name: (type_identifier) @def)
(type_item name: (type_identifier) @def)
(macro_definition name: (identifier) @def)
(source_file (const_item name: (identifier) @def))
(source_file (static_item name: (identifier) @def))
(source_file (function_item name: (identifier) @def))
(source_file (mod_item body: (declaration_list (function_item name: (identifier) @def))))

; Calls, plain and through a receiver.
(call_expression function: (identifier) @call)
(call_expression function: (scoped_identifier name: (identifier) @call))
(call_expression function: (field_expression field: (field_identifier) @call))
(macro_invocation macro: (identifier) @call)

; Types used, including through a path.
(type_identifier) @type
