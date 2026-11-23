# Cases from the Oils project's spec tests (spec/func-parsing.test.sh),
# https://github.com/oils-for-unix/oils, Apache-2.0. Expectations are bash's.

#### Function with spaces, to see if ( and ) are separate tokens.
# NOTE: Newline after ( is not OK.
fun ( ) { echo in-func; }; fun
## stdout: in-func

#### subshell function
# bash allows this.
i=0
j=0
inc() { i=$((i+5)); }
inc_subshell() ( j=$((j+5)); )
inc
inc_subshell
echo $i $j
## stdout: 5 0

#### Hard case, function with } token in it
rbrace() { echo }; }; rbrace
## stdout: }

#### . in function name
# bash accepts; dash doesn't
func-name.ext ( ) { echo func-name.ext; }
func-name.ext
## stdout: func-name.ext

#### = in function name
# WOW, bash is so lenient. foo=bar is a command, I suppose.  I  think I'm doing
# to disallow this one.
func-name=ext ( ) { echo func-name=ext; }
func-name=ext
## stdout: func-name=ext

#### Function name with !
# bash allows this; dash doesn't.
foo!bar() { ls ; }
## stdout-json: ""
## status: 0

#### Function name with -
# bash allows this; dash doesn't.
foo-bar() { ls ; }
## stdout-json: ""
## status: 0

#### Break after ) is OK.
# newline is always a token in "normal" state.
echo hi; fun ( )
{ echo in-func; }
fun
## STDOUT:
hi
in-func
## END

#### Nested definition
# A function definition is a command, so it can be nested
fun() {
  nested_func() { echo nested; }
  nested_func
}
fun
## stdout: nested

