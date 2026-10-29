# Cases from the Oils project's spec tests (spec/case_.test.sh),
# https://github.com/oils-for-unix/oils, Apache-2.0. Expectations are bash's.

#### Case statement
case a in
  a) echo A ;;
  *) echo star ;;
esac

for x in a b; do
  case $x in
    # the pattern is DYNAMIC and evaluated on every iteration
    $x) echo loop ;;
    *) echo star ;;
  esac
done
## STDOUT:
A
loop
loop
## END

#### Case statement with ;;&
# ;;& keeps testing conditions
# NOTE: ;& and ;;& are bash 4 only, not on Mac
case a in
  a) echo A ;;&
  *) echo star ;;&
  *) echo star2 ;;
esac
## STDOUT:
A
star
star2
## END
## status: 0

#### Case statement with ;&
# ;& ignores the next condition.  Why would that be useful?

for x in aa bb cc dd zz; do
  case $x in
    aa) echo aa ;&
    bb) echo bb ;&
    cc) echo cc ;;
    dd) echo dd ;;
  esac
  echo --
done
## STDOUT:
aa
bb
cc
--
bb
cc
--
cc
--
dd
--
--
## END
## status: 0

#### Case with empty condition
case $empty in
  ''|foo) echo match ;;
  *) echo no ;;
esac
## stdout: match

#### Match a literal with a glob character
x='*.py'
case "$x" in
  '*.py') echo match ;;
esac
## stdout: match

#### Match a literal with a glob character with a dynamic pattern
x='b.py'
pat='[ab].py'
case "$x" in
  $pat) echo match ;;
esac
## stdout: match

#### Quoted literal in glob pattern
x='[ab].py'
pat='[ab].py'
case "$x" in
  "$pat") echo match ;;
esac
## stdout: match

#### Multiple Patterns Match
x=foo
result='-'
case "$x" in
  f*|*o) result="$result X"
esac
echo $result
## stdout: - X

#### case \n bug regression

case
in esac
## STDOUT:
## END
## status: 2

