#!/bin/sh
# Native kernel build contracts shared by the Containerfile and fixture tests.
set -eu

command=${1:?expected validate-arch, check-config, or stage-boot}
arch=${2:?expected native RPM architecture}
case "$arch" in
    x86_64) image=arch/x86/boot/bzImage; arch_option=CONFIG_X86_64=y ;;
    aarch64) image=arch/arm64/boot/Image; arch_option=CONFIG_ARM64=y ;;
    *) echo "unsupported kernel architecture: $arch" >&2; exit 1 ;;
esac

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
    *) echo "unknown kernel build command: $command" >&2; exit 1 ;;
esac
