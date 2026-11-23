# Cases from the Oils project's spec tests (spec/quote.test.sh),
# https://github.com/oils-for-unix/oils, Apache-2.0. Expectations are bash's.

#### Unquoted words
echo unquoted    words
## stdout: unquoted words

#### Single-quoted
echo 'single   quoted'
## stdout: single   quoted

#### Two single-quoted parts
echo 'two single-quoted pa''rts in one token'
## stdout: two single-quoted parts in one token

#### Unquoted and single quoted
echo unquoted' and single-quoted'
## stdout: unquoted and single-quoted

#### newline inside single-quoted string
echo 'newline
inside single-quoted string'
## STDOUT:
newline
inside single-quoted string
## END

#### Double-quoted
echo "double   quoted"
## stdout: double   quoted

#### Mix of quotes in one word
echo unquoted'  single-quoted'"  double-quoted  "unquoted
## stdout: unquoted  single-quoted  double-quoted  unquoted

#### Var substitution
FOO=bar
echo "==$FOO=="
## stdout: ==bar==

#### Var substitution with braces
FOO=bar
echo foo${FOO}
## stdout: foobar

#### Var substitution with braces, quoted
FOO=bar
echo "foo${FOO}"
## stdout: foobar

#### Var length
FOO=bar
echo "foo${#FOO}"
## stdout: foo3

#### Backslash escapes
echo \$ \| \a \b \c \d \\
## stdout: $ | a b c d \

#### Backslash escapes inside double quoted string
echo "\$ \\ \\ \p \q"
## stdout: $ \ \ \p \q

#### C-style backslash escapes inside double quoted string
# mksh and dash implement POSIX incompatible extensions.  $ ` " \ <newline>
# are the only special ones
echo "\a \b"
## stdout: \a \b

#### Literal $
echo $
## stdout: $

#### Quoted Literal $
echo $ "$" $
## stdout: $ $ $

#### Line continuation
echo foo\
$
## stdout: foo$

#### Line continuation inside double quotes
echo "foo\
$"
## stdout: foo$

#### Semicolon
echo separated; echo by semi-colon
## STDOUT:
separated
by semi-colon
## END

#### $''
echo $'foo'
## stdout: foo

#### $'' with quotes
echo $'single \' double \"'
## stdout: single ' double "

#### $'' with newlines
echo $'col1\ncol2\ncol3'
## STDOUT:
col1
col2
col3
## END

#### $"" is a synonym for ""
echo $"foo"
x=x
echo $"foo $x"
## STDOUT:
foo
foo x
## END

#### printf supports tabs
# This accepts \t by itself, hm.
printf "c1\tc2\nc3\tc4\n"
## stdout-json: "c1\tc2\nc3\tc4\n"

