#!/bin/sh
# Native kernel build steps shared by os/kernel/Containerfile (the release
# build), os/kernel/dev-build.sh (the incremental dev build `tilo-vm swap
# kernel` runs) and the fixture tests. Both builds run these commands in the
# same builder image with the same /work layout:
#
#   /work/kernel-config/     os/kernel/config-fragments/
#   /work/kernel-patches/    os/kernel/patches/
#   /work/upstream-pending.toml, /work/check-upstream-pending.py
#                            os/kernel/upstream-pending.toml and its check
#   /work/check-config.py    os/kernel/check-config.py
#   /work/module-signing/    signing-key.pem, signing-cert.pem, signing-cert.der
#   /work/rpmbuild/SOURCES/  the Fedora SRPM's sources (its per-arch .config)
#
# so a kernel built for a dev VM comes from the same source tree, fragments,
# patches, checks and signing path as the one the image ships.
#
# Every command is safe to re-run on a tree it already prepared or built:
# the release build runs each once on a pristine tree, the dev build re-runs
# them on a persistent one and Kbuild recompiles only what changed.
set -eu

command=${1:?expected validate-arch, check-config, stage-boot, signing-key, prepare-source, compile or stage-modules}
arch=${2:?expected native RPM architecture}
case "$arch" in
    x86_64) image=arch/x86/boot/bzImage; arch_option=CONFIG_X86_64=y ;;
    aarch64) image=arch/arm64/boot/Image; arch_option=CONFIG_ARM64=y ;;
    *) echo "unsupported kernel architecture: $arch" >&2; exit 1 ;;
esac

self=$0

positive_jobs() {
    case "$1" in ''|*[!0-9]*|0) echo "KERNEL_BUILD_JOBS must be a positive integer" >&2; exit 1 ;; esac
}

# `python3 check-config.py --expect` for each required resolved setting;
# collects the misses in $missing.
expect_options() {
    for opt in "$@"; do
        if python3 /work/check-config.py .config --expect "$opt"; then echo "  ok      $opt"
        else echo "  MISSING $opt (resolved to: $(grep -E "^(# )?${opt%%=*}[ =]" .config || echo 'absent'))"; missing="$missing $opt"; fi
    done
}

case "$command" in
    validate-arch) ;;
    check-config)
        config=${3:?expected resolved kernel configuration}
        # These remain mandatory on both architectures. Architecture-specific
        # control-flow protections are checked separately, never as substitutes.
        required="
            $arch_option
            CONFIG_MODULE_SIG=y
            CONFIG_MODULE_SIG_FORCE=y
            CONFIG_USER_NS=y
            CONFIG_SECCOMP_FILTER=y
            CONFIG_SECURITY_LANDLOCK=y
            CONFIG_SECURITY_YAMA=y
            CONFIG_SECURITY_LOCKDOWN_LSM=y
            CONFIG_STRICT_KERNEL_RWX=y
            CONFIG_STACKPROTECTOR_STRONG=y
            CONFIG_FORTIFY_SOURCE=y
            CONFIG_HARDENED_USERCOPY=y
            CONFIG_VMAP_STACK=y
            CONFIG_SLAB_FREELIST_HARDENED=y
            CONFIG_RANDOMIZE_KSTACK_OFFSET=y
            CONFIG_LIST_HARDENED=y
        "
        if [ "$arch" = x86_64 ]; then
            required="$required CONFIG_X86_USER_SHADOW_STACK=y CONFIG_X86_KERNEL_IBT=y"
        fi
        echo "----- required security options ($arch) -----"
        missing=""
        if ! grep -Fqx "# CONFIG_MODULE_SIG_ALL is not set" "$config"; then
            echo "MISSING # CONFIG_MODULE_SIG_ALL is not set" >&2
            exit 1
        fi
        for opt in $required; do
            if grep -Fqx "$opt" "$config"; then
                echo "  ok      $opt"
            else
                echo "  MISSING $opt (resolved to: $(grep -E "^(# )?${opt%%=*}[ =]" "$config" || echo absent))"
                missing="$missing $opt"
            fi
        done
        if [ -n "$missing" ]; then
            echo "kernel config lost required security options:$missing" >&2
            exit 1
        fi
        ;;
    stage-boot)
        source_dir=${3:?expected prepared kernel source directory}
        stage=${4:?expected absolute staging directory}
        kver=${5:?expected kernel release}
        case "$stage" in /*) ;; *) echo "kernel staging directory must be absolute" >&2; exit 1 ;; esac
        boot_dir="$stage/lib/modules/$kver"
        cd "$source_dir"
        test -s "$image"
        mkdir -p "$boot_dir"
        # bootc finds the boot artifacts alongside the versioned module tree.
        cp "$image" "$boot_dir/vmlinuz"
        cp System.map "$boot_dir/System.map"
        cp .config "$boot_dir/config"
        if [ "$arch" = aarch64 ]; then
            # Snapdragon laptops use board DTBs. Keep the complete vendor tree
            # for the boot pack / installer to select the matching board later.
            make dtbs
            make INSTALL_DTBS_PATH="$boot_dir/dtb" dtbs_install
            test -n "$(find "$boot_dir/dtb" -type f -name '*.dtb' -print -quit)" || {
                echo "ARM64 kernel did not install any device trees" >&2
                exit 1
            }
        fi
        ;;
    signing-key)
        # One signing identity: per build for the release (deleted before
        # publish), per cache for dev builds. Only the public certificate is
        # compiled into the kernel.
        dir=${3:?expected signing directory}
        mkdir -p -m 0700 "$dir"
        tilo-crypto generate-secure-boot-cert \
            'Tilo ephemeral kernel module signing key' \
            "$dir/signing-key.pem" \
            "$dir/signing-cert.pem"
        tilo-crypto certificate-der \
            "$dir/signing-cert.pem" \
            "$dir/signing-cert.der"
        ;;
    prepare-source)
        # The prepared source lands at /work/rpmbuild/BUILD/kernel-<VER>/linux-<VER>-<ARCH>/.
        # Apply Tilo's patches, swap upstream's libcrypto signing helpers for a
        # shim, and merge our config fragment on top of Fedora's with the
        # in-tree merge_config.sh (the canonical kconfig fragment merger), then
        # olddefconfig to resolve any newly-exposed symbols non-interactively.
        #
        # After olddefconfig, every fragment assignment is compared with the
        # resolved config. A Fedora rename or dependency change therefore fails
        # here instead of silently dropping a hardening or performance knob.
        ksrc=${3:?expected prepared kernel source directory}
        cd "$ksrc"
        # The patch set applied to this tree is recorded beside it, so a
        # persistent dev tree moves from the old set to the new one (reverse
        # the old, apply the new) and an unchanged set touches no file.
        applied=.tilo-applied-patches
        # Posted-but-unmerged series are carried as posted and only on the
        # Fedora kernel they were verified on (os/kernel/upstream-pending.toml).
        python3 /work/check-upstream-pending.py /work/upstream-pending.toml /work/kernel-patches \
            --fedora-srpm /usr/lib/tilo/kernel-buildroot-srpm.txt
        if [ -d "$applied" ] && diff -r "$applied" /work/kernel-patches >/dev/null 2>&1; then
            echo "kernel patches unchanged since the last prepare"
        else
            if [ -d "$applied" ]; then
                for patch in $(ls -r "$applied"/*.patch 2>/dev/null); do
                    git apply -R "$patch"
                done
            fi
            git apply --check /work/kernel-patches/*.patch
            git apply /work/kernel-patches/*.patch
            rm -rf "$applied"
            mkdir "$applied"
            cp /work/kernel-patches/*.patch "$applied/"
        fi
        # Do not compile upstream's libcrypto-backed sign-file/extract-cert.
        # Kbuild still asks extract-cert for our CONFIG_MODULE_SIG_KEY, so provide
        # the already-validated DER certificate through a deliberately tiny shim.
        if grep -q 'hostprogs-always-$(CONFIG_MODULE_SIG_FORMAT).*sign-file' scripts/Makefile; then
            sed -i '/hostprogs-always-$(CONFIG_MODULE_SIG_FORMAT).*sign-file/d' scripts/Makefile
        fi
        if grep -q '^hostprogs := extract-cert$' certs/Makefile; then
            sed -i '/^hostprogs := extract-cert$/d' certs/Makefile
        fi
        ! grep -q 'hostprogs-always-$(CONFIG_MODULE_SIG_FORMAT).*sign-file' scripts/Makefile
        ! grep -q '^hostprogs := extract-cert$' certs/Makefile
        printf '%s\n' '#!/bin/sh' 'set -eu' 'out=$2' \
          'case ${1:-} in' \
          '  "") : > "$out" ;;' \
          '  /work/module-signing/signing-key.pem) cp /work/module-signing/signing-cert.der "$out" ;;' \
          '  *) echo "unexpected certificate input: $1" >&2; exit 1 ;;' \
          'esac' \
          > certs/extract-cert.new
        # Rewrite only on change: a newer shim re-extracts the certificate.
        if cmp -s certs/extract-cert.new certs/extract-cert; then
            rm -f certs/extract-cert.new
        else
            mv certs/extract-cert.new certs/extract-cert
        fi
        chmod 0755 certs/extract-cert
        # `rpmbuild -bp` runs %prep which extracts + patches, but does NOT drop
        # the target .config into the source tree — Fedora's kernel spec does
        # that inside %build. Copy the right config manually:
        # `kernel-<arch>-fedora.config` is Fedora's non-debug baseline.
        candidates="kernel-${arch}-fedora.config kernel-${arch}.config"
        found=""
        for c in $candidates; do
            [ -f "/work/rpmbuild/SOURCES/$c" ] && found="/work/rpmbuild/SOURCES/$c" && break
        done
        [ -n "$found" ] || {
            echo "no matching .config in /work/rpmbuild/SOURCES/ (tried: $candidates)"
            ls -1 /work/rpmbuild/SOURCES/ | grep -F .config | head
            exit 1
        }
        cp "$found" .config
        cp .config /work/fedora.config
        cat /work/kernel-config/tilo.config \
            "/work/kernel-config/$arch.config" > /work/tilo-effective.config
        scripts/kconfig/merge_config.sh -m .config /work/tilo-effective.config
        make olddefconfig
        python3 /work/check-config.py .config --fragment /work/tilo-effective.config
        diff -u /work/fedora.config .config | head -200 > /work/config.diff || true
        echo "----- effective config diff (top 200 lines) -----"
        cat /work/config.diff
        # `merge_config.sh -m` warns about a symbol it could not place and carries
        # on, and `olddefconfig` will silently drop one whose dependencies are not
        # met.  Both are exactly how a security option goes missing without anyone
        # noticing until the running kernel is asked about it.  Assert the ones
        # that must be in the resolved .config, and fail the build if they aren't.
        echo "----- required security options -----"
        missing=""
        expect_options \
            CONFIG_MODULE_SIG=y \
            '# CONFIG_MODULE_SIG_ALL is not set' \
            CONFIG_MODULE_SIG_FORCE=y \
            '# CONFIG_MODULE_FORCE_LOAD is not set' \
            '# CONFIG_MODULE_UNLOAD is not set' \
            '# CONFIG_LIVEPATCH is not set' \
            '# CONFIG_DEBUG_FS is not set' \
            '# CONFIG_PROC_PAGE_MONITOR is not set' \
            CONFIG_USER_NS=y \
            CONFIG_SECCOMP_FILTER=y \
            CONFIG_SECURITY_LANDLOCK=y \
            CONFIG_SECURITY_YAMA=y \
            CONFIG_SECURITY_LOCKDOWN_LSM=y \
            CONFIG_SECURITY_DMESG_RESTRICT=y \
            CONFIG_BPF_UNPRIV_DEFAULT_OFF=y \
            '# CONFIG_ACPI_CUSTOM_METHOD is not set' \
            '# CONFIG_ACPI_CONFIGFS is not set' \
            '# CONFIG_EFI_CUSTOM_SSDT_OVERLAYS is not set' \
            CONFIG_PREEMPT=y \
            CONFIG_PREEMPT_DYNAMIC=y \
            CONFIG_HZ_1000=y \
            CONFIG_HZ=1000 \
            CONFIG_SCSI_VIRTIO=y \
            CONFIG_DRM_VIRTIO_GPU=y \
            CONFIG_HW_RANDOM_VIRTIO=y \
            CONFIG_IWLWIFI=m \
            CONFIG_SCHED_AUTOGROUP=y \
            CONFIG_CPU_FREQ_GOV_SCHEDUTIL=y \
            CONFIG_CPU_FREQ_DEFAULT_GOV_SCHEDUTIL=y \
            CONFIG_IRQ_TIME_ACCOUNTING=y \
            CONFIG_HIGH_RES_TIMERS=y \
            CONFIG_NO_HZ_IDLE=y \
            '# CONFIG_HZ_PERIODIC is not set' \
            '# CONFIG_NO_HZ_FULL is not set' \
            CONFIG_UCLAMP_TASK=y \
            CONFIG_UCLAMP_TASK_GROUP=y \
            CONFIG_UCLAMP_BUCKETS_COUNT=5 \
            CONFIG_ENERGY_MODEL=y \
            CONFIG_TRANSPARENT_HUGEPAGE_MADVISE=y \
            CONFIG_MODULE_COMPRESS_ZSTD=y \
            CONFIG_MODULE_COMPRESS_ALL=y \
            CONFIG_RD_ZSTD=y \
            CONFIG_CRYPTO_LZ4=y \
            CONFIG_LZ4_COMPRESS=y \
            CONFIG_ZRAM=y \
            CONFIG_ZRAM_BACKEND_LZ4=y \
            CONFIG_ZRAM_DEF_COMP_LZ4=y \
            '# CONFIG_DEBUG_INFO_NONE is not set' \
            CONFIG_DEBUG_INFO_DWARF5=y \
            CONFIG_DEBUG_INFO_BTF=y \
            CONFIG_KALLSYMS=y \
            '# CONFIG_KALLSYMS_ALL is not set' \
            CONFIG_STRICT_KERNEL_RWX=y \
            CONFIG_STACKPROTECTOR_STRONG=y \
            CONFIG_FORTIFY_SOURCE=y \
            CONFIG_HARDENED_USERCOPY=y \
            '# CONFIG_SCHED_CLASS_EXT is not set' \
            '# CONFIG_SCHED_CORE is not set' \
            '# CONFIG_SCHED_DEBUG is not set' \
            CONFIG_VMAP_STACK=y \
            CONFIG_SLAB_FREELIST_HARDENED=y \
            CONFIG_RANDOMIZE_KSTACK_OFFSET=y \
            CONFIG_LIST_HARDENED=y \
            '# CONFIG_LEGACY_PTYS is not set' \
            '# CONFIG_LEGACY_TIOCSTI is not set' \
            '# CONFIG_USB_CONFIGFS is not set' \
            '# CONFIG_USB_GADGETFS is not set' \
            '# CONFIG_USB_FUNCTIONFS is not set' \
            '# CONFIG_USB_MASS_STORAGE is not set' \
            '# CONFIG_USB_G_SERIAL is not set' \
            '# CONFIG_USB_G_PRINTER is not set' \
            '# CONFIG_USB_G_WEBCAM is not set' \
            '# CONFIG_USB_RAW_GADGET is not set' \
            '# CONFIG_USBIP_CORE is not set' \
            '# CONFIG_ATM is not set' \
            '# CONFIG_ATALK is not set' \
            '# CONFIG_CAN is not set' \
            '# CONFIG_HAMRADIO is not set' \
            '# CONFIG_6LOWPAN is not set' \
            '# CONFIG_IEEE802154 is not set' \
            '# CONFIG_IP_DCCP is not set' \
            '# CONFIG_IP_SCTP is not set' \
            '# CONFIG_LAPB is not set' \
            '# CONFIG_MCTP is not set' \
            '# CONFIG_MISDN is not set' \
            '# CONFIG_NFC is not set' \
            '# CONFIG_PHONET is not set' \
            '# CONFIG_RDS is not set' \
            '# CONFIG_TIPC is not set' \
            '# CONFIG_X25 is not set'
        if [ "$arch" = x86_64 ]; then
            expect_options CONFIG_X86_USER_SHADOW_STACK=y CONFIG_X86_INTEL_PSTATE=y \
                CONFIG_X86_AMD_PSTATE=y CONFIG_X86_AMD_PSTATE_DEFAULT_MODE=3 \
                CONFIG_X86_KERNEL_IBT=y CONFIG_LEGACY_VSYSCALL_NONE=y \
                CONFIG_RANDOMIZE_BASE=y CONFIG_RANDOMIZE_MEMORY=y \
                CONFIG_IA32_EMULATION=y '# CONFIG_X86_X32_ABI is not set' \
                '# CONFIG_USELIB is not set' \
                '# CONFIG_DEVPORT is not set' '# CONFIG_X86_MSR is not set' \
                '# CONFIG_X86_CPUID is not set' \
                '# CONFIG_USB_GADGET is not set' \
                '# CONFIG_PARPORT is not set' '# CONFIG_BLK_DEV_FD is not set' \
                '# CONFIG_PCCARD is not set' \
                CONFIG_SCHED_MC_PRIO=y
        else
            expect_options CONFIG_ARM64_PTR_AUTH=y CONFIG_ARM64_PTR_AUTH_KERNEL=y \
                CONFIG_ARM64_BTI=y CONFIG_RANDOMIZE_BASE=y \
                CONFIG_RANDOMIZE_MODULE_REGION_FULL=y
        fi
        if [ -n "$missing" ]; then echo "kernel config lost required security options:$missing"; exit 1; fi
        sh "$self" check-config "$arch" .config
        ;;
    compile)
        # vmlinuz, the in-tree modules and the out-of-tree camera loopback
        # module against the same tree. Prints the kernel release last.
        ksrc=${3:?expected prepared kernel source directory}
        jobs=${4:?expected a job count}
        extmod=${5:?expected the v4l2loopback source directory}
        positive_jobs "$jobs"
        cd "$ksrc"
        # An optional compiler launcher (sccache in CI) wraps the target and
        # host compiler for both the tree and the out-of-tree module. Bypass it
        # for system_certificates.S, whose .incbin input changes with each
        # ephemeral signing key and is not reliably part of a cache key.
        if [ -n "${TILO_KERNEL_CC_LAUNCHER:-}" ]; then
            make -j"$jobs" CC="sh /work/kernel-cc-wrapper.sh gcc" HOSTCC="sh /work/kernel-cc-wrapper.sh gcc" all
            make -C "$ksrc" M="$extmod" -j"$jobs" CC="sh /work/kernel-cc-wrapper.sh gcc" HOSTCC="sh /work/kernel-cc-wrapper.sh gcc" modules
        else
            make -j"$jobs" all
            make -C "$ksrc" M="$extmod" -j"$jobs" modules
        fi
        make -s kernelrelease
        ;;
    stage-modules)
        # Stage vmlinuz and the signed module tree for packaging:
        # <stage>/lib/modules/<kver>/{vmlinuz,System.map,config,kernel/,extra/,modules.*}.
        ksrc=${3:?expected prepared kernel source directory}
        stage=${4:?expected absolute staging directory}
        kver=${5:?expected kernel release}
        extmod=${6:?expected the v4l2loopback source directory}
        signing=${7:?expected the module signing directory}
        jobs=${8:?expected a job count}
        positive_jobs "$jobs"
        case "$stage" in /*) ;; *) echo "kernel staging directory must be absolute" >&2; exit 1 ;; esac
        rm -rf "$stage"
        mkdir -p "$stage/lib/modules/$kver"
        # bootc / ostree find the boot artifacts alongside the modules,
        # NOT under /boot: `bootc install` fails "Failed to find kernel in
        # /usr/lib/modules, /usr/lib/ostree-boot or /boot" if vmlinuz isn't
        # co-located with the module tree. /boot itself is populated at
        # deploy time from these files.
        sh "$self" stage-boot "$arch" "$ksrc" "$stage" "$kver"
        # Check both the linked kernel and the image actually staged for boot.
        # On x86 the compressed payload can be stale even when vmlinux is fresh.
        if [ "$arch" = x86_64 ]; then
            python3 /work/check-signing-identity.py "$ksrc/vmlinux" \
              "$ksrc/certs/signing_key.x509" "$signing/signing-cert.der" \
              "$stage/lib/modules/$kver/vmlinuz" "$ksrc/scripts/extract-vmlinux"
        else
            python3 /work/check-signing-identity.py "$ksrc/vmlinux" \
              "$ksrc/certs/signing_key.x509" "$signing/signing-cert.der" \
              "$stage/lib/modules/$kver/vmlinuz"
        fi
        cd "$ksrc"
        # Install and strip before signing. Automatic Kbuild signing is disabled
        # because its host utility links libcrypto.
        make -j"$jobs" INSTALL_MOD_PATH="$stage" INSTALL_MOD_STRIP=1 modules_install
        install -Dm0644 "$extmod/v4l2loopback.ko" \
          "$stage/lib/modules/$kver/extra/v4l2loopback.ko"
        strip --strip-debug "$stage/lib/modules/$kver/extra/v4l2loopback.ko"
        test "$(modinfo -F name "$stage/lib/modules/$kver/extra/v4l2loopback.ko")" = v4l2loopback
        case "$(modinfo -F vermagic "$stage/lib/modules/$kver/extra/v4l2loopback.ko")" in
          "$kver "*) ;;
          *) echo "v4l2loopback was not built for $kver" >&2; exit 1 ;;
        esac
        install -Dm0644 "$extmod/COPYING" \
          "$stage/usr/share/licenses/tilo-kernel-core/v4l2loopback-COPYING"
        # List first, then sign in parallel: signing renames files in the tree
        # that a concurrent find would pick up again.
        modules=$(mktemp)
        find "$stage/lib/modules/$kver" \( -name '*.ko' -o -name '*.ko.zst' \) -type f -print0 > "$modules"
        xargs -0 -r -P "$jobs" -n 32 sh "$self" sign-modules "$arch" "$signing" < "$modules"
        rm -f "$modules"
        # Prove the modules really are signed rather than trusting the config —
        # with MODULE_SIG_FORCE a silently unsigned module tree is a kernel that
        # boots without its drivers.  `modinfo -F sig_id` reads the signature out
        # of the installed file and understands the compressed .ko.xz / .ko.zst
        # shapes, which a raw tail-for-the-marker check would not.
        total=$(find "$stage/lib/modules/$kver" -name '*.ko*' -type f | wc -l)
        unsigned=$(find "$stage/lib/modules/$kver" -name '*.ko*' -type f -print0 \
            | xargs -0 -r -P "$jobs" -n 64 sh -c 'for m; do modinfo -F sig_id "$m" 2>/dev/null | grep -q . || echo "$m"; done' _)
        echo "modules: $total installed, $(printf '%s' "$unsigned" | grep -c . || true) unsigned"
        test "$total" -gt 0
        if [ -n "$unsigned" ]; then
            echo "unsigned modules under MODULE_SIG_FORCE:"; printf '%s\n' "$unsigned" | head -20; exit 1
        fi
        # `depmod` builds modules.dep + friends inside the staging tree so
        # `dnf install` doesn't have to run scriptlets on the live system.
        /sbin/depmod -a -b "$stage" "$kver"
        # Remove the source + build symlinks that `modules_install` creates
        # — they point outside the staging tree and rpm would fail file
        # ownership checks on them.
        rm -f "$stage/lib/modules/$kver/source" "$stage/lib/modules/$kver/build"
        ;;
    sign-modules)
        # Sign (and verify) each module file with tilo-crypto, in place;
        # compressed modules are signed inside their zstd envelope.
        signing=${3:?expected the module signing directory}
        shift 3
        for file in "$@"; do
            case "$file" in
                *.ko.zst)
                    module="${file%.zst}"
                    zstd -q -d --rm "$file" -o "$module"
                    tilo-crypto sign-module "$signing/signing-key.pem" \
                      "$signing/signing-cert.pem" "$module" "$module.signed"
                    tilo-crypto verify-module "$signing/signing-cert.pem" "$module.signed"
                    mv "$module.signed" "$module"
                    zstd -q --rm "$module" -o "$file"
                    ;;
                *.ko)
                    tilo-crypto sign-module "$signing/signing-key.pem" \
                      "$signing/signing-cert.pem" "$file" "$file.signed"
                    tilo-crypto verify-module "$signing/signing-cert.pem" "$file.signed"
                    mv "$file.signed" "$file"
                    ;;
                *) echo "not a module this build signs: $file" >&2; exit 1 ;;
            esac
        done
        ;;
    *) echo "unknown kernel build command: $command" >&2; exit 1 ;;
esac
