# Cases from the Oils project's spec tests (spec/builtin-echo.test.sh),
# https://github.com/oils-for-unix/oils, Apache-2.0. Expectations are bash's.

#### echo dashes
echo -
echo --
echo ---
## STDOUT:
-
--
---
## END

#### echo backslashes
echo \\
echo '\'
echo '\\'
echo "\\"
## STDOUT:
\
\
\\
\
## END

#### echo -e backslashes
echo -e \\
echo -e '\'
echo -e '\\'
echo -e "\\"
echo

# backslash at end of line
echo -e '\
line2'
## STDOUT:
\
\
\
\

\
line2
## END

#### echo builtin should disallow typed args - literal
echo (42)
## STDOUT:
## END
## status: 2

#### echo builtin should disallow typed args - variable
var x = 43
echo (x)
## STDOUT:
## END
## status: 2

#### echo -en
echo -en 'abc\ndef\n'
## STDOUT:
abc
def
## END

#### echo -ez (invalid flag)
# bash differs from the other three shells, but its behavior is possibly more
# sensible, if you're going to ignore the error.  It doesn't make sense for
# the 'e' to mean 2 different things simultaneously: flag and literal to be
# printed.
echo -ez 'abc\n'
## STDOUT:
-ez abc\n
## END

#### echo -e with whitespace C escapes
echo -e '\n\r\t\v'
## stdout-json: "\n\r\t\u000b\n"

#### \0
echo -e 'ab\0cd'
## stdout-json: "ab\u0000cd\n"

#### echo -e with hex escape
echo -e 'abcd\x65f'
## STDOUT:
abcdef
## END

#### echo to redirected directory is an error
mkdir -p dir

echo foo > ./dir
echo status=$?
printf foo > ./dir
echo status=$?
## STDOUT:
status=1
status=1
## END

