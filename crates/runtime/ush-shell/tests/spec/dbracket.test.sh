# Cases from the Oils project's spec tests (spec/dbracket.test.sh),
# https://github.com/oils-for-unix/oils, Apache-2.0. Expectations are bash's.

#### [[ glob matching, [[ has no glob expansion
[[ foo.py == *.py ]] && echo true
[[ foo.p  == *.py ]] || echo false
## STDOUT:
true
false
## END

#### [[ glob matching with escapes
[[ 'foo.*' == *."*" ]] && echo true
# note that the pattern arg to fnmatch should be '*.\*'
## stdout: true

#### equality
[[ '*.py' == '*.py' ]] && echo true
[[ foo.py == '*.py' ]] || echo false
## STDOUT:
true
false
## END

#### [[ glob matching with unquoted var
pat=*.py
[[ foo.py == $pat ]] && echo true
[[ foo.p  == $pat ]] || echo false
## STDOUT:
true
false
## END

#### [[ regex matching
# mksh doesn't have this syntax of regex matching.  I guess it comes from perl?
regex='.*\.py'
[[ foo.py =~ $regex ]] && echo true
[[ foo.p  =~ $regex ]] || echo false
## STDOUT:
true
false
## END

#### [[ regex syntax error
# hm, it doesn't show any error, but it exits 2.
[[ foo.py =~ * ]] && echo true
## stdout-json: ""
## status: 2

#### [[ has no word splitting
var='one two'
[[ 'one two' == $var ]] && echo true
## stdout: true

#### [[ has quote joining
var='one two'
[[ 'one 'tw"o" == $var ]] && echo true
## stdout: true

#### [[ empty string is false
[[ 'a' ]] && echo true
[[ ''  ]] || echo false
## STDOUT:
true
false
## END

#### && chain
[[ t && t && '' ]] || echo false
## stdout: false

#### || chain
[[ '' || '' || t ]] && echo true
## stdout: true

#### [[ compound expressions
# Notes on whitespace:
# - 1 and == need space seprating them, but ! and ( don't.
# - [[ needs whitesapce after it, but ]] doesn't need whitespace before it!
[[ ''||! (1 == 2)&&(2 == 2)]] && echo true
## stdout: true

#### precedence of && and || inside [[
[[ True || '' && '' ]] && echo true
## stdout: true

#### precedence of && and || in a command context
if test True || test '' && test ''; then
  echo YES
else
  echo "NO precedence"
fi
## stdout: NO precedence

#### > on strings
# NOTE: < doesn't need space, even though == does?  That's silly.
[[ b>a ]] && echo true
[[ b<a ]] || echo false
## STDOUT:
true
false
## END

#### != on strings
# NOTE: b!=a does NOT work
[[ b != a ]] && echo true
[[ a != a ]] || echo false
## STDOUT:
true
false
## END

#### [[ compare with literal -f (compare with test-builtin.test.sh)
var=-f
[[ $var == -f ]] && echo true
[[ '-f' == $var ]] && echo true
## STDOUT:
true
true
## END

#### [[ with unquoted empty var (compare with test-builtin.test.sh)
empty=''
[[ $empty == '' ]] && echo true
## stdout: true

#### [[ at runtime doesn't work
dbracket=[[
$dbracket foo == foo ]]
## stdout-json: ""
## status: 127

#### [[ over multiple lines is OK
# Hm it seems you can't split anywhere?
[[ foo == foo
&& bar == bar
]] && echo true
## STDOUT:
true
## END
## status: 0

#### Argument that looks like a real operator
[[ -f < ]] && echo 'should be parse error'
## stdout-json: ""
## status: 2

#### User array compared to "$@" (broken unless shopt -s strict_array)
# Both are coerced to string!  It treats it more like an  UNQUOTED ${a[@]}.

a=('1 3' 5)
b=(1 2 3)
set -- 1 '3 5'
[[ "$@" = "${a[@]}" ]] && echo true
[[ "$@" = "${b[@]}" ]] || echo false
## STDOUT:
true
false
## END

#### Array coerces to string (shopt -s strict_array to disallow)
a=('1 3' 5)
[[ '1 3 5' = "${a[@]}" ]] && echo true
[[ '1 3 4' = "${a[@]}" ]] || echo false
## STDOUT:
true
false
## END

#### (( array1 == array2 )) doesn't work
a=('1 3' 5)
b=('1 3' 5)
c=('1' '3 5')
d=('1' '3 6')

# shells EXPAND a and b first
(( a == b ))
echo status=$?

(( a == c ))
echo status=$?

(( a == d ))
echo status=$?
## STDOUT:
status=1
status=1
status=1
## END
## status: 0

#### Quotes don't matter in comparison
[[ '3' = 3 ]] && echo true
[[ '3' -eq 3 ]] && echo true
## STDOUT:
true
true
## END

#### -eq does dynamic arithmetic parsing (not supported in OSH)
[[ 1+2 -eq 3 ]] && echo true
expr='1+2'
[[ $expr -eq 3 ]] && echo true  # must be dynamically parsed
## STDOUT:
true
true
## END

#### [[ '(' ]] is treated as literal
[[ '(' ]]
echo status=$?
## stdout: status=0

#### [[ '(' foo ]] is syntax error
[[ '(' foo ]]
echo status=$?
## stdout-json: ""
## status: 2

#### empty ! is treated as literal
[[ '!' ]]
echo status=$?
## stdout: status=0

#### [[ -z ]] is syntax error
[[ -z ]]
echo status=$?
## stdout-json: ""
## status: 2

#### [[ -z '>' ]]
[[ -z '>' ]] || echo false  # -z is operator
## stdout: false

#### [[ -z '>' a ]] is syntax error
[[ -z '>' -- ]]
echo status=$?
## stdout-json: ""
## status: 2

#### [[ ]] is syntax error
[[ ]]
echo status=$?
## stdout-json: ""
## status: 2

#### [[ && ]] is syntax error
[[ && ]]
echo status=$?
## stdout-json: ""
## status: 2

#### [[ a 3< b ]] doesn't work (bug regression)
[[ a 3< b ]]
echo status=$?
[[ a 3> b ]]
echo status=$?
## stdout-json: ""
## status: 2

#### tilde expansion in [[
HOME=/home/bob
[[ ~ == /home/bob ]]
echo status=$?

[[ ~ == */bob ]]
echo status=$?

[[ ~ == */z ]]
echo status=$?
## STDOUT:
status=0
status=0
status=1
## END

#### more tilde expansion
[[ ~ ]]
echo status=$?
HOME=''
[[ ~ ]]
echo status=$?
[[ -n ~ ]]
echo unary=$?

[[ ~ == ~ ]]
echo status=$?

[[ $HOME == ~ ]]
echo fnmatch=$?
[[ ~ == $HOME ]]
echo fnmatch=$?
## STDOUT:
status=0
status=1
unary=1
status=0
fnmatch=0
fnmatch=0
## END

#### special chars
[[ ^ == ^ ]]
echo caret $?
[[ '!' == ! ]]
echo bang $?
## STDOUT:
caret 0
bang 0
## END

#### negative numbers - zero, decimal, octal, hex, base N

[[ -0 -eq 0 ]]; echo zero=$?

[[ -42 -eq -42 ]]; echo decimal=$?

# note: mksh doesn't do octal conversion
[[ -0123 -eq -83 ]]; echo octal=$?

[[ -0xff -eq -255 ]]; echo hex=$?

[[ -64#a -eq -10 ]]; echo baseN=$?
## STDOUT:
zero=0
decimal=0
octal=0
hex=0
baseN=0
## END

