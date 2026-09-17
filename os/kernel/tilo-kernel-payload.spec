# Minimal hand-written spec for the Tilo kernel payload.
# Used by os/kernel/Containerfile — the standard `make binrpm-pkg`
# path pulls in Fedora's entire brp-* scriptlet chain and stalls
# indefinitely at the payload phase on this kernel size. This spec
# skips every optional processing step (no brp-compress, no
# brp-strip, no debuginfo, no docs, no manpages) and packages the
# pre-staged files at %{stage} exactly as-is.

Name:           tilo-kernel-core
Version:        %{kver}
Release:        1
Summary:        Tilo custom kernel (initramfs-less boot, tuned for interactive use)
License:        GPL-2.0
# Use the native RPM target, matching the kernel compiled by this builder.
ExclusiveArch:  x86_64 aarch64
# Satisfy Fedora's `Requires: kernel-core` in the base OS + let
# `dnf swap kernel-core tilo-kernel-core` resolve by both name and
# virtual provides.
Provides:       kernel-core = %{version}-%{release}
Provides:       kernel-modules = %{version}-%{release}
Provides:       kernel = %{version}-%{release}
# Kernel packages tag which uname -r they install as; dnf's
# ordering logic uses this.
Provides:       installonlypkg(kernel)

# Empty %prep + %build + %install — everything is already staged.
%description
Tilo custom kernel with statically linked storage/USB/paravirt drivers on the
boot path, signed modular network drivers outside it, and PREEMPT + HZ=1000 +
THP=madvise and dynamic full preemption tuning for consistent frame latency.

%install
mkdir -p %{buildroot}
# Copy the pre-staged tree verbatim into buildroot.
cp -a %{stage}/. %{buildroot}/

%files
%defattr(-,root,root,-)
/lib/modules/%{kver}
