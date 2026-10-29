# tilolang grammar

The canonical text of a package, as the parser (`src/parser.rs`) reads it and the printer
(`src/print.rs`) writes it. The README shows the language by example; this is the reference.

Notation: EBNF. `NL` is a new line (or `;`), which ends statements, fields and arms; commas are
optional at line ends. New lines inside brackets, after a binary operator and before a line that
starts with `.name` (a method call continuing the previous line) don't end anything.

```
file       = [ header ] { item } ;                    (* one file of the package has the header *)
header     = "app" app_id string [ "(" app_opt { "," app_opt } ")" ] ;
app_opt    = ( "icon" | "category" ) ":" "." ident
           | "previously" ":" ( app_id | "[" app_id { "," app_id } "]" ) ;
app_id     = ident { "." ident } ;

item       = { doc } ( use | type_decl | alias | enum_decl | root | default_view | row_view
                     | [ role ] action | flow | view | fn | scenario ) ;
use        = "use" app_id [ "as" ident ] ;
root       = "root" [ ident ":" ] type ;                      (* `root notes: Notes` *)
alias      = "type" Ident "=" type ;                          (* a container named so is a resource *)
default_view = "default" "view" ident "for" ( type | ident ) ;   (* a type, or a root by name *)
row_view   = "row" "view" ident "for" type ;
role       = "primary" | "destructive" | "migration" ;

type_decl  = "type" Ident [ "(" "origin" ":" Ident { "." Ident } ")" ] "{" { field | search } "}" ;
field      = { doc } ident ":" type [ "merge" merge_kind ] [ "(" ( "title" | "body" | "previously" ) ")" ]
             [ "=" expr ] ;
merge_kind = "register" | "counter" | "text" ;
search     = "search" "{" { search_line NL } "}" ;
search_line = "filter" ident [ string ] [ opts ]
            | "filter" string "=" expr [ opts ]
            | "sort" ident ( "newest" | "oldest" | "a_z" | "z_a" )
            | "hide" ident ;

enum_decl  = "enum" Ident "{" { { doc } ident [ opts ] [ "," ] } "}" ;

type       = one_type { "|" one_type } ;                         (* a union of resource types *)
one_type   = ident { "." ident }                                  (* Int, Note, todo.Item, std.Identifier *)
           | ( "List" | "Set" | "Map" | "Collection" | "Option" | "Result" | "State" )
             "<" type { "," type } ">"
           | "(" [ type { "," type } ] ")" [ "->" type ] ;         (* tuple, (), or a function type *)

action     = "action" ident params [ "->" type ] [ "proposal" ident [ "(" ident { "," ident } ")" ] ]
             [ opts ] block ;
flow       = "flow" ident params [ "->" type ] block ;
fn         = "fn" ident params "->" type block ;
view       = "view" ident [ "<" Ident { "," Ident } ">" ] params ui_block ;
scenario   = "scenario" string [ "->" type ] block ;
params     = "(" [ param { "," param } ] ")" ;
param      = ident ":" type [ "=" expr ] ;                         (* defaults: action parameters only *)
opts       = "(" ident ":" expr { "," ident ":" expr } ")" ;       (* icon:, label:, on: *)

block      = "{" { stmt NL } "}" ;
stmt       = "let" ident [ ":" type ] "=" expr
           | place ( "=" | "+=" | "-=" ) expr
           | "for" ident "in" expr block
           | "as" expr [ "at" expr ] block                          (* in a scenario: act as a person, at a time *)
           | "return" [ expr ]
           | expr ;
place      = ident { "." ident } ;

expr       = ident "->" expr                                       (* lambda: only as a method argument *)
           | [ or ] ".." [ or ]                                    (* range: a..b, a.., ..b *)
           | or ;
or         = and { ( "or" | "??" ) and } ;                        (* `??`: an Option's value or a fallback *)
and        = not { "and" not } ;
not        = "not" not | cmp ;
cmp        = add [ ( "==" | "!=" | "<" | "<=" | ">" | ">=" ) add ] ;
add        = mul { ( "+" | "-" ) mul } ;
mul        = unary { ( "*" | "/" | "%" ) unary } ;
unary      = ( "-" | "try" ) unary | postfix ;
postfix    = primary { "." ident | "?." ident [ "(" [ arg { "," arg } ] ")" ]
                     | "(" [ arg { "," arg } ] ")" } ;
arg        = [ ident ":" ] expr ;
primary    = int | float | length | duration | calendar | string | "true" | "false" | reference
           | ident | "root" type
           | "." ident [ "(" [ arg { "," arg } ] ")" ]             (* enum value: .some(x), .primary *)
           | "(" expr { "," expr } ")" | "[" [ expr { "," expr } ] "]"
           | block | if | match ;
if         = "if" ( expr | let_binds ) block [ "else" ( if | block ) ] ;
let_binds  = "let" ident "=" expr { "," [ "let" ] ident "=" expr } ;   (* each an Option *)
match      = "match" expr "{" { pattern "=>" expr ( "," | NL ) } "}" ;
pattern    = "." ident [ "(" ident { "," ident } ")" ]            (* `.some(x)`; "_" skips a field *)
           | ident ":" type ;                                     (* a union's case, by its type *)

ui_block   = "{" { ui_stmt NL } "}" ;
ui_stmt    = ( "window" | "lens" ) ident ":" type [ "=" expr ]     (* view state, at the top *)
           | "let" ident "=" expr
           | "for" ident "in" expr ui_block
           | "if" ( expr | let_binds ) ui_block [ "else" ( ui_if | ui_block ) ]
           | "match" expr "{" { pattern "=>" ( ui_block | ui_stmt ) ( "," | NL ) } "}"
           | "on" ident [ "(" ident [ ":" type ] ")" ] ":" expr    (* an event and its action *)
           | ident ":" ui_block                                    (* a named slot *)
           | string ":" expr                                       (* a fact: label and value *)
           | ui_call ;
ui_call    = postfix [ ui_block | "{" ident "->" { ui_stmt NL } "}" ] [ "as" ident ] ;
```

Tokens:

- `Ident` starts with an uppercase letter (types and enums), `ident` with a lowercase one.
  Keywords: `app use as type enum action flow view fn let if else match for in return and or not
  true false try merge proposal`. `window`, `lens`, `root`, `default`, `row`, `scenario`,
  `primary`, `destructive` and `migration` are keywords only where the grammar above uses them.
- `length` is a number with `rem`, `dp` or `fr` (`11rem`); `duration` an integer with `s`, `m`,
  `h`, `d` or `w` (`-7d`).
- `calendar` is a day `2026-06-29` (a Date), an instant `2026-06-29T07:02Z` or
  `2026-06-29T07:02:30Z` (a Time, in UTC), or a month `2026-08` (only as a filter's value), as
  search writes them.
- `string` is `"..."`; `{expr}` inside it interpolates.
- `reference` is `@app.Type:id`, `doc/item` for an item of a document, and an optional
  `@version` (`@so.tilo.todo.Todo:01J8/3K`). An older reference's `/field:item` steps still
  read, as the item of its last step.
- `.name` where a lambda goes is a key path, the lambda `x -> x.name` (`sort_by(.due)`).
- `//` starts a comment to the end of the line. `///` is a doc comment: it belongs to the
  declaration that follows, is kept in the metadata, and is what agents read.
