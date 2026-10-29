# Cases from the Oils project's spec tests (spec/tilde.test.sh),
# https://github.com/oils-for-unix/oils, Apache-2.0. Expectations are bash's.

#### ~ expansion in assignment
HOME=/home/bob
a=~/src
echo $a
## stdout: /home/bob/src

#### No ~ expansion in dynamic assignment
HOME=/home/bob
binding='const=~/src'
readonly "$binding"
echo $const
## stdout: ~/src

#### tilde expansion of word after redirect
HOME=$TMP
echo hi > ~/tilde1.txt
cat $HOME/tilde1.txt | wc -c
## stdout: 3
## status: 0

#### other user
echo ~nonexistent
## stdout: ~nonexistent

#### ${undef:-~}
HOME=/home/bar
echo ${undef:-~}
echo ${HOME:+~/z}
echo "${undef:-~}"
echo ${undef:-"~"}
## STDOUT:
/home/bar
/home/bar/z
~
~
## END

#### ${x//~/~root}
HOME=/home/bar
x=~
echo ${x//~/~root}

# gah there is some expansion, what the hell
echo ${HOME//~/~root}

x=[$HOME]
echo ${x//~/~root}
## STDOUT:
/root
/root
[/root]
## END

#### x=foo:~ has tilde expansion
HOME=/home/bar
x=foo:~
echo $x
echo "$x"  # quotes don't matter, the expansion happens on assignment?
x='foo:~'
echo $x

x=foo:~,  # comma ruins it, must be /
echo $x

x=~:foo
echo $x

# no tilde expansion here
echo foo:~
## STDOUT:
foo:/home/bar
foo:/home/bar
foo:~
foo:~,
/home/bar:foo
foo:~
## END

#### tilde expansion an assignment keyword
HOME=/home/bar
f() {
  local x=foo:~
  echo $x
}
f
## STDOUT:
foo:/home/bar
## END

#### temp assignment x=~ env

HOME=/home/bar

xx=~ env | grep xx=

# Does it respect the colon rule too?
xx=~root:~:~ env | grep xx=
## STDOUT:
xx=/home/bar
xx=/root:/home/bar:/home/bar
## END

