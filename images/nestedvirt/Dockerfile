# Build for linux/amd64 with images/nestedvirt as the build context.
FROM ghcr.io/linuxserver/baseimage-ubuntu:noble@sha256:2e1eef1ee5e6734f7e225fdeccaea332f3ebd23aecea4fa48658b034d95e089e AS base

FROM base AS inner
RUN apt-get -o Acquire::Retries=2 -o APT::Update::Error-Mode=any update \
    && DEBIAN_FRONTEND=noninteractive apt-get -o Acquire::Retries=2 install -y --no-install-recommends \
        busybox-static cpio fakeroot linux-image-virtual/noble-security \
    && install -m 0644 /boot/vmlinuz /inner-bzImage
COPY --chmod=755 init /initramfs/init
# fakeroot records the console and null device nodes without a privileged build.
RUN cd /initramfs && mkdir bin dev proc sys \
    && cp /bin/busybox bin/busybox && ln -s busybox bin/sh \
    && fakeroot sh -ec 'mknod dev/console c 5 1; mknod dev/null c 1 3; find . -print | cpio -o -H newc > /inner.cpio'

FROM base
RUN apt-get -o Acquire::Retries=2 -o APT::Update::Error-Mode=any update \
    && DEBIAN_FRONTEND=noninteractive apt-get -o Acquire::Retries=2 install -y --no-install-recommends \
        busybox-static cpuid qemu-system-x86 \
    && rm -rf /var/lib/apt/lists/*
COPY --from=inner /inner-bzImage /inner.cpio /
