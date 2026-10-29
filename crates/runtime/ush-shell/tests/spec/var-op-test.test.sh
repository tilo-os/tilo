# Cases from the Oils project's spec tests (spec/var-op-test.test.sh),
# https://github.com/oils-for-unix/oils, Apache-2.0. Expectations are bash's.

#### Lazy Evaluation of Alternative
i=0
x=x
echo ${x:-$((i++))}
echo $i
echo ${undefined:-$((i++))}
echo $i  # i is one because the alternative was only evaluated once
## STDOUT:
x
0
0
1
## END
## status: 0

#### Default value when empty
empty=''
echo ${empty:-is empty}
## stdout: is empty

#### Default value when unset
echo ${unset-is unset}
## stdout: is unset

#### Assign default with array
set -- '1 2' '3 4'
argv.py X${unset=x"$@"x}X
argv.py "$unset"
## STDOUT:
['Xx1', '2', '3', '4xX']
['x1 2 3 4x']
## END

#### Assign default value when empty
empty=''
${empty:=is empty}
echo $empty
## stdout: is empty

#### Assign default value when unset
${unset=is unset}
echo $unset
## stdout: is unset

#### ${v:+foo} Alternative value when empty
v=foo
empty=''
echo ${v:+v is not empty} ${empty:+is not empty}
## stdout: v is not empty

#### ${v+foo} Alternative value when unset
v=foo
echo ${v+v is not unset} ${unset:+is not unset}
## stdout: v is not unset

#### "${x+foo}" quoted (regression)
# Python's configure caught this
argv.py "${with_icc+set}" = set
## STDOUT:
['', '=', 'set']
## END

#### ${s+foo} and ${s:+foo} when set -u
set -u
v=v
echo v=${v:+foo}
echo v=${v+foo}
unset v
echo v=${v:+foo}
echo v=${v+foo}
## STDOUT:
v=foo
v=foo
v=
v=
## END

#### ${v-foo} and ${v:-foo} when set -u
set -u
v=v
echo v=${v:-foo}
echo v=${v-foo}
unset v
echo v=${v:-foo}
echo v=${v-foo}
## STDOUT:
v=v
v=v
v=foo
v=foo
## END

#### $@ (empty) and - and +
echo argv=${@-minus}
echo argv=${@+plus}
echo argv=${@:-minus}
echo argv=${@:+plus}
## STDOUT:
argv=minus
argv=
argv=minus
argv=
## END

#### $@ ("") and - and +
set -- ""
echo argv=${@-minus}
echo argv=${@+plus}
echo argv=${@:-minus}
echo argv=${@:+plus}
## STDOUT:
argv=
argv=plus
argv=minus
argv=
## END

#### $@ ("" "") and - and +
set -- "" ""
echo argv=${@-minus}
echo argv=${@+plus}
echo argv=${@:-minus}
echo argv=${@:+plus}
## STDOUT:
argv=
argv=plus
argv=
argv=plus
## END

#### $* ("" "") and - and + (IFS=)
set -- "" ""
IFS=
echo argv=${*-minus}
echo argv=${*+plus}
echo argv=${*:-minus}
echo argv=${*:+plus}
## STDOUT:
argv=
argv=plus
argv=
argv=plus
## END

#### "$*" ("" "") and - and + (IFS=)
set -- "" ""
IFS=
echo "argv=${*-minus}"
echo "argv=${*+plus}"
echo "argv=${*:-minus}"
echo "argv=${*:+plus}"
## STDOUT:
argv=
argv=plus
argv=minus
argv=
## END

#### Error when empty
empty=''
echo ${empty:?'is em'pty}  # test eval of error
echo should not get here
## stdout-json: ""
## status: 1

#### Error when unset
echo ${unset?is empty}
echo should not get here
## stdout-json: ""
## status: 1

#### Error when unset
v=foo
echo ${v+v is not unset} ${unset:+is not unset}
## stdout: v is not unset

#### ${var=x} dynamic scope
f() { : "${hello:=x}"; echo $hello; }
f
echo hello=$hello

f() { hello=x; }
f
echo hello=$hello
## STDOUT:
x
hello=x
hello=x
## END

#### array ${arr[0]=x}
arr=()
echo ${#arr[@]}
: ${arr[0]=x}
echo ${#arr[@]}
## STDOUT:
0
1
## END

#### "\z" as arg
echo "${undef-\$}"
echo "${undef-\(}"
echo "${undef-\z}"
echo "${undef-\"}"
echo "${undef-\`}"
echo "${undef-\\}"
## STDOUT:
$
\(
\z
"
`
\
## END

#### "\e" as arg
echo "${undef-\e}"
## STDOUT:
\e
## END

