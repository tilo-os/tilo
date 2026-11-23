# Cases from the Oils project's spec tests (spec/loop.test.sh),
# https://github.com/oils-for-unix/oils, Apache-2.0. Expectations are bash's.

#### implicit for loop
# This is like "for i in $@".
fun() {
  for i; do
    echo $i
  done
  echo "finished=$i"
}
fun 1 2 3
## STDOUT:
1
2
3
finished=3
## END

#### for loop with invalid identifier
# should be compile time error, but runtime error is OK too
for - in a b c; do
  echo hi
done
## stdout-json: ""
## status: 1

#### the word 'in' can be the loop variable

for in in a b c; do
  echo $in
done
echo finished=$in
## STDOUT:
a
b
c
finished=c
## END

#### Tilde expansion within for loop
HOME=/home/bob
for name in ~/src ~/git; do
  echo $name
done
## STDOUT:
/home/bob/src
/home/bob/git
## END

#### Brace Expansion within Array
for i in -{a,b} {c,d}-; do
  echo $i
  done
## STDOUT:
-a
-b
c-
d-
## END

#### using loop var outside loop
fun() {
  for i in a b c; do
    echo $i
  done
  echo $i
}
fun
## STDOUT:
a
b
c
c
## END
## status: 0

#### continue
for i in a b c; do
  echo $i
  if test $i = b; then
    continue
  fi
  echo $i
done
## STDOUT:
a
a
b
c
c
## END
## status: 0

#### break
for i in a b c; do
  echo $i
  if test $i = b; then
    break
  fi
done
## STDOUT:
a
b
## END
## status: 0

#### while in while condition
# This is a consequence of the grammar
while while true; do echo cond; break; done
do
  echo body
  break
done
## STDOUT:
cond
body
## END

#### while in pipe
x=$(find spec/ | wc -l)
y=$(find spec/ | while read path; do
  echo $path
done | wc -l
)
test $x -eq $y
echo status=$?
## stdout: status=0

#### while in pipe with subshell
i=0
seq 3 | ( while read foo; do
  i=$((i+1))
  #echo $i
done
echo $i )
## stdout: 3

#### until loop
# This is just the opposite of while?  while ! cond?
until false; do
  echo hi
  break
done
## stdout: hi

#### continue at top level
if true; then
  echo one
  continue
  echo two
fi
## STDOUT:
one
two
## END
## status: 0

#### continue in subshell
for i in $(seq 2); do
  echo "> $i"
  ( if true; then continue; fi; echo "Should not print" )
  echo subshell status=$?
  echo ". $i"
done
## STDOUT:
> 1
Should not print
subshell status=0
. 1
> 2
Should not print
subshell status=0
. 2
## END

#### continue in subshell aborts with errexit
# The other shells don't let you recover from this programming error!
set -o errexit
for i in $(seq 2); do
  echo "> $i"
  ( if true; then continue; fi; echo "Should not print" )
  echo 'should fail after subshell'
  echo ". $i"
done
## STDOUT:
> 1
Should not print
should fail after subshell
. 1
> 2
Should not print
should fail after subshell
. 2
## END
## status: 0

#### break in condition of loop
while break; do
  echo x
done
echo done
## STDOUT:
done
## END

#### break in condition of nested loop
for i in 1 2 3; do
  echo i=$i
  while break; do
    echo x
  done
done
echo done
## STDOUT:
i=1
i=2
i=3
done
## END

#### return within eval
f() {
  echo one
  eval 'return'
  echo two
}
f
## STDOUT:
one
## END

#### break/continue within eval
# NOTE: This changes things
# set -e
f() {
  for i in $(seq 5); do 
    if test $i = 2; then
      eval continue
    fi
    if test $i = 4; then
      eval break
    fi
    echo $i
  done

  eval 'return'
  echo 'done'
}
f
## STDOUT:
1
3
## END

#### multi-level break with argument 

# reported in issue #1459

counterA=100
counterB=100

while test "$counterA" -gt 0
do
    counterA=$((counterA - 1))
    while test "$counterB" -gt 0
    do
        counterB=$((counterB - 1))
        if test "$counterB" = 50
        then
            break 2
        fi
    done
done

echo "$counterA"
echo "$counterB"
## STDOUT:
99
50
## END

#### multi-level continue

for i in 1 2; do
  for j in a b c; do
    if test $j = b; then
      continue
    fi
    echo $i $j
  done
done

echo ---

for i in 1 2; do
  for j in a b c; do
    if test $j = b; then
      continue 2   # MULTI-LEVEL
    fi
    echo $i $j
  done
done
## STDOUT:
1 a
1 c
2 a
2 c
---
1 a
2 a
## END

#### $b break, $c continue, $r return, $e exit

# hm would it be saner to make FATAL builtins called break/continue/etc.?
# On the other hand, this spits out errors loudly.

echo '- break'
b=break
for i in 1 2 3; do
  echo $i
  $b
done

echo '- continue'
c='continue'
for i in 1 2 3; do
  if test $i = 2; then
    $c
  fi
  echo $i
done

r='return'
f() {
  echo '- return'
  for i in 1 2 3; do
    echo $i
    if test $i = 2; then
      $r 99
    fi
  done
}
f
echo status=$?

echo '- exit'
e='exit'
$e 5
echo 'not executed'
## STDOUT:
- break
1
- continue
1
3
- return
1
2
status=99
- exit
## END
## status: 5

#### \break \continue \return \exit

echo '- break'
for i in 1 2 3; do
  echo $i
  \break
done

echo '- continue'
for i in 1 2 3; do
  if test $i = 2; then
    \continue
  fi
  echo $i
done

f() {
  echo '- return'
  for i in 1 2 3; do
    echo $i
    if test $i = 2; then
      \return 99
    fi
  done
}
f
echo status=$?

echo '- exit'
\exit 5
echo 'not executed'
## STDOUT:
- break
1
- continue
1
3
- return
1
2
status=99
- exit
## END
## status: 5

