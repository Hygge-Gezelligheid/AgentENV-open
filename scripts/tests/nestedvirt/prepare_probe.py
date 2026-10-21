# Copyright 2026 AgentENV contributors.
# SPDX-License-Identifier: Apache-2.0
# pylint: disable=missing-module-docstring
import argparse
import shutil
import subprocess
from pathlib import Path


def copy_binary(path, root):
    """Copy an executable and the shared dependencies reported by ldd."""
    binary = Path(path)
    destination = root / binary.relative_to("/")
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(binary, destination)
    result = subprocess.run(
        ["ldd", str(binary)], text=True, capture_output=True, check=True
    )
    for word in result.stdout.split():
        if word.startswith("/"):
            dependency = Path(word)
            output = root / dependency.relative_to("/")
            output.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(dependency, output)


def main():
    """Prepare QEMU and the inner Linux validation images."""
    parser = argparse.ArgumentParser()
    parser.add_argument("work", type=Path)
    parser.add_argument("--inner-kernel", type=Path, required=True,
                        help="x86 bzImage to boot inside QEMU")
    args = parser.parse_args()
    work = args.work.resolve()
    root = work / "probe-root"
    inner = work / "inner-initrd-root"
    for base in (root, inner):
        for name in ("bin", "dev", "proc", "sys", "run", "root"):
            (base / name).mkdir(parents=True, exist_ok=True)
        shutil.copy2("/bin/busybox", base / "bin/busybox")
        if not (base / "bin/sh").exists():
            (base / "bin/sh").symlink_to("busybox")
        for name in subprocess.check_output(
            ["/bin/busybox", "--list"], text=True
        ).splitlines():
            applet = base / "bin" / name
            if not applet.exists():
                applet.symlink_to("busybox")

    shutil.copy2(Path(__file__).with_name("inner-init.sh"), inner / "init")
    (inner / "init").chmod(0o755)
    with (root / "inner.cpio").open("wb") as dest:
        subprocess.run(
            ["bash", "-c", "find . -print0 | cpio --null -o --format=newc"],
            cwd=inner,
            stdout=dest,
            check=True,
        )
    shutil.copy2(args.inner_kernel.resolve(), root / "inner-bzImage")
    copy_binary("/usr/bin/qemu-system-x86_64", root)
    copy_binary("/bin/bash", root)
    for path in (Path("/usr/share/qemu"), Path("/usr/share/seabios")):
        if path.exists():
            shutil.copytree(path, root / path.relative_to("/"), dirs_exist_ok=True)
    for source, destination in [
        ("start-inner.sh", "start-inner"),
        ("stop-inner.sh", "stop-inner"),
        ("outer-init.sh", "init"),
    ]:
        shutil.copy2(Path(__file__).with_name(source), root / destination)
        (root / destination).chmod(0o755)
    disk = work / "probe-root.ext4"
    subprocess.run(["truncate", "-s", "256M", str(disk)], check=True)
    subprocess.run(["mkfs.ext4", "-q", "-F", "-d", str(root), str(disk)], check=True)
    print(disk)


if __name__ == "__main__":
    main()
