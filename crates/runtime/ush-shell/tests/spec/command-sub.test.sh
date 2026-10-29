# Cases from the Oils project's spec tests (spec/command-sub.test.sh),
# https://github.com/oils-for-unix/oils, Apache-2.0. Expectations are bash's.

#### case
foo=a; case $foo in [0-9]) echo number;; [a-z]) echo letter ;; esac
## stdout: letter

#### Command sub word part
# "The token shall not be delimited by the end of the substitution."
foo=FOO; echo $(echo $foo)bar$(echo $foo)
## stdout: FOObarFOO

#### Backtick
foo=FOO; echo `echo $foo`bar`echo $foo`
## stdout: FOObarFOO

#### Backtick 2
echo `echo -n l; echo -n s`
## stdout: ls

#### Making command out of command sub should work
# Works in bash and dash!
$(echo ec)$(echo ho) split builtin
## stdout: split builtin

#### Making keyword out of command sub should NOT work
$(echo f)$(echo or) i in a b c; do echo $i; done
echo status=$?
## stdout-json: ""
## status: 2

#### Command Sub word split
argv.py $(echo 'hi there') "$(echo 'hi there')"
## stdout: ['hi', 'there', 'hi there']

#### Command Sub and exit code
# A command resets the exit code, but an assignment doesn't.
echo $(echo x; exit 33)
echo $?
x=$(echo x; exit 33)
echo $?
## STDOUT:
x
0
33
## END

#### Command Sub in local sets exit code
# A command resets the exit code, but an assignment doesn't.
f() {
  echo $(echo x; exit 33)
  echo $?
  local x=$(echo x; exit 33)
  echo $?
}
f
## STDOUT:
x
0
0
## END

#### Double Quotes in Command Sub in Double Quotes
# virtualenv's bin/activate uses this.
# This is weird!  Double quotes within `` is different than double quotes
# within $()!  All shells agree.
# I think this is related to the nested backticks case!
echo "x $(echo hi)"
echo "x $(echo "hi")"
echo "x $(echo \"hi\")"
echo "x `echo hi`"
echo "x `echo "hi"`"
echo "x `echo \"hi\"`"
## STDOUT:
x hi
x hi
x "hi"
x hi
x hi
x hi
## END

#### Quoting " within ``
echo 1 `echo \"`
#echo 2 `echo \\"`
#echo 3 `echo \\\"`
#echo 4 `echo \\\\"`
## STDOUT:
1 "
## END

#### Quoting $ within ``
echo 1 `echo $`
echo 2 `echo \$`
echo 3 `echo \\$`
echo 4 `echo \\\$`
echo 5 `echo \\\\$`
## STDOUT:
1 $
2 $
3 $
4 $
5 \$
## END

#### Quoting $ within `` within double quotes
echo "1 `echo $`"
echo "2 `echo \$`"
echo "3 `echo \\$`"
echo "4 `echo \\\$`"
echo "5 `echo \\\\$`"
## STDOUT:
1 $
2 $
3 $
4 $
5 \$
## END

#### Quoting \ within ``
# You need FOUR backslashes to make a literal \.
echo [1 `echo \ `]
echo [2 `echo \\ `]
echo [3 `echo \\\\ `]
## STDOUT:
[1 ]
[2 ]
[3 \]
## END

#### Quoting \ within `` within double quotes
echo "[1 `echo \ `]"
echo "[2 `echo \\ `]"
echo "[3 `echo \\\\ `]"
## STDOUT:
[1  ]
[2  ]
[3 \]
## END

#### Quoting ( within ``
echo 1 `echo \(`
echo 2 `echo \\(`
echo 3 `echo \\ \\(`
## STDOUT:
1 (
2 (
3 (
## END

#### Quoting ( within `` within double quotes
echo "1 `echo \(`"
echo "2 `echo \\(`"
echo "3 `echo \\ \\(`"
## STDOUT:
1 (
2 (
3  (
## END

#### Quoting non-special characters within ``
echo [1 `echo \z]`
echo [2 `echo \\z]`
echo [3 `echo \\\z]`
echo [4 `echo \\\\z]`
## STDOUT:
[1 z]
[2 z]
[3 \z]
[4 \z]
## END

#### Quoting non-special characters within `` within double quotes
echo "[1 `echo \z`]"
echo "[2 `echo \\z`]"
echo "[3 `echo \\\z`]"
echo "[4 `echo \\\\z`]"
## STDOUT:
[1 z]
[2 z]
[3 \z]
[4 \z]
## END

#### Quoting double quotes within backticks
echo \"foo\"   # for comparison
echo `echo \"foo\"`
echo `echo \\"foo\\"`
## STDOUT:
"foo"
"foo"
"foo"
## END

#### More levels of double quotes in backticks

# Shells don't agree here, some of them give you form feeds!
# There are two levels of processing I don't understand.

#echo BUG
#exit

echo `echo \\\"foo\\\"` -
echo `echo \\\\"foo\\\\"` -
echo `echo \\\\\"foo\\\\\"` -
## STDOUT:
\foo\ -
\foo\ -
\"foo\" -
## END

