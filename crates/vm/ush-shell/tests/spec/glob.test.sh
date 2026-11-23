# Cases from the Oils project's spec tests (spec/glob.test.sh),
# https://github.com/oils-for-unix/oils, Apache-2.0. Expectations are bash's.

#### glob double quote escape
echo "*.sh"
## stdout: *.sh

#### glob single quote escape
echo "*.sh"
## stdout: *.sh

#### glob backslash escape
echo \*.sh
## stdout: *.sh

#### 0 char glob -- does NOT work
echo []bin
## STDOUT:
[]bin
## END

#### looks like glob at the start, but isn't
echo [bin
## stdout: [bin

#### looks like glob plus negation at the start, but isn't
echo [!bin
## stdout: [!bin

#### glob after var expansion
touch _tmp/a.A _tmp/aa.A _tmp/b.B
f="_tmp/*.A"
g="$f _tmp/*.B"
echo $g
## stdout: _tmp/a.A _tmp/aa.A _tmp/b.B

#### quoted var expansion with glob meta characters
touch _tmp/a.A _tmp/aa.A _tmp/b.B
f="_tmp/*.A"
echo "[ $f ]"
## stdout: [ _tmp/*.A ]

#### glob after "$@" expansion
fun() {
  echo "$@"
}
fun '_tmp/*.B'
## stdout: _tmp/*.B

#### glob after $@ expansion
touch _tmp/b.B
fun() {
  echo $@
}
fun '_tmp/*.B'
## stdout: _tmp/b.B

#### no glob after ~ expansion
HOME=*
echo ~/*.py
## stdout: */*.py

#### store literal globs in array then expand
touch _tmp/a.A _tmp/aa.A _tmp/b.B
g=("_tmp/*.A" "_tmp/*.B")
echo ${g[@]}
## stdout: _tmp/a.A _tmp/aa.A _tmp/b.B

#### glob inside array
touch _tmp/a.A _tmp/aa.A _tmp/b.B
g=(_tmp/*.A _tmp/*.B)
echo "${g[@]}"
## stdout: _tmp/a.A _tmp/aa.A _tmp/b.B

#### glob with escaped - in char class
touch _tmp/foo.-
touch _tmp/c.C
echo _tmp/*.[C-D] _tmp/*.[C\-D]
## stdout: _tmp/c.C _tmp/c.C _tmp/foo.-

#### glob with char class expression
# note: mksh doesn't support [[:punct:]] ?
touch _tmp/e.E _tmp/foo.-
echo _tmp/*.[[:punct:]E]
## stdout: _tmp/e.E _tmp/foo.-

#### glob double quotes
# note: mksh doesn't support [[:punct:]] ?
touch _tmp/\"quoted.py\"
echo _tmp/\"*.py\"
## stdout: _tmp/"quoted.py"

#### : escaped

touch _tmp/foo.-
echo _tmp/*.[[:punct:]] _tmp/*.[[:punct\:]]
## STDOUT:
_tmp/foo.- _tmp/foo.-
## END

#### Glob after var manipulation
touch _tmp/foo.zzz _tmp/bar.zzz
g='_tmp/*.zzzZ'
echo $g ${g%Z}
## stdout: _tmp/*.zzzZ _tmp/bar.zzz _tmp/foo.zzz

#### Glob after part joining
touch _tmp/foo.yyy _tmp/bar.yyy
g='_tmp/*.yy'
echo $g ${g}y
## stdout: _tmp/*.yy _tmp/bar.yyy _tmp/foo.yyy

#### Glob flags on file system
touch _tmp/-n _tmp/zzzzz
cd _tmp
echo -* hello zzzz?
## stdout-json: "hello zzzzz"

#### set -o noglob (bug #698)
var='\z'
set -f
echo $var
## STDOUT:
\z
## END

#### PatSub of unescaped [[] and []]
x='[foo]'
echo ${x//[\[z]/<}  # the right way to do it
echo ${x//[\]z]/>}
echo ${x//[[z]/<}  # also accepted
echo ${x//[]z]/>}
## STDOUT:
<foo]
[foo>
<foo]
[foo>
## END

#### PatSub of negated unescaped [[] and []]
x='[foo]'
echo ${x//[^\[z]/<}  # the right way to do it
echo ${x//[^\]z]/>}
echo ${x//[^[z]/<}  # also accepted
#echo ${x//[^]z]/>}  # only busybox ash interprets as ^\]
## STDOUT:
[<<<<
>>>>]
[<<<<
## END

#### \ in unquoted substitutions does not match a backslash
mkdir x
touch \
  x/test.ifs.\\.txt \
  x/test.ifs.\'.txt \
  x/test.ifs.a.txt \
  x/test.ifs.\\b.txt

v="*\\*.txt"
argv.py x/$v

v="*\'.txt"
argv.py x/$v

v='*\a.txt'
argv.py x/$v

v='*\b.txt'
argv.py x/$v
## STDOUT:
['x/*\\*.txt']
["x/test.ifs.'.txt"]
['x/test.ifs.a.txt']
['x/test.ifs.\\b.txt']
## END

#### \ in unquoted substitutions is preserved
v='\*\*.txt'
echo $v
echo "$v"
## STDOUT:
\*\*.txt
\*\*.txt
## END

#### \ in unquoted substitutions is preserved with set -o noglob
set -f
v='*\*.txt'
echo $v
## STDOUT:
*\*.txt
## END

#### \ in unquoted substitutions is preserved without glob matching
mkdir x
touch \
  'x/test.ifs.\.txt' \
  'x/test.ifs.*.txt'
v='*\*.txt'
argv.py x/unmatching.$v
## STDOUT:
['x/unmatching.*\\*.txt']
## END

#### \ in unquoted substitutions escapes globchars
mkdir x
touch \
  'x/test.ifs.\.txt' \
  'x/test.ifs.*.txt'

v='*\*.txt'
argv.py x/$v

v="\\" u='*.txt'
argv.py x/*$v$u

v="\\" u="*.txt"
argv.py x/*$v*.txt
## STDOUT:
['x/test.ifs.*.txt']
['x/test.ifs.*.txt']
['x/test.ifs.*.txt']
## END

#### pattern starting with . does not return . and ..

echo hi .*
## STDOUT:
hi .*
## END

