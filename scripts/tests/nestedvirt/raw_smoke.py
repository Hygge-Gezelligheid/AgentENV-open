# Copyright 2026 AgentENV contributors.
# SPDX-License-Identifier: Apache-2.0
# pylint: disable=missing-module-docstring
import argparse
import http.client
import json
import re
import shutil
import socket
import subprocess
import time
from pathlib import Path


class UnixHTTP(http.client.HTTPConnection):
    """Use the standard HTTP client over a Unix socket."""

    def connect(self):
        """Connect to the Firecracker API socket."""
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(self.timeout)
        self.sock.connect(self.host)


class VM:
    """Manage a dedicated Firecracker instance and retain serial evidence."""

    def __init__(self, binary, directory):
        self.directory = directory
        directory.mkdir(parents=True, exist_ok=True)
        socket_directory = directory.parents[2] / ".s"
        socket_directory.mkdir(exist_ok=True)
        self.socket = socket_directory / f"{time.monotonic_ns()}.sock"
        self.log = directory / "serial.log"
        self.output = self.log.open("wb")
        self.result = ""
        self.process = subprocess.Popen(
            [str(binary), "--api-sock", str(self.socket)],
            stdin=subprocess.PIPE,
            stdout=self.output,
            stderr=subprocess.STDOUT,
        )

    def wait(self, condition, description, timeout=30):
        """Wait for a condition and include serial output on failure."""
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if condition():
                return
            if self.process.poll() is not None:
                raise RuntimeError(
                    f"Firecracker exited: {self.log.read_text(errors='replace')[-4000:]}"
                )
            time.sleep(0.1)
        raise TimeoutError(
            f"{description}: {self.log.read_text(errors='replace')[-4000:]}"
        )

    def api(self, method, path, body):
        """Call the Firecracker API and require a successful response."""
        self.wait(self.socket.exists, "API socket", 10)
        connection = UnixHTTP(str(self.socket), timeout=10)
        connection.request(
            method, path, json.dumps(body), {"Content-Type": "application/json"}
        )
        response = connection.getresponse()
        result = response.read().decode()
        connection.close()
        if response.status != 204:
            raise RuntimeError(f"{method} {path}: {response.status} {result}")

    def command(self, command, timeout=30):
        """Execute a command in the outer guest and check its exit status."""
        token = f"DONE_{time.monotonic_ns()}"
        start = self.log.stat().st_size
        self.process.stdin.write(
            f"{command}; result=$?; printf '\\n{token}:%s\\n' \"$result\"\n".encode()
        )
        self.process.stdin.flush()

        def finished():
            data = (
                self.log.read_bytes()[start:].decode(errors="replace").replace("\r", "")
            )
            match = re.search(rf"(?:^|\n){token}:(\d+)\n", data)
            if match:
                if match.group(1) != "0":
                    raise RuntimeError(f"Guest command failed: {data}")
                self.result = data
                return True
            return False

        self.wait(finished, command, timeout)
        return self.result

    def close(self):
        """Stop this instance normally and report shutdown failures."""
        if self.process.poll() is None:
            self.process.terminate()
            self.process.wait(timeout=45)
        self.output.close()

    def inner_ticks(self, path):
        """Read the latest inner guest execution counter."""
        deadline = time.monotonic() + 90
        while time.monotonic() < deadline:
            text = self.command(f"/bin/busybox sleep 1; /bin/busybox tail -n 30 {path}")
            ticks = re.findall(r"INNER_TICK=(\d+)", text)
            if ticks:
                return int(ticks[-1])
        raise TimeoutError(f"Inner guest did not execute: {text}")

    def advancing_ticks(self, path):
        """Require the execution counter to grow across successive samples."""
        first = self.inner_ticks(path)
        self.command("/bin/busybox sleep 3")
        second = self.inner_ticks(path)
        if second <= first:
            raise RuntimeError(f"Inner guest stopped advancing {first} -> {second}")
        return second

    def initialized_inner(self):
        """Require QMP to confirm KVM is enabled and the inner vCPU is stopped."""
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            text = self.command(
                "printf '%s\\n' '{\"execute\":\"qmp_capabilities\"}' "
                "'{\"execute\":\"query-kvm\",\"id\":\"kvm\"}' "
                "'{\"execute\":\"query-status\",\"id\":\"status\"}' "
                "| /bin/busybox nc -w 1 127.0.0.1 4444; true"
            )
            replies = {}
            for line in text.splitlines():
                if line.startswith('{"'):
                    reply = json.loads(line)
                    if "id" in reply:
                        replies[reply["id"]] = reply.get("return", {})
            if (
                replies.get("kvm", {}).get("enabled") is True
                and replies.get("kvm", {}).get("present") is True
                and replies.get("status", {}).get("status") == "prelaunch"
                and replies.get("status", {}).get("running") is False
            ):
                return
        raise TimeoutError(f"Inner KVM did not initialize in stopped state: {text}")


def main():
    """Run snapshot recovery tests for all three stages."""
    parser = argparse.ArgumentParser()
    parser.add_argument("work", type=Path)
    parser.add_argument("--firecracker", type=Path, required=True)
    parser.add_argument("--kernel", type=Path, required=True, help="Outer guest vmlinux ELF")
    parser.add_argument("--rounds", type=int, default=3)
    args = parser.parse_args()
    if args.rounds < 1:
        parser.error("--rounds must be positive")
    work = args.work.resolve()
    binary = args.firecracker.resolve()
    kernel = args.kernel.resolve()
    original_rootfs = work / "probe-root.ext4"
    results = []
    run_id = str(time.time_ns())
    (work / "evidence").mkdir(parents=True, exist_ok=True)
    (work / "evidence/raw-results.json").write_text("[]\n")
    for phase in ("unused", "initialized", "running"):
        for iteration in range(args.rounds):
            directory = work / "evidence" / f"raw-{run_id}-{phase}-{iteration}"
            directory.mkdir(parents=True)
            rootfs = directory / "root.ext4"
            shutil.copy2(original_rootfs, rootfs)
            vm = VM(binary, directory / "boot")
            try:
                vm.api("PUT", "/machine-config", {"vcpu_count": 2, "mem_size_mib": 512})
                vm.api(
                    "PUT",
                    "/boot-source",
                    {
                        "kernel_image_path": str(kernel),
                        "boot_args": "console=ttyS0 reboot=k panic=1 pci=off acpi=off root=/dev/vda rw init=/init no-kvmapf",
                    },
                )
                vm.api(
                    "PUT",
                    "/drives/rootfs",
                    {
                        "drive_id": "rootfs",
                        "path_on_host": str(rootfs),
                        "is_root_device": True,
                        "is_read_only": False,
                    },
                )
                vm.api("PUT", "/actions", {"action_type": "InstanceStart"})
                vm.wait(
                    lambda vm=vm: "OUTER_READY" in vm.log.read_text(errors="replace"),
                    "outer boot",
                )
                vm.command(
                    "/bin/busybox uname -r && test -c /dev/kvm && /bin/busybox grep -m1 -o vmx /proc/cpuinfo"
                )
                vm.command(
                    "test $(/bin/busybox grep -c '^processor' /proc/cpuinfo) -eq 2 && /bin/busybox grep -w no-kvmapf /proc/cmdline && /bin/busybox mkdir -p /run/nestedvirt-probe"
                )
                if phase != "unused":
                    suffix = " -S" if phase == "initialized" else ""
                    vm.command(
                        f"/start-inner{suffix} > /run/nestedvirt-probe/inner.log 2>&1 & echo $! > /run/nestedvirt-probe/inner.pid"
                    )
                    if phase == "initialized":
                        vm.initialized_inner()
                before = None
                if phase == "running":
                    before = vm.advancing_ticks("/run/nestedvirt-probe/inner.log")
                vm.command("/bin/busybox sync")
                vm.api("PATCH", "/vm", {"state": "Paused"})
                state, memory = directory / "state.bin", directory / "memory.bin"
                vm.api(
                    "PUT",
                    "/snapshot/create",
                    {
                        "snapshot_type": "Full",
                        "snapshot_path": str(state),
                        "mem_file_path": str(memory),
                    },
                )
            finally:
                vm.close()
            restored = VM(binary, directory / "restore")
            try:
                restored.api(
                    "PUT",
                    "/snapshot/load",
                    {
                        "snapshot_path": str(state),
                        "mem_backend": {
                            "backend_type": "File",
                            "backend_path": str(memory),
                        },
                        "enable_diff_snapshots": False,
                        "resume_vm": True,
                    },
                )
                restored.command(
                    "echo RESTORED; test -c /dev/kvm && /bin/busybox grep -w no-kvmapf /proc/cmdline"
                )
                if phase == "initialized":
                    restored.initialized_inner()
                    restored.command("/stop-inner /run/nestedvirt-probe/inner.pid")
                elif phase == "running":
                    first = restored.inner_ticks("/run/nestedvirt-probe/inner.log")
                    restored.command("/bin/busybox sleep 3")
                    after = restored.inner_ticks("/run/nestedvirt-probe/inner.log")
                    if after <= first or after <= before:
                        raise RuntimeError(
                            f"Inner guest stalled: {before} -> {first} -> {after}"
                        )
                    restored.command("/stop-inner /run/nestedvirt-probe/inner.pid")
                restored.command(
                    "/start-inner > /run/nestedvirt-probe/new-inner.log 2>&1 & echo $! > /run/nestedvirt-probe/new-inner.pid"
                )
                restored.advancing_ticks("/run/nestedvirt-probe/new-inner.log")
                restored.command(
                    "/stop-inner /run/nestedvirt-probe/new-inner.pid && /bin/busybox sync"
                )
            finally:
                restored.close()
            results.append(
                {
                    "run_id": run_id,
                    "phase": phase,
                    "iteration": iteration,
                    "result": "PASS",
                    "outer_vcpus": 2,
                    "outer_memory_mib": 512,
                }
            )
            (work / "evidence/raw-results.json").write_text(
                json.dumps(results, indent=2) + "\n"
            )
            print(json.dumps(results[-1]), flush=True)


if __name__ == "__main__":
    main()
