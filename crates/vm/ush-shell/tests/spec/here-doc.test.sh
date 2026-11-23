# Cases from the Oils project's spec tests (spec/here-doc.test.sh),
# https://github.com/oils-for-unix/oils, Apache-2.0. Expectations are bash's.

#### Here string
cat <<< 'hi'
## STDOUT:
hi
## END

#### Here string with $
cat <<< $'one\ntwo\n'
## STDOUT:
one
two

## END

#### Here redirect with explicit descriptor
# A space between 0 and <<EOF causes it to pass '0' as an arg to cat.
cat 0<<EOF
one
EOF
## stdout: one

#### Here doc with var sub, command sub, arith sub
var=v
cat <<EOF
var: ${var}
command: $(echo hi)
arith: $((1+2))
EOF
## STDOUT:
var: v
command: hi
arith: 3
## END

#### Here doc in middle.  And redirects in the middle.
# This isn't specified by the POSIX grammar, but it's accepted by both dash and
# bash!
echo foo > foo.txt
echo bar > bar.txt
cat <<EOF 1>&2 foo.txt - bar.txt
here
EOF
## stdout-json: ""

#### Here doc line continuation
cat <<EOF \
; echo two
one
EOF
## STDOUT:
one
two
## END

#### Here doc with quote expansion in terminator
cat <<'EOF'"2"
one
two
EOF2
## STDOUT:
one
two
## END

#### Here doc with multiline double quoted string
cat <<EOF; echo "two
three"
one
EOF
## STDOUT:
one
two
three
## END

#### Two here docs -- first is ignored; second ones wins!
<<EOF1 cat <<EOF2
hello
EOF1
there
EOF2
## stdout: there

#### Here doc with builtin 'read'
# read can't be run in a subshell.
read v1 v2 <<EOF
val1 val2
EOF
echo =$v1= =$v2=
## stdout: =val1= =val2=

#### Compound command here doc
while read line; do
  echo X $line
done <<EOF
1
2
3
EOF
## STDOUT:
X 1
X 2
X 3
## END

#### Here doc in while condition and here doc in body
while cat <<E1 && cat <<E2; do cat <<E3; break; done
1
E1
2
E2
3
E3
## STDOUT:
1
2
3
## END

#### Here doc in while condition and here doc in body on multiple lines
while cat <<E1 && cat <<E2
1
E1
2
E2
do
  cat <<E3
3
E3
  break
done
## STDOUT:
1
2
3
## END

#### Here doc in while loop split up more
while cat <<E1
1
E1

cat <<E2
2
E2

do
  cat <<E3
3
E3
  break
done
## STDOUT:
1
2
3
## END

#### Mixing << and <<-
cat <<-EOF; echo --; cat <<EOF2
	one
EOF
two
EOF2
## STDOUT:
one
--
two
## END

#### Two compound commands with two here docs
while read line; do echo X $line; done <<EOF; echo ==;  while read line; do echo Y $line; done <<EOF2
1
2
EOF
3
4
EOF2
## STDOUT:
X 1
X 2
==
Y 3
Y 4
## END

#### Function def and execution with here doc
fun() { cat; } <<EOF; echo before; fun; echo after 
1
2
EOF
## STDOUT:
before
1
2
after
## END

#### Redirect after here doc
cat <<EOF 1>&2
out
EOF
## stdout-json: ""

#### here doc stripping tabs
cat <<-EOF
	1
	2
		3  # 2 tabs are both stripped
  4  # spaces are preserved
	EOF
## STDOUT:
1
2
3  # 2 tabs are both stripped
  4  # spaces are preserved
## END

#### Here doc within subshell with boolean
[[ $(cat <<EOF
foo
EOF
) == foo ]]; echo $?
## stdout: 0

#### Here Doc in if condition
if cat <<EOF; then
here doc in IF CONDITION
EOF
  echo THEN executed
fi
## STDOUT:
here doc in IF CONDITION
THEN executed
## END

#### Nested here docs which are indented
cat <<- EOF
	outside
	$(cat <<- INSIDE
		inside
INSIDE
)
EOF
## STDOUT:
outside
inside
## END

#### Here doc and backslash double quote
cat <<EOF
a \"quote\"
EOF
## STDOUT:
a \"quote\"
## END

#### Here doc escapes
# these are the chars from _DQ_ESCAPED_CHAR
cat <<EOF
\\ \" \$ \`
EOF
## STDOUT:
\ \" $ `
## END

