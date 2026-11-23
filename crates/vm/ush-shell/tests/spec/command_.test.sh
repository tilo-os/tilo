# Cases from the Oils project's spec tests (spec/command_.test.sh),
# https://github.com/oils-for-unix/oils, Apache-2.0. Expectations are bash's.

#### External programs don't have _OVM in environment
# bug fix for leakage
env | grep _OVM
echo status=$?
## stdout: status=1

