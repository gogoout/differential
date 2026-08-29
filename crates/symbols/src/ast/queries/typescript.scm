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

(call_expression function: (identifier) @call)
(call_expression function: (member_expression property: (property_identifier) @call))

(type_identifier) @type
