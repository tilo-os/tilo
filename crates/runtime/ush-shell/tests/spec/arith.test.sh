# Cases from the Oils project's spec tests (spec/arith.test.sh),
# https://github.com/oils-for-unix/oils, Apache-2.0. Expectations are bash's.

#### Side Effect in Array Indexing
a=(4 5 6)
echo "${a[b=2]} b=$b"
## stdout: 6 b=2

#### Add one to var
i=1
echo $(($i+1))
## stdout: 2

#### $ is optional
i=1
echo $((i+1))
## stdout: 2

#### SimpleVarSub within arith
j=0
echo $(($j + 42))
## stdout: 42

#### BracedVarSub within ArithSub
echo $((${j:-5} + 1))
## stdout: 6

#### Arith word part
foo=1; echo $((foo+1))bar$(($foo+1))
## stdout: 2bar2

#### Arith sub with word parts
# Making 13 from two different kinds of sub.  Geez.
echo $((1 + $(echo 1)${undefined:-3}))
## stdout: 14

#### Arith sub within arith sub
# This is unnecessary but works in all shells.
echo $((1 + $((2 + 3)) + 4))
## stdout: 10

#### Backticks within arith sub
# This is unnecessary but works in all shells.
echo $((`echo 1` + 2))
## stdout: 3

#### Integer constant parsing
echo $(( 0x12A ))
echo $(( 0x0A ))
echo $(( 0777 ))
echo $(( 0010 ))
echo $(( 24#ag7 ))
## STDOUT:
298
10
511
8
6151
## END

#### Newline in the middle of expression
echo $((1
+ 2))
## stdout: 3

#### Ternary operator
a=1
b=2
echo $((a>b?5:10))
## stdout: 10

#### Preincrement
a=4
echo $((++a))
echo $a
## STDOUT:
5
5
## END

#### Postincrement
a=4
echo $((a++))
echo $a
## STDOUT:
4
5
## END

#### Increment undefined variables with nounset
set -o nounset
(( undef1++ ))
(( ++undef2 ))
echo "[$undef1][$undef2]"
## stdout-json: ""
## status: 1

#### Comma operator (borrowed from C)
a=1
b=2
echo $((a,(b+1)))
## stdout: 3

#### Augmented assignment
a=4
echo $((a+=1))
echo $a
## STDOUT:
5
5
## END

#### Comparison Ops
echo $(( 1 == 1 ))
echo $(( 1 != 1 ))
echo $(( 1 < 1 ))
echo $(( 1 <= 1 ))
echo $(( 1 > 1 ))
echo $(( 1 >= 1 ))
## STDOUT:
1
0
0
1
0
1
## END

#### Logical Ops
echo $((1 || 2))
echo $((1 && 2))
echo $((!(1 || 2)))
## STDOUT:
1
1
0
## END

#### Logical Ops Short Circuit
x=11
(( 1 || (x = 22) ))
echo $x
(( 0 || (x = 33) ))
echo $x
(( 0 && (x = 44) ))
echo $x
(( 1 && (x = 55) ))
echo $x
## STDOUT:
11
33
33
55
## END

#### Bitwise ops
echo $((1|2))
echo $((1&2))
echo $((1^2))
echo $((~(1|2)))
## STDOUT:
3
0
3
-4
## END

#### Unary minus and plus
a=1
b=3
echo $((- a + + b))
## STDOUT:
2
## END

#### No floating point
echo $((1 + 2.3))
## stdout-json: ""
## status: 1

#### Constants in base 36
echo $((36#a))-$((36#z))
## stdout: 10-35

#### Multiple digit constants with base N
echo $((10#0123)), $((16#1b))
## stdout: 123, 27

#### Dynamic base constants
base=16
echo $(( ${base}#a ))
## stdout: 10

#### Octal constant
echo $(( 011 ))
## stdout: 9

#### Dynamic octal constant
zero=0
echo $(( ${zero}11 ))
## stdout: 9

#### Dynamic hex constants
zero=0
echo $(( ${zero}xAB ))
## stdout: 171

#### Hex constant with capital X
echo $(( 0XAA ))
## stdout: 170

#### Dynamic var names - result of runtime parse/eval
foo=5
x=oo
echo $(( foo + f$x + 1 ))
## stdout: 11

#### Recursive name evaluation is a result of runtime parse/eval
foo=5
bar=foo
spam=bar
eggs=spam
echo $((foo+1)) $((bar+1)) $((spam+1)) $((eggs+1))
## stdout: 6 6 6 6

#### nounset with arithmetic
set -o nounset
x=$(( y + 5 ))
echo "should not get here: x=${x:-<unset>}"
## stdout-json: ""
## status: 1

#### 64-bit integer doesn't overflow

a=$(( 1 << 31 ))
echo $a

b=$(( a + a ))
echo $b

c=$(( b + a ))
echo $c

x=$(( 1 << 62 ))
y=$(( x - 1 ))
echo "max positive = $(( x + y ))"

#echo "overflow $(( x + x ))"
## STDOUT:
2147483648
4294967296
6442450944
max positive = 9223372036854775807
## END

#### Invalid LValue
a=9
(( (a + 2) = 3 ))
echo $a
## stdout: 9
## status: 0

#### Invalid LValue that looks like array
(( 1[2] = 3 ))
echo "status=$?"
## stdout: status=1
## status: 0

#### Invalid LValue: two sets of brackets
(( a[1][2] = 3 ))
echo "status=$?"
#   shells treat this as a NON-fatal error
## stdout: status=1
## status: 0

#### Operator Precedence
echo $(( 1 + 2*3 - 8/2 ))
## stdout: 3

#### Exponentiation with **
echo $(( 3 ** 0 ))
echo $(( 3 ** 1 ))
echo $(( 3 ** 2 ))
## STDOUT:
1
3
9
## END

#### Negative exponent
# bash explicitly disallows negative exponents!
echo $(( 2**-1 * 5 ))
## stdout-json: ""
## status: 1

#### Add integer to indexed array (a[0] decay)
declare -a array=(1 2 3)
echo $((array + 5))
## STDOUT:
6
## END
## status: 0

#### Add integer to associative array (a[0] decay)
typeset -A assoc
assoc[0]=42
echo $((assoc + 5))
## stdout: 47
## status: 0

#### Double subscript
a=(1 2 3)
echo $(( a[1] ))
echo $(( a[1][1] ))
## STDOUT:
2
## END
## status: 1

#### result of ArithSub -- array[0] decay
a=(4 5 6)
echo declared
b=$(( a ))
echo $b
## STDOUT:
declared
4
## END
## status: 0

#### result of ArithSub -- assoc[0] decay
declare -A A=(['foo']=bar ['spam']=eggs)
echo declared
b=$(( A ))
echo $b
## STDOUT:
declared
0
## END
## status: 0

#### assignment with dynamic var name
foo=bar
echo $(( x$foo = 42 ))
echo xbar=$xbar
## STDOUT:
42
xbar=42
## END

#### array assignment with dynamic array name
foo=bar
echo $(( x$foo[5] = 42 ))
echo 'xbar[5]='${xbar[5]}
## STDOUT:
42
xbar[5]=42
## END

#### unary assignment with dynamic var name
foo=bar
xbar=42
echo $(( x$foo++ ))
echo xbar=$xbar
## STDOUT:
42
xbar=43
## END

#### unary array assignment with dynamic var name
foo=bar
xbar[5]=42
echo $(( x$foo[5]++ ))
echo 'xbar[5]='${xbar[5]}
## STDOUT:
42
xbar[5]=43
## END

#### Dynamic parsing of arithmetic
e=1+2
echo $(( e + 3 ))
[[ e -eq 3 ]] && echo true
[ e -eq 3 ]
echo status=$?
## STDOUT:
6
true
status=2
## END

#### Dynamic parsing on empty string
a=''
echo $(( a ))

a2=' '
echo $(( a2 ))
## STDOUT:
0
0
## END

#### nested ternary (bug fix)
echo $((1?2?3:4:5))
## STDOUT:
3
## END

#### 1 ? a=1 : b=2 ( bug fix)
echo $((1 ? a=1 : 42 ))
echo a=$a

# this does NOT work
#echo $((1 ? a=1 : b=2 ))
## STDOUT:
1
a=1
## END

#### Negative numbers with integer division /

echo $(( 10 / 3))
echo $((-10 / 3))
echo $(( 10 / -3))
echo $((-10 / -3))

echo ---

a=20
: $(( a /= 3 ))
echo $a

a=-20
: $(( a /= 3 ))
echo $a

a=20
: $(( a /= -3 ))
echo $a

a=-20
: $(( a /= -3 ))
echo $a
## STDOUT:
3
-3
-3
3
---
6
-6
-6
6
## END

#### Negative numbers with %

echo $(( 10 % 3))
echo $((-10 % 3))
echo $(( 10 % -3))
echo $((-10 % -3))
## STDOUT:
1
-1
1
-1
## END

#### s[0] with string '12 34'

s='12 34'
echo '12 34' $(( s[0] )) $(( s[1] ))
echo status=$?
## STDOUT:
status=1
## END
## status: 0

