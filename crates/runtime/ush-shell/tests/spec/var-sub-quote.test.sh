# Cases from the Oils project's spec tests (spec/var-sub-quote.test.sh),
# https://github.com/oils-for-unix/oils, Apache-2.0. Expectations are bash's.

#### "${empty:-}"
empty=
argv.py "${empty:-}"
## stdout: ['']

#### ${empty:-}
empty=
argv.py ${empty:-}
## stdout: []

#### array with empty values
declare -a A=('' x "" '')
argv.py "${A[@]}"
## stdout: ['', 'x', '', '']

#### substitution of IFS character, quoted and unquoted
IFS=:
s=:
argv.py $s
argv.py "$s"
## STDOUT:
['']
[':']
## END

#### :-
empty=''
argv.py ${empty:-a} ${Unset:-b}
## stdout: ['a', 'b']

#### -
empty=''
argv.py ${empty-a} ${Unset-b}
# empty one is still elided!
## stdout: ['b']

#### Inner single quotes
argv.py ${Unset:-'b'}
## stdout: ['b']

#### Inner single quotes, outer double quotes
# This is the WEIRD ONE.  Single quotes appear outside.  But all shells agree!
argv.py "${Unset:-'b'}"
## stdout: ["'b'"]

#### Inner double quotes
argv.py ${Unset:-"b"}
## stdout: ['b']

#### Inner double quotes, outer double quotes
argv.py "${Unset-"b"}"
## stdout: ['b']

#### Multiple words: no quotes
argv.py ${Unset:-a b c}
## stdout: ['a', 'b', 'c']

#### Multiple words: no outer quotes, inner single quotes
argv.py ${Unset:-'a b c'}
## stdout: ['a b c']

#### Multiple words: no outer quotes, inner double quotes
argv.py ${Unset:-"a b c"}
## stdout: ['a b c']

#### Multiple words: outer double quotes, no inner quotes
argv.py "${Unset:-a b c}"
## stdout: ['a b c']

#### Multiple words: outer double quotes, inner double quotes
argv.py "${Unset:-"a b c"}"
## stdout: ['a b c']

#### Multiple words: outer double quotes, inner single quotes
argv.py "${Unset:-'a b c'}"
# WEIRD ONE.
## stdout: ["'a b c'"]

#### Mixed inner quotes
argv.py ${Unset:-"a b" c}
## stdout: ['a b', 'c']

#### Mixed inner quotes with outer quotes
argv.py "${Unset:-"a b" c}"
## stdout: ['a b c']

#### part_value tree with multiple words
argv.py ${a:-${a:-"1 2" "3 4"}5 "6 7"}
## stdout: ['1 2', '3 45', '6 7']

#### part_value tree on RHS
v=${a:-${a:-"1 2" "3 4"}5 "6 7"}
argv.py "${v}"
## stdout: ['1 2 3 45 6 7']

#### Var with multiple words: no quotes
var='a b c'
argv.py ${Unset:-$var}
## stdout: ['a', 'b', 'c']

#### Multiple words: no outer quotes, inner single quotes
var='a b c'
argv.py ${Unset:-'$var'}
## stdout: ['$var']

#### Multiple words: no outer quotes, inner double quotes
var='a b c'
argv.py ${Unset:-"$var"}
## stdout: ['a b c']

#### Multiple words: outer double quotes, no inner quotes
var='a b c'
argv.py "${Unset:-$var}"
## stdout: ['a b c']

#### Multiple words: outer double quotes, inner double quotes
var='a b c'
argv.py "${Unset:-"$var"}"
## stdout: ['a b c']

#### Multiple words: outer double quotes, inner single quotes
# WEIRD ONE.
#
# I think I should just disallow any word with single quotes inside double
# quotes.
var='a b c'
argv.py "${Unset:-'$var'}"
## stdout: ["'a b c'"]

#### No outer quotes, Multiple internal quotes
# It's like a single command word.  Parts are joined directly.
var='a b c'
argv.py ${Unset:-A$var " $var"D E F}
## stdout: ['Aa', 'b', 'c', ' a b cD', 'E', 'F']

#### Strip a string with single quotes, unquoted
foo="'a b c d'"
argv.py ${foo%d\'}
## stdout: ["'a", 'b', 'c']

#### Strip a string with single quotes, double quoted
foo="'a b c d'"
argv.py "${foo%d\'}"
## STDOUT:
["'a b c "]
## END

#### The string to strip can be single quoted, outer is unquoted
foo='a b c d'
argv.py ${foo%'c d'} ${foo%'c  d'}
## stdout: ['a', 'b', 'a', 'b', 'c', 'd']

#### Syntax error for single quote in double quote
foo="'a b c d'"
argv.py "${foo%d'}"
## stdout-json: ""
## status: 2

#### $'' allowed within VarSub arguments
# Odd behavior of bash/mksh: $'' is recognized but NOT ''!
x=abc
echo ${x%$'b'*}
echo "${x%$'b'*}"  # git-prompt.sh relies on this
## STDOUT:
a
a
## END

#### # operator with single quoted arg (dash/ash and bash/mksh disagree, reported by Crestwave)
var=a
echo -${var#'a'}-
echo -"${var#'a'}"-
var="'a'"
echo -${var#'a'}-
echo -"${var#'a'}"-
## STDOUT:
--
--
-'a'-
-'a'-
## END

#### / operator with single quoted arg (causes syntax error in regex in OSH, reported by Crestwave)
var="++--''++--''"
echo no plus or minus "${var//[+-]}"
echo no plus or minus "${var//['+-']}"
## STDOUT:
no plus or minus ''''
no plus or minus ''''
## END
## status: 0

#### single quotes work inside character classes
x='a[[[---]]]b'
echo "${x//['[]']}"
## STDOUT:
a---b
## END

#### comparison: :- operator with single quoted arg
echo ${unset:-'a'}
echo "${unset:-'a'}"
## STDOUT:
a
'a'
## END

#### Var substitution with newlines (#2492)
echo "${var-a \
b}"
echo "${var-a
b}"

echo "${var:-c \
d}"
echo "${var:-c
d}"

var=set
echo "${var:+e \
f}"
echo "${var:+e
f}"
## STDOUT:
a b
a
b
c d
c
d
e f
e
f
## END

#### Var substitution with \n in value
echo "${var-a\nb}"
echo "${var:-c\nd}"
var=val
echo "${var:+e\nf}"
## STDOUT:
a\nb
c\nd
e\nf
## END

