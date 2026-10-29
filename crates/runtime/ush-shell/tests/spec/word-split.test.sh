# Cases from the Oils project's spec tests (spec/word-split.test.sh),
# https://github.com/oils-for-unix/oils, Apache-2.0. Expectations are bash's.

#### IFS is scoped
IFS=b
word=abcd
f() { local IFS=c; argv.py $word; }
f
argv.py $word
## STDOUT:
['ab', 'd']
['a', 'cd']
## END

#### Tilde sub is not split, but var sub is
HOME="foo bar"
argv.py ~
argv.py $HOME
## STDOUT:
['foo bar']
['foo', 'bar']
## END

#### Word splitting
a="1 2"
b="3 4"
argv.py $a"$b"
## STDOUT:
['1', '23 4']
## END

#### Word splitting 2
a="1 2"
b="3 4"
c="5 6"
d="7 8"
argv.py $a"$b"$c"$d"
## STDOUT:
['1', '23 45', '67 8']
## END

#### $*
fun() { argv.py -$*-; }
fun "a 1" "b 2" "c 3"
## stdout: ['-a', '1', 'b', '2', 'c', '3-']

#### "$*"
fun() { argv.py "-$*-"; }
fun "a 1" "b 2" "c 3"
## stdout: ['-a 1 b 2 c 3-']

#### $@
# How does this differ from $* ?  I don't think it does.
fun() { argv.py -$@-; }
fun "a 1" "b 2" "c 3"
## stdout: ['-a', '1', 'b', '2', 'c', '3-']

#### "$@"
fun() { argv.py "-$@-"; }
fun "a 1" "b 2" "c 3"
## stdout: ['-a 1', 'b 2', 'c 3-']

#### empty argv
argv.py 1 "$@" 2 $@ 3 "$*" 4 $* 5
## stdout: ['1', '2', '3', '', '4', '5']

#### $* with empty IFS
set -- "1 2" "3  4"

IFS=
argv.py $*
argv.py "$*"
## STDOUT:
['1 2', '3  4']
['1 23  4']
## END

#### Word elision with space
s1=' '
argv.py $s1
## stdout: []

#### Word elision with non-whitespace IFS
# Treated differently than the default IFS.  What is the rule here?
IFS='_'
char='_'
space=' '
empty=''
argv.py $char
argv.py $space
argv.py $empty
## STDOUT:
['']
[' ']
[]
## END

#### Leading/trailing word elision with non-whitespace IFS
# This behavior is weird.
IFS=_
s1='_a_b_'
argv.py $s1
## stdout: ['', 'a', 'b']

#### Leading ' ' vs leading ' _ '
# This behavior is weird, but all shells agree.
IFS='_ '
s1='_ a  b _ '
s2='  a  b _ '
argv.py $s1
argv.py $s2
## STDOUT:
['', 'a', 'b']
['a', 'b']
## END

#### Multiple non-whitespace IFS chars.
IFS=_-
s1='a__b---c_d'
argv.py $s1
## stdout: ['a', '', 'b', '', '', 'c', 'd']

#### IFS with whitespace and non-whitepace.
# NOTE: Three delimiters means two empty words in the middle.  No elision.
IFS='_ '
s1='a_b _ _ _ c  _d e'
argv.py $s1
## stdout: ['a', 'b', '', '', 'c', 'd', 'e']

#### empty $@ and $* is elided
fun() { argv.py 1 $@ $* 2; }
fun
## stdout: ['1', '2']

#### unquoted empty arg is elided
empty=""
argv.py 1 $empty 2
## stdout: ['1', '2']

#### unquoted whitespace arg is elided
space=" "
argv.py 1 $space 2
## stdout: ['1', '2']

#### empty literals are not elided
space=" "
argv.py 1 $space"" 2
## stdout: ['1', '', '2']

#### no splitting when IFS is empty
IFS=""
foo="a b"
argv.py $foo
## stdout: ['a b']

#### default value can yield multiple words
argv.py 1 ${undefined:-"2 3" "4 5"} 6
## stdout: ['1', '2 3', '4 5', '6']

#### default value can yield multiple words with part joining
argv.py 1${undefined:-"2 3" "4 5"}6
## stdout: ['12 3', '4 56']

#### default value with unquoted IFS char
IFS=_
argv.py 1${undefined:-"2_3"x_x"4_5"}6
## stdout: ['12_3x', 'x4_56']

#### IFS characters are glob metacharacters
IFS='* '
s='a*b c'
argv.py $s

IFS='?'
s='?x?y?z?'
argv.py $s

IFS='['
s='[x[y[z['
argv.py $s
## STDOUT:
['a', 'b', 'c']
['', 'x', 'y', 'z']
['', 'x', 'y', 'z']
## END

#### Trailing space
argv.py 'Xec  ho '
argv.py X'ec  ho '
argv.py X"ec  ho "
## STDOUT:
['Xec  ho ']
['Xec  ho ']
['Xec  ho ']
## END

#### Empty IFS (regression for bug)
IFS=
echo ["$*"]
set a b c
echo ["$*"]
## STDOUT:
[]
[abc]
## END

#### Unset IFS (regression for bug)
set a b c
unset IFS
echo ["$*"]
## STDOUT:
[a b c]
## END

#### IFS=o (regression for bug)
IFS=o
echo hi
## STDOUT:
hi
## END

#### IFS and joining arrays
IFS=:
set -- x 'y z'
argv.py "$@"
argv.py $@
argv.py "$*"
argv.py $*
## STDOUT:
['x', 'y z']
['x', 'y z']
['x:y z']
['x', 'y z']
## END

#### TODO
empty=""
space=" "
AB="A B"
X="X"
Yspaces=" Y "
## stdout-json: ""

#### IFS='' with $@ and $* and printf (bug #627)
set -- a 'b c'
IFS=''
printf '[%s]\n' $@
printf '[%s]\n' $*
## STDOUT:
[a]
[b c]
[a]
[b c]
## END

#### Bug #698, similar crash
var='\'
set -f
echo $var
## STDOUT:
\
## END

#### Bug #1664, \\ with noglob

# Note that we're not changing IFS

argv.py [\\]_
argv.py "[\\]_"

# TODO: no difference observed here, go back to original bug

#argv.py [\\_
#argv.py "[\\_"

echo noglob

# repeat cases with -f, noglob
set -f

argv.py [\\]_
argv.py "[\\]_"

#argv.py [\\_
#argv.py "[\\_"
## STDOUT:
['[\\]_']
['[\\]_']
noglob
['[\\]_']
['[\\]_']
## END

#### Empty IFS bug #2141 (from pnut)

res=0
sum() {
  # implement callee-save calling convention using `set`
  # here, we save the value of $res after the function parameters
  set $@ $res           # $1 $2 $3 are now set
  res=$(($1 + $2))
  echo "$1 + $2 = $res"
  res=$3                # restore the value of $res
}

unset IFS
sum 12 30 # outputs "12 + 30 = 42"

IFS=' '
sum 12 30 # outputs "12 + 30 = 42"

IFS=
sum 12 30 # outputs "1230 + 0 = 1230"

# I added this
IFS=''
sum 12 30

set -u
IFS=
sum 12 30 # fails with "fatal: Undefined variable '2'" on res=$(($1 + $2))
## STDOUT:
12 + 30 = 42
12 + 30 = 42
12 + 30 = 42
12 + 30 = 42
12 + 30 = 42
## END

#### ""$A"" - empty string on both sides - derived from spec/toysh-posix #15

A="   abc   def   "

argv.py $A
argv.py ""$A""

unset IFS

argv.py $A
argv.py ""$A""

echo

# Do the same thing in a for loop - this is IDENTICAL behavior

for i in $A; do echo =$i=; done
echo

for i in ""$A""; do echo =$i=; done
echo

unset IFS

for i in $A; do echo =$i=; done
echo

for i in ""$A""; do echo =$i=; done
## STDOUT:
['abc', 'def']
['', 'abc', 'def', '']
['abc', 'def']
['', 'abc', 'def', '']

=abc=
=def=

==
=abc=
=def=
==

=abc=
=def=

==
=abc=
=def=
==
## END

#### Regression: ${!v} should be split
v=hello
IFS=5
echo ${#v}
echo "${#v}"
## STDOUT:

5
## END

