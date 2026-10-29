# Cases from the Oils project's spec tests (spec/assign.test.sh),
# https://github.com/oils-for-unix/oils, Apache-2.0. Expectations are bash's.

#### Env value doesn't persist
FOO=foo printenv.py FOO
echo -$FOO-
## STDOUT:
foo
--
## END

#### Env value with equals
FOO=foo=foo printenv.py FOO
## stdout: foo=foo

#### Env binding can use preceding bindings, but not subsequent ones
# This means that for ASSIGNMENT_WORD, on the RHS you invoke the parser again!
# Could be any kind of quoted string.
FOO="foo" BAR="[$FOO][$BAZ]" BAZ=baz printenv.py FOO BAR BAZ
## STDOUT:
foo
[foo][]
baz
## END

#### Env value with two quotes
FOO='foo'"adjacent" printenv.py FOO
## stdout: fooadjacent

#### Env value with escaped <
FOO=foo\<foo printenv.py FOO
## stdout: foo<foo

#### FOO=foo echo [foo]
FOO=foo echo "[$foo]"
## stdout: []

#### FOO=foo fun
fun() {
  echo "[$FOO]"
}
FOO=foo fun
## stdout: [foo]

#### Env binding not allowed before compound command
# bash gives exit code 2 for syntax error, because of 'do'.
# dash gives 0 because there is stuff after for?  Should really give an error.
# mksh gives acceptable error of 1.
FOO=bar for i in a b; do printenv.py $FOO; done
## stdout-json: ""
## status: 2

#### Empty env binding
EMPTY= printenv.py EMPTY
## stdout: 

#### Assignment doesn't do word splitting
words='one two'
a=$words
argv.py "$a"
## stdout: ['one two']

#### Assignment doesn't do glob expansion
touch _tmp/z.Z _tmp/zz.Z
a=_tmp/*.Z
argv.py "$a"
## stdout: ['_tmp/*.Z']

#### assignments / array assignments not interpreted after 'echo'
a=1 echo b[0]=2 c=3
## stdout: b[0]=2 c=3

#### dynamic local variables (and splitting)
f() {
  local "$1"  # Only x is assigned here
  echo x=\'$x\'
  echo a=\'$a\'

  local $1  # x and a are assigned here
  echo x=\'$x\'
  echo a=\'$a\'
}
f 'x=y a=b'
## STDOUT:
x='y a=b'
a=''
x='y'
a='b'
## END

#### readonly x= gives empty string (regression)
readonly x=
argv.py "$x"
## STDOUT:
['']
## END

#### 'local x' does not set variable
set -o nounset
f() {
  local x
  echo $x
}
f
## stdout-json: ""
## status: 1

#### 'local -a x' does not set variable
set -o nounset
f() {
  local -a x
  echo $x
}
f
## stdout-json: ""
## status: 1

#### 'local x' and then array assignment
f() {
  local x
  x[3]=foo
  echo ${x[3]}
}
f
## stdout: foo
## status: 0

#### 'declare -A' and then dict assignment
declare -A foo
key=bar
foo["$key"]=value
echo ${foo["bar"]}
## stdout: value
## status: 0

#### declare in an if statement
# bug caught by my feature detection snippet in bash-completion
if ! foo=bar; then
  echo BAD
fi
echo $foo
if ! eval 'spam=eggs'; then
  echo BAD
fi
echo $spam
## STDOUT:
bar
eggs
## END

#### Modify a temporary binding
# (regression for bug found by Michael Greenberg)
f() {
  echo "x before = $x"
  x=$((x+1))
  echo "x after  = $x"
}
x=5 f
## STDOUT:
x before = 5
x after  = 6
## END

#### Using ${x-default} after unsetting local shadowing a global
f() {
  echo "x=$x"
  local x='local'
  echo "x=$x"
  unset x
  echo "- operator = ${x-default}"
  echo ":- operator = ${x:-default}"
}
x=global
f
## STDOUT:
x=global
x=local
- operator = default
:- operator = default
## END

#### static assignment doesn't split
words='a b c'
export ex=$words
glo=$words
readonly ro=$words
argv.py "$ex" "$glo" "$ro"
## STDOUT:
['a b c', 'a b c', 'a b c']
## END

#### aliased assignment doesn't split
shopt -s expand_aliases || true
words='a b c'
alias e=export
alias r=readonly
e ex=$words
r ro=$words
argv.py "$ex" "$ro"
## STDOUT:
['a b c', 'a b c']
## END

#### assignment using dynamic var names doesn't split
words='a b c'
arg_ex=ex=$words
arg_ro=ro=$words

# no quotes, this is split of course
export $arg_ex
readonly $arg_ro

argv.py "$ex" "$ro"

arg_ex2=ex2=$words
arg_ro2=ro2=$words

# quotes, no splitting
export "$arg_ex2"
readonly "$arg_ro2"

argv.py "$ex2" "$ro2"
## STDOUT:
['a', 'a']
['a b c', 'a b c']
## END

#### assign and glob
cd $TMP
touch foo=a foo=b
foo=*
argv.py "$foo"
unset foo

export foo=*
argv.py "$foo"
unset foo
## STDOUT:
['*']
['*']
## END

#### declare and glob
cd $TMP
touch foo=a foo=b
typeset foo=*
argv.py "$foo"
unset foo
## STDOUT:
['*']
## END

#### readonly a=(1 2) no_value c=(3 4) makes 'no_value' readonly
readonly a=(1 2) no_value c=(3 4)
no_value=x
## stdout-json: ""
## status: 1

#### export a=1 no_value c=2
no_value=foo
export a=1 no_value c=2
printenv.py no_value
## STDOUT:
foo
## END

#### local a=loc $var c=loc
var='b'
b=global
echo $b
f() {
  local a=loc $var c=loc
  argv.py "$a" "$b" "$c"
}
f
## STDOUT:
global
['loc', '', 'loc']
## END

