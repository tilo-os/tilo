# Cases from the Oils project's spec tests (spec/subshell.test.sh),
# https://github.com/oils-for-unix/oils, Apache-2.0. Expectations are bash's.

#### Subshell exit code
( false; )
echo $?
## stdout: 1
## status: 0

#### Subshell with redirects
( echo 1 ) > a.txt
( env echo 2 ) > b.txt
( env echo 3; ) > c.txt  # Sentence in LST
( echo 4; echo 5 ) > d.txt
echo status=$?
cat a.txt b.txt c.txt d.txt
## STDOUT:
status=0
1
2
3
4
5
## END

