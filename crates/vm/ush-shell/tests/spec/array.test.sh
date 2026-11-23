# Cases from the Oils project's spec tests (spec/array.test.sh),
# https://github.com/oils-for-unix/oils, Apache-2.0. Expectations are bash's.

#### nounset / set -u with empty array (bug in bash 4.3, fixed in 4.4)

# http://lists.gnu.org/archive/html/help-bash/2017-09/msg00005.html

set -o nounset
empty=()
argv.py "${empty[@]}"
echo status=$?
## STDOUT:
[]
status=0
## END

#### local array
# mksh support local variables, but not local arrays, oddly.
f() {
  local a=(1 '2 3')
  argv.py "${a[0]}"
}
f
## stdout: ['1']
## status: 0

#### Command with with word splitting in array
array=('1 2' $(echo '3 4'))
argv.py "${array[@]}"
## stdout: ['1 2', '3', '4']

#### array over multiple lines
a=(
1
'2 3'
)
argv.py "${a[@]}"
## stdout: ['1', '2 3']
## status: 0

#### array with invalid token
a=(
1
&
'2 3'
)
argv.py "${a[@]}"
## stdout-json: ""
## status: 2

#### array with empty string
empty=('')
argv.py "${empty[@]}"
## stdout: ['']

#### Retrieve index
a=(1 '2 3')
argv.py "${a[1]}"
## stdout: ['2 3']

#### Retrieve out of bounds index
a=(1 '2 3')
argv.py "${a[3]}"
## stdout: ['']

#### Negative index
a=(1 '2 3')
argv.py "${a[-1]}" "${a[-2]}" "${a[-5]}"  # last one out of bounds
## stdout: ['2 3', '1', '']

#### Negative index and sparse array
a=(0 1 2 3 4)
unset a[1]
unset a[4]
echo "${a[@]}"
echo -1 ${a[-1]}
echo -2 ${a[-2]}
echo -3 ${a[-3]}
echo -4 ${a[-4]}
echo -5 ${a[-5]}

a[-1]+=0  # append 0 on the end
echo ${a[@]}
(( a[-1] += 42 ))
echo ${a[@]}
## STDOUT:
0 2 3
-1 3
-2 2
-3
-4 0
-5
0 2 30
0 2 72
## END

#### Negative index and sparse array
a=(0 1)
unset 'a[-1]'  # remove last element
a+=(2 3)
echo ${a[0]} $((a[0]))
echo ${a[1]} $((a[1]))
echo ${a[2]} $((a[2]))
echo ${a[3]} $((a[3]))
## STDOUT:
0 0
2 2
3 3
0
## END

#### Length after unset
a=(0 1 2 3)
unset a[-1]
echo len=${#a[@]}
unset a[-1]
echo len=${#a[@]}
## STDOUT:
len=3
len=2
## END

#### Retrieve index that is a variable
a=(1 '2 3')
i=1
argv.py "${a[$i]}"
## stdout: ['2 3']

#### Retrieve index that is a variable without $
a=(1 '2 3')
i=5
argv.py "${a[i-4]}"
## stdout: ['2 3']

#### Retrieve index that is a command sub
a=(1 '2 3')
argv.py "${a[$(echo 1)]}"
## stdout: ['2 3']

#### Retrieve array indices with ${!a}
a=(1 '2 3')
argv.py "${!a[@]}"
## stdout: ['0', '1']

#### Retrieve sparse array indices with ${!a}
a=()
(( a[99]=1 ))
argv.py "${!a[@]}"
## STDOUT:
['99']
## END

#### ${!a[1]} is named ref in bash
# mksh ignores it
foo=bar
a=('1 2' foo '2 3')
argv.py "${!a[1]}"
## stdout: ['bar']
## status: 0

#### ${!a} on array

# bash gives empty string because it's like a[0]
# mksh gives the name of the variable with !.  Very weird.

a=(1 '2 3')
argv.py "${!a}"
## stdout: ['']
## status: 0

#### All elements unquoted
a=(1 '2 3')
argv.py ${a[@]}
## stdout: ['1', '2', '3']

#### All elements quoted
a=(1 '2 3')
argv.py "${a[@]}"
## stdout: ['1', '2 3']

#### $*
a=(1 '2 3')
argv.py ${a[*]}
## stdout: ['1', '2', '3']

#### "$*"
a=(1 '2 3')
argv.py "${a[*]}"
## stdout: ['1 2 3']

#### Interpolate array into array
a=(1 '2 3')
a=(0 "${a[@]}" '4 5')
argv.py "${a[@]}"
## stdout: ['0', '1', '2 3', '4 5']

#### Set element
a=(1 '2 3')
a[0]=9
argv.py "${a[@]}"
## stdout: ['9', '2 3']

#### Set element with var ref
a=(1 '2 3')
i=0
a[$i]=9
argv.py "${a[@]}"
## stdout: ['9', '2 3']

#### Set element with array ref
# This makes parsing a little more complex.  Anything can be inside [],
# including other [].
a=(1 '2 3')
i=(0 1)
a[${i[1]}]=9
argv.py "${a[@]}"
## stdout: ['1', '9']

#### Set array item to array
a=(1 2)
a[0]=(3 4)
echo "status=$?"
## stdout: status=1
## status: 0

#### Slice of array with [@]
# mksh doesn't support this syntax!  It's a bash extension.
a=(1 2 3)
argv.py "${a[@]:1:2}"
## stdout: ['2', '3']

#### Negative slice begin
# mksh doesn't support this syntax!  It's a bash extension.
# NOTE: for some reason -2) has to be in parens?  Ah that's because it
# conflicts with :-!  That's silly.  You can also add a space.
a=(1 2 3 4 5)
argv.py "${a[@]:(-4)}"
## stdout: ['2', '3', '4', '5']

#### Negative slice length
a=(1 2 3 4 5)
argv.py "${a[@]: 1: -3}"
## stdout-json: ""
## status: 1

#### Slice with arithmetic
a=(1 2 3)
i=5
argv.py "${a[@]:i-4:2}"
## stdout: ['2', '3']

#### Number of elements
a=(1 '2 3')
echo "${#a[@]}" ${#a[@]}  # bug fix: also test without quotes
## stdout: 2 2

#### Length of an element
a=(1 '2 3')
echo "${#a[1]}"
## stdout: 3

#### Iteration
a=(1 '2 3')
for v in "${a[@]}"; do
  echo $v
done
## STDOUT:
1
2 3
## END

#### glob within array yields separate elements
touch y.Y yy.Y
a=(*.Y)
argv.py "${a[@]}"
## stdout: ['y.Y', 'yy.Y']

#### declare array and then append
declare -a array
array+=(a)
array+=(b c)
argv.py "${array[@]}"
## stdout: ['a', 'b', 'c']

#### Array syntax in wrong place
ls foo=(1 2)
## stdout-json: ""
## status: 2

#### Stripping a whole array unquoted
# Problem: it joins it first.
files=('foo.c' 'sp ace.h' 'bar.c')
argv.py ${files[@]%.c}
## stdout: ['foo', 'sp', 'ace.h', 'bar']
## status: 0

#### Stripping a whole array quoted
files=('foo.c' 'sp ace.h' 'bar.c')
argv.py "${files[@]%.c}"
## stdout: ['foo', 'sp ace.h', 'bar']
## status: 0

#### ${mystr[@]} and ${mystr[*]} are no-ops
s='abc'
echo ${s[@]}
echo ${s[*]}
## STDOUT:
abc
abc
## END

#### Create a "user" array out of the argv array
set -- 'a b' 'c'
array1=('x y' 'z')
array2=("$@")
argv.py "${array1[@]}" "${array2[@]}"
## stdout: ['x y', 'z', 'a b', 'c']

#### Tilde expansion within array
HOME=/home/bob
a=(~/src ~/git)
echo "${a[@]}"
## stdout: /home/bob/src /home/bob/git

#### Brace Expansion within Array
a=(-{a,b} {c,d}-)
echo "${a[@]}"
## stdout: -a -b c- d-

#### array default
default=('1 2' '3')
argv.py "${undef[@]:-${default[@]}}"
## stdout: ['1 2', '3']

#### declare -a / local -a is empty array
declare -a myarray
argv.py "${myarray[@]}"
myarray+=('x')
argv.py "${myarray[@]}"

f() {
  local -a myarray
  argv.py "${myarray[@]}"
  myarray+=('x')
  argv.py "${myarray[@]}"
}
f
## STDOUT:
[]
['x']
[]
['x']
## END

#### Create sparse array implicitly
(( a[99]=1 ))
echo len=${#a[@]}
argv.py "${a[@]}"
echo "unset=${a[33]}"
echo len-of-unset=${#a[33]}
## STDOUT:
len=1
['1']
unset=
len-of-unset=0
## END

#### Append sparse arrays
a=()
(( a[99]=1 ))
b=()
(( b[33]=2 ))
(( b[66]=3 ))
a+=( "${b[@]}" )
argv.py "${a[@]}"
argv.py "${a[99]}" "${a[100]}" "${a[101]}"
## STDOUT:
['1', '2', '3']
['1', '2', '3']
## END

#### Slice of sparse array with [@]
# mksh doesn't support this syntax!  It's a bash extension.
(( a[33]=1 ))
(( a[66]=2 ))
(( a[99]=2 ))
argv.py "${a[@]:15:2}"
## stdout: ['1', '2']

#### Dynamic parsing of LHS a[$code]=value

declare -a array
array[x=1]='one'

code='y=2'
#code='1+2'  # doesn't work either
array[$code]='two'

argv.py "${array[@]}"
echo x=$x
echo y=$y
## STDOUT:
['one', 'two']
x=1
y=2
## END

#### Dynamic parsing of RHS ${a[$code]}
declare -a array
array=(zero one two three)

echo ${array[1+2]}

code='1+2'
echo ${array[$code]}
## STDOUT:
three
three
## END

#### test -v a[i] with arith expressions

array=(1 2 3 '')

test -v 'array[1+1]'
echo status=$?

test -v 'array[4+1]'
echo status=$?

echo
echo dbracket

[[ -v array[1+1] ]]
echo status=$?

[[ -v array[4+1] ]]
echo status=$?
## STDOUT:
status=0
status=1

dbracket
status=0
status=1
## END

#### More arith expressions in [[ -v array[expr]] ]] 

typeset -a array
array=('' nonempty)

# This feels inconsistent with the rest of bash?
zero=0

[[ -v array[zero+0] ]]
echo zero=$?

[[ -v array[zero+1] ]]
echo one=$?

[[ -v array[zero+2] ]]
echo two=$?

echo ---

i='0+0'
[[ -v array[i] ]]
echo zero=$?

i='0+1'
[[ -v array[i] ]]
echo one=$?

i='0+2'
[[ -v array[i] ]]
echo two=$?

echo ---

i='0+0'
[[ -v array[$i] ]]
echo zero=$?

i='0+1'
[[ -v array[$i] ]]
echo one=$?

i='0+2'
[[ -v array[$i] ]]
echo two=$?
## STDOUT:
zero=0
one=0
two=1
---
zero=0
one=0
two=1
---
zero=0
one=0
two=1
## END

#### Regression: Assigning with out-of-range negative index
a=()
a[-1]=1
## STDOUT:
## END
## status: 1

#### Regression: Negative index in [[ -v a[index] ]]
a[0]=x
a[5]=y
a[10]=z
[[ -v a[-1] ]] && echo 'a has -1'
[[ -v a[-2] ]] && echo 'a has -2'
[[ -v a[-5] ]] && echo 'a has -5'
[[ -v a[-6] ]] && echo 'a has -6'
[[ -v a[-10] ]] && echo 'a has -10'
[[ -v a[-11] ]] && echo 'a has -11'
## STDOUT:
a has -1
a has -6
a has -11
## END

#### Regression: Negative out-of-range index in [[ -v a[index] ]]
e=()
[[ -v e[-1] ]] && echo 'e has -1'
## stdout-json: ""
## status: 1

