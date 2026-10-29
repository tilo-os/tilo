# Cases from the Oils project's spec tests (spec/builtin-printf.test.sh),
# https://github.com/oils-for-unix/oils, Apache-2.0. Expectations are bash's.

#### printf with no args
printf
## stdout-json: ""
## status: 2

#### printf -v %s
var=foo
printf -v $var %s 'hello there'
argv.py "$foo"
## STDOUT:
['hello there']
## END

#### printf -v %q
val='"quoted" with spaces and \'

# quote 'val' and store it in foo
printf -v foo %q "$val"
# then round trip back to eval
eval "bar=$foo"

# debugging:
#echo foo="$foo"
#echo bar="$bar"
#echo val="$val"

test "$bar" = "$val" && echo OK
## STDOUT:
OK
## END

#### printf -v a[1]
a=(a b c)
printf -v 'a[1]' %s 'foo'
echo status=$?
argv.py "${a[@]}"
## STDOUT:
status=0
['a', 'foo', 'c']
## END

#### printf -v syntax error
printf -v 'a[' %s 'foo'
echo status=$?
## STDOUT:
status=2
## END

#### dynamic declare instead of %s
var=foo
declare $var='hello there'
argv.py "$foo"
## STDOUT:
['hello there']
## END

#### dynamic declare instead of %q
var=foo
val='"quoted" with spaces and \'
# I think this is bash 4.4 only.
declare $var="${val@Q}"
echo "$foo"
## STDOUT:
'"quoted" with spaces and \'
## END

#### printf with too few arguments
printf -- '-%s-%s-%s-\n' 'a b' 'x y'
## STDOUT:
-a b-x y--
## END

#### printf with too many arguments
printf -- '-%s-%s-\n' a b c d e
## STDOUT:
-a-b-
-c-d-
-e--
## END

#### printf width strings
printf '[%5s]\n' abc
printf '[%-5s]\n' abc
## STDOUT:
[  abc]
[abc  ]
## END

#### printf integer
printf '%d\n' 42
printf '%i\n' 42  # synonym
printf '%d\n' \'a # if first character is a quote, use character code
printf '%d\n' \"a # double quotes work too
printf '[%5d]\n' 42
printf '[%-5d]\n' 42
printf '[%05d]\n' 42
#printf '[%-05d]\n' 42  # the leading 0 is meaningless
#[42   ]
## STDOUT:
42
42
97
97
[   42]
[42   ]
[00042]
## END

#### printf %6.4d -- "precision" does padding for integers
printf '[%6.4d]\n' 42
printf '[%.4d]\n' 42
printf '[%6.d]\n' 42
echo --
printf '[%6.4d]\n' -42
printf '[%.4d]\n' -42
printf '[%6.d]\n' -42
## STDOUT:
[  0042]
[0042]
[    42]
--
[ -0042]
[-0042]
[   -42]
## END

#### printf %6.4x X o 
printf '[%6.4x]\n' 42
printf '[%.4x]\n' 42
printf '[%6.x]\n' 42
echo --
printf '[%6.4X]\n' 42
printf '[%.4X]\n' 42
printf '[%6.X]\n' 42
echo --
printf '[%6.4o]\n' 42
printf '[%.4o]\n' 42
printf '[%6.o]\n' 42
## STDOUT:
[  002a]
[002a]
[    2a]
--
[  002A]
[002A]
[    2A]
--
[  0052]
[0052]
[    52]
## END

#### %06d zero padding vs. %6.6d
printf '[%06d]\n' 42
printf '[%06d]\n' -42  # 6 TOTAL
echo --
printf '[%6.6d]\n' 42
printf '[%6.6d]\n' -42  # 6 + 1 for the - sign!!!
## STDOUT:
[000042]
[-00042]
--
[000042]
[-000042]
## END

#### %06x %06X %06o
printf '[%06x]\n' 42
printf '[%06X]\n' 42
printf '[%06o]\n' 42
## STDOUT:
[00002a]
[00002A]
[000052]
## END

#### %06s is no-op
printf '(%6s)\n' 42
printf '(%6s)\n' -42
printf '(%06s)\n' 42
printf '(%06s)\n' -42
echo status=$?
## STDOUT:
(    42)
(   -42)
(    42)
(   -42)
status=0
## END

#### printf %6.4s does both truncation and padding
printf '[%6s]\n' foo
printf '[%6.4s]\n' foo
printf '[%-6.4s]\n' foo
printf '[%6s]\n' spam-eggs
printf '[%6.4s]\n' spam-eggs
printf '[%-6.4s]\n' spam-eggs
## STDOUT:
[   foo]
[   foo]
[foo   ]
[spam-eggs]
[  spam]
[spam  ]
## END

#### printf %6.0s and %0.0s
printf '[%6.0s]\n' foo
printf '[%0.0s]\n' foo
## STDOUT:
[      ]
[]
## END

#### printf %6.s and %0.s
printf '[%6.s]\n' foo
printf '[%0.s]\n' foo
## STDOUT:
[      ]
[]
## END

#### printf %*.*s (width/precision from args)
printf '[%*s]\n' 9 hello
printf '[%.*s]\n' 3 hello
printf '[%*.3s]\n' 9 hello
printf '[%9.*s]\n' 3 hello
printf '[%*.*s]\n' 9 3 hello
## STDOUT:
[    hello]
[hel]
[      hel]
[      hel]
[      hel]
## END

#### unsigned / octal / hex
printf '[%u]\n' 42
printf '[%o]\n' 42
printf '[%x]\n' 42
printf '[%X]\n' 42
echo

printf '[%X]\n' \'a  # if first character is a quote, use character code
printf '[%X]\n' \'ab # extra chars ignored
## STDOUT:
[42]
[52]
[2a]
[2A]

[61]
[61]
## END

#### unsigned / octal / hex big

for big in $(( 1 << 32 )) $(( (1 << 63) - 1 )); do
  printf '[%u]\n' $big
  printf '[%o]\n' $big
  printf '[%x]\n' $big
  printf '[%X]\n' $big
  echo
done
## STDOUT:
[4294967296]
[40000000000]
[100000000]
[100000000]

[9223372036854775807]
[777777777777777777777]
[7fffffffffffffff]
[7FFFFFFFFFFFFFFF]

## END

#### empty string (osh is more strict)
printf '%d\n' ''
## STDOUT:
0
## END

#### No char after ' => zero code point

# most shells use 0 here
printf '%d\n' \'
printf '%d\n' \"
## STDOUT:
0
0
## END

#### negative numbers with unsigned / octal / hex
printf '[%u]\n' -42
echo status=$?

printf '[%o]\n' -42
echo status=$?

printf '[%x]\n' -42
echo status=$?

printf '[%X]\n' -42
echo status=$?
## STDOUT:
[18446744073709551574]
status=0
[1777777777777777777726]
status=0
[ffffffffffffffd6]
status=0
[FFFFFFFFFFFFFFD6]
status=0
## END

#### printf floating point (not required, but they all implement it)
printf '[%f]\n' 3.14159
printf '[%.2f]\n' 3.14159
printf '[%8.2f]\n' 3.14159
printf '[%-8.2f]\n' 3.14159
printf '[%-f]\n' 3.14159
printf '[%-f]\n' 3.14
## STDOUT:
[3.141590]
[3.14]
[    3.14]
[3.14    ]
[3.141590]
[3.140000]
## END

#### printf floating point with - and 0
printf '[%8.4f]\n' 3.14
printf '[%08.4f]\n' 3.14
printf '[%8.04f]\n' 3.14  # meaning less 0
printf '[%08.04f]\n' 3.14
echo ---
# these all boil down to the same thing.  The -, 8, and 4 are respected, but
# none of the 0 are.
printf '[%-8.4f]\n' 3.14
printf '[%-08.4f]\n' 3.14
printf '[%-8.04f]\n' 3.14
printf '[%-08.04f]\n' 3.14
## STDOUT:
[  3.1400]
[003.1400]
[  3.1400]
[003.1400]
---
[3.1400  ]
[3.1400  ]
[3.1400  ]
[3.1400  ]
## END

#### printf eE fF gG
printf '[%e]\n' 3.14
printf '[%E]\n' 3.14
printf '[%f]\n' 3.14
# bash is the only one that implements %F?  Is it a synonym?
#printf '[%F]\n' 3.14
printf '[%g]\n' 3.14
printf '[%G]\n' 3.14
## STDOUT:
[3.140000e+00]
[3.140000E+00]
[3.140000]
[3.14]
[3.14]
## END

#### printf backslash escapes
argv.py "$(printf 'a\tb')"
argv.py "$(printf '\xE2\x98\xA0')"
argv.py "$(printf '\044e')"
argv.py "$(printf '\0377')"  # out of range
## STDOUT:
['a\tb']
['\xe2\x98\xa0']
['$e']
['\x1f7']
## END

#### printf unicode backslash escapes
argv.py "$(printf '\u2620')"
argv.py "$(printf '\U0000065f')"
## STDOUT:
['\xe2\x98\xa0']
['\xd9\x9f']
## END

#### printf invalid backslash escape (is ignored)
printf '[\Z]\n'
## STDOUT:
[\Z]
## END

#### printf % escapes
printf '[%%]\n'
## STDOUT:
[%]
## END

#### printf %c ASCII

printf '%c\n' a
printf '%c\n' ABC
printf '%cZ\n' ABC
## STDOUT:
a
A
AZ
## END

#### printf invalid format
printf '%z' 42
echo status=$?
printf '%-z' 42
echo status=$?
## STDOUT:
status=1
status=1
## END

#### printf %q
x='a b'
printf '[%q]\n' "$x"
## STDOUT:
[a\ b]
## END

#### printf %6q (width)
# NOTE: coreutils /usr/bin/printf does NOT implement this %6q !!!
x='a b'
printf '[%6q]\n' "$x"
printf '[%1q]\n' "$x"
## STDOUT:
[  a\ b]
[a\ b]
## END

#### printf negative numbers
printf '[%d] ' -42
echo status=$?
printf '[%i] ' -42
echo status=$?

# extra LEADING space too
printf '[%d] ' ' -42'
echo status=$?
printf '[%i] ' ' -42'
echo status=$?

# extra TRAILING space too
printf '[%d] ' ' -42 '
echo status=$?
printf '[%i] ' ' -42 '
echo status=$?

# extra TRAILING chars
printf '[%d] ' ' -42z'
echo status=$?
printf '[%i] ' ' -42z'
echo status=$?

exit 0  # ok
## STDOUT:
[-42] status=0
[-42] status=0
[-42] status=0
[-42] status=0
[-42] status=1
[-42] status=1
[-42] status=1
[-42] status=1
## END

#### printf + and space flags
# I didn't know these existed -- I only knew about - and 0 !
printf '[%+d]\n' 42
printf '[%+d]\n' -42
printf '[% d]\n' 42
printf '[% d]\n' -42
## STDOUT:
[+42]
[-42]
[ 42]
[-42]
## END

#### printf # flag
# I didn't know these existed -- I only knew about - and 0 !
# Note: '#' flag for integers outputs a prefix ONLY WHEN the value is non-zero
printf '[%#o][%#o]\n' 0 42
printf '[%#x][%#x]\n' 0 42
printf '[%#X][%#X]\n' 0 42
echo ---
# Note: '#' flag for %f, %g always outputs the decimal point.
printf '[%.0f][%#.0f]\n' 3 3
# Note: In addition, '#' flag for %g does not omit zeroes in fraction
printf '[%g][%#g]\n' 3 3
## STDOUT:
[0][052]
[0][0x2a]
[0][0X2A]
---
[3][3.]
[3][3.00000]
## END

#### Runtime error for invalid integer
x=3abc
printf '%d\n' $x
echo status=$?
printf '%d\n' xyz
echo status=$?
## STDOUT:
3
status=1
0
status=1
## END

#### Regression for 'printf x y'
printf x y
printf '%s\n' z
## STDOUT:
xz
## END

#### printf %b does backslash escaping

printf '[%s]\n' '\044'  # escapes not evaluated
printf '[%b]\n' '\044'  # YES, escapes evaluated
echo

printf '[%s]\n' '\x7e'  # escapes not evaluated
printf '[%b]\n' '\x7e'  # YES, escapes evaluated
echo

# not a valid escape
printf '[%s]\n' '\A'
printf '[%b]\n' '\A'
## STDOUT:
[\044]
[$]

[\x7e]
[~]

[\A]
[\A]
## END

#### printf %b unicode escapes

printf '[%s]\n' '\u03bc'  # escapes not evaluated
printf '[%b]\n' '\u03bc'  # YES, escapes evaluated
## STDOUT:
[\u03bc]
[μ]
## END

#### printf %b respects \c early return
printf '[%b]\n' 'ab\ncd\cxy'
echo $?
## STDOUT:
[ab
cd0
## END

#### printf %b supports octal escapes, both \141 and \0141

printf 'three %b\n' '\141'  # di
printf 'four  %b\n' '\0141'
echo

# trailing 9
printf '%b\n' '\1419'
printf '%b\n' '\01419'

# Notes:
#
# - echo -e: 
#   - NO  3 digit octal  - echo -e '\141' does not work
#   - YES 4 digit octal
# - printf %b
#   - YES 3 digit octal
#   - YES 4 digit octal
# - printf string (outer)
#   - YES 3 digit octal
#   - NO  4 digit octal
# - $'' and $PS1
#   - YES 3 digit octal
#   - NO  4 digit octal
## STDOUT:
three a
four  a

a9
a9
## END

#### printf %d %X support hex 0x5 and octal 055

echo hex
printf '%d\n' 0x55
printf '%X\n' 0x55

echo hex CAPS
printf '%d\n' 0X55
printf '%X\n' 0X55

echo octal 3
printf '%d\n' 055
printf '%X\n' 055

echo octal 4
printf '%d\n' 0055
printf '%X\n' 0055

echo octal 5
printf '%d\n' 00055
printf '%X\n' 00055
## STDOUT:
hex
85
55
hex CAPS
85
55
octal 3
45
2D
octal 4
45
2D
octal 5
45
2D
## END

#### printf %d with + prefix (positive sign)

echo decimal
printf '%d\n' +42

echo octal
printf '%d\n' +077

echo hex lowercase
printf '%d\n' +0xab

echo hex uppercase
printf '%d\n' +0XAB
## STDOUT:
decimal
42
octal
63
hex lowercase
171
hex uppercase
171
## END

