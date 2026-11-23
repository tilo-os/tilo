# Cases from the Oils project's spec tests (spec/if_.test.sh),
# https://github.com/oils-for-unix/oils, Apache-2.0. Expectations are bash's.

#### If
if true; then
  echo if
fi
## stdout: if

#### else
if false; then
  echo if
else
  echo else
fi
## stdout: else

#### elif
if (( 0 )); then
  echo if
elif true; then
  echo elif
else
  echo else
fi
## stdout: elif

#### Long style
if [[ 0 -eq 1 ]]
then
  echo if
  echo if
elif true
then
  echo elif
else
  echo else
  echo else
fi
## stdout: elif

#### if break corner case

# This is analogous to the 'while' case in spec/loop
f() {
  if break; then
    echo hi
  fi
}
f
## STDOUT:
hi
## END

