#!/usr/bin/env bash
set -euo pipefail
export PATH=/usr/bin:/usr/sbin:/bin:/sbin
echo TILO_ARM_CAPSULE_CPU_FEATURES
grep '^Features' /proc/cpuinfo | sort -u
bind_file() {
    mkdir -p "$(dirname "$2")"
    touch "$2"
    mount --bind "$1" "$2"
    mount -o remount,bind,ro "$2"
}
bind_file /inputs/tilo-launcher /usr/libexec/tilo/tilo-launcher
bind_file /inputs/tilo-windows-runtime /usr/libexec/tilo-windows-runtime
bind_file /inputs/umu_run.py /usr/libexec/umu_run.py
bind_file /inputs/umu_run.py /usr/libexec/tilo-umu-run
for name in fex-emulator.json fex-graphics-provider.json; do
    bind_file "/inputs/$name" "/usr/share/tilo/windows/$name"
done
mkdir -p /var/home
mkdir -p /usr/lib/wine-wow64 /usr/lib64/wine-wow64 /dev/dri /run/user/1000 /home/tilo
touch /dev/dri/renderD128
mount --bind /dev/null /dev/dri/renderD128
chown 1000:1000 /run/user/1000 /home/tilo
chmod 700 /run/user/1000
/usr/bin/true
/usr/libexec/tilo/tilo-launcher __sandbox-exec --ro /usr --ro /lib --ro /lib64 \
    --rw /tmp --net-restricted -- /usr/bin/true
echo TILO_ARM_CAPSULE_TRANSPORT_PASS
mode=$(cat /inputs/mode)
if [[ $mode == probe ]]; then exit 0; fi
if [[ $mode == wine-diagnostic ]]; then
    echo TILO_DIRECT_WINE_DIAGNOSTIC_NOT_CAPSULE_ACCEPTANCE
    rootfs=/usr/lib/tilo/windows-runners/x86
    mkdir -p /opt/tilo/x86 /usr/share/wine
    mount --bind "$rootfs" /opt/tilo/x86
    for resource in usr/lib/wine-wow64 usr/lib64/wine-wow64 usr/share/wine; do
        mount --bind "$rootfs/$resource" "/$resource"
    done
    prefix=/tmp/tilo-direct-wine-prefix
    mkdir -p "$prefix/drive_c" "$prefix/dosdevices"
    ln -s ../drive_c "$prefix/dosdevices/c:"
    cp "$rootfs/usr/lib64/wine-wow64/wine/x86_64-windows/cmd.exe" "$prefix/drive_c/tilo-console.exe"
    chown -R 1000:1000 "$prefix"
    setpriv --reuid=1000 --regid=1000 --clear-groups --bounding-set=-all \
        --inh-caps=-all --ambient-caps=-all \
        env -i PATH=/usr/bin:/bin HOME=/home/tilo WINEPREFIX="$prefix" WINEARCH=win64 WINEDEBUG=-all \
        timeout --signal=TERM --kill-after=10s 240s /usr/libexec/tilo-windows-runtime \
        wine64 "$prefix/drive_c/tilo-console.exe" /d /c 'echo TILO_DIRECT_WINE_PASS>C:\tilo-direct-marker.txt'
    test "$(tr -d '\r\n' < "$prefix/drive_c/tilo-direct-marker.txt")" = TILO_DIRECT_WINE_PASS
    echo TILO_DIRECT_WINE_DIAGNOSTIC_PASS
    exit 0
fi
if [[ $mode == fex-diagnostic ]]; then
    echo TILO_DIRECT_FEX_DIAGNOSTIC_NOT_CAPSULE_ACCEPTANCE
    status=0
    for guest in true getconf; do
        arguments=()
        if [[ $guest == getconf ]]; then arguments=(_NPROCESSORS_ONLN); fi
        result=0
        setpriv --reuid=1000 --regid=1000 --clear-groups --bounding-set=-all \
            --inh-caps=-all --ambient-caps=-all \
            env -i PATH=/usr/bin:/bin HOME=/home/tilo FEX_INTERPRETER_INSTALLED=0 \
            FEX_ROOTFS=/usr/lib/tilo/windows-runners/x86 FEX_SERVERSOCKETPATH=/tmp/tilo-fex-diagnostic.sock \
            timeout --signal=TERM --kill-after=10s 60s /usr/bin/FEX "/usr/bin/$guest" "${arguments[@]}" || result=$?
        echo "TILO_FEX_DIAGNOSTIC_RESULT=$guest:$result"
        if [[ $result != 0 ]]; then status=1; fi
    done
    if [[ $status == 0 ]]; then echo TILO_FEX_DIAGNOSTIC_PASS; fi
    exit "$status"
fi
printf 'TILO_ARM_CAPSULE_MAX_USER_NAMESPACES='
cat /proc/sys/user/max_user_namespaces
/inputs/compat-tests --list > /tmp/capsule-tests.list
cases=(wine64 wine32 proton)
policy=landlock
if [[ $mode == namespace-diagnostic ]]; then
    cases=(wine64)
    policy=namespace
    echo TILO_NAMESPACE_ONLY_DIAGNOSTIC_NOT_LANDLOCK_ACCEPTANCE
fi
for kind in "${cases[@]}"; do
    test_name="plan::tests::generated_arm_${kind}_${policy}_executes_windows_console"
    grep -Fx "$test_name: test" /tmp/capsule-tests.list
    case $kind in
        wine64) executable=/usr/lib/tilo/windows-runners/x86/usr/lib64/wine-wow64/wine/x86_64-windows/cmd.exe ;;
        wine32) executable=/usr/lib/tilo/windows-runners/x86/usr/lib/wine-wow64/wine/i386-windows/cmd.exe ;;
        proton) executable=/usr/lib/tilo/windows-runners/proton/UMU-Proton-10.0-4/files/lib/wine/x86_64-windows/cmd.exe ;;
    esac
    setpriv --reuid=1000 --regid=1000 --clear-groups --bounding-set=-all \
        --inh-caps=-all --ambient-caps=-all \
        env -i PATH=/usr/bin:/bin HOME=/home/tilo TMPDIR=/tmp \
        TILO_ARM_CAPSULE_CMD_EXE="$executable" \
        timeout --signal=TERM --kill-after=10s 600s /inputs/compat-tests \
        "$test_name" --exact --ignored --nocapture --test-threads=1
    echo "TILO_ARM_CAPSULE_CASE_PASS=$kind"
done
