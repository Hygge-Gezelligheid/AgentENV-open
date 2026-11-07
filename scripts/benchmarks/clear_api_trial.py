#!/usr/bin/env python3
"""Record one isolated API snapshot chain; compare independent paired trials."""
import argparse
import datetime
import ctypes
import mmap
import json
import os
from pathlib import Path
import re
import time
import urllib.error
import urllib.request

from guest_process_rpc import run as guest_run


def tree_space(root, extra=()):
    seen = set()
    logical = allocated = files = 0
    for directory, _, names in (item for path in (root, *extra) for item in os.walk(path)):
        for name in names:
            path = Path(directory) / name
            try:
                stat = path.lstat()
            except FileNotFoundError:
                continue
            key = (stat.st_dev, stat.st_ino)
            if key in seen:
                continue
            seen.add(key)
            files += 1
            logical += stat.st_size
            allocated += stat.st_blocks * 512
    return dict(files=files, logical_bytes=logical, allocated_bytes=allocated)



def referenced_layer_space(repository):
    digests = set()
    def visit(value):
        if isinstance(value, dict):
            digest = value.get('digest', '')
            if isinstance(digest, str) and digest.startswith('sha256:'):
                digests.add(digest)
            for child in value.values():
                visit(child)
        elif isinstance(value, list):
            for child in value:
                visit(child)
    for path in (repository/'catalog/records').glob('*.json'):
        visit(json.loads(path.read_text())['committed'])
    stats = [(repository/'managed-layers'/(digest.replace(':', '_')+'.overlaybd.commit')).stat()
        for digest in digests]
    return dict(files=len(stats), logical_bytes=sum(stat.st_size for stat in stats),
        allocated_bytes=sum(stat.st_blocks * 512 for stat in stats))


def pause_window(path):
    pause = None
    windows = []
    for line in path.read_text().splitlines():
        match = re.search(r"^(\S+).*'(pause|resume) vm' VMM action took (\d+) us", line)
        if not match:
            continue
        stamp = datetime.datetime.fromisoformat(match[1].replace('Z', '+00:00')).timestamp()
        duration = int(match[3]) / 1e6
        if match[2] == 'pause':
            pause = (stamp, duration)
        elif pause:
            windows.append(dict(
                paused_lower_ms=max(0, stamp - duration - pause[0]) * 1000,
                paused_upper_ms=(stamp - pause[0] + pause[1]) * 1000))
            pause = None
    if not windows:
        raise RuntimeError('Missing paired Firecracker pause/resume log events')
    return windows[-1]


def memory_residency(paths):
    library = ctypes.CDLL(None, use_errno=True)
    library.mincore.argtypes = (ctypes.c_void_p, ctypes.c_size_t, ctypes.POINTER(ctypes.c_ubyte))
    library.mincore.restype = ctypes.c_int
    resident = total = 0
    for path in paths:
        with path.open('rb') as source:
            size = os.fstat(source.fileno()).st_size
            if not size:
                continue
            pages = (size + mmap.PAGESIZE - 1) // mmap.PAGESIZE
            with mmap.mmap(source.fileno(), size, access=mmap.ACCESS_COPY) as mapping:
                vector = (ctypes.c_ubyte * pages)()
                address = ctypes.addressof(ctypes.c_char.from_buffer(mapping))
                if library.mincore(address, size, vector) != 0:
                    raise OSError(ctypes.get_errno(), 'mincore failed')
                resident += sum(value & 1 for value in vector)
                total += pages
    return dict(resident_pages=resident, total_pages=total)


def evict_snapshot_data(repository, state, memory_paths):
    before = memory_residency(memory_paths)
    seen = set()
    for root in (repository/'managed-layers', repository/'snapshots',
                 state/'snapshot-local-cache', state/'image-cache/commits'):
        for directory, _, files in os.walk(root):
            for name in files:
                path = Path(directory)/name
                stat = path.lstat()
                if not path.is_file() or path.is_symlink() or (stat.st_dev, stat.st_ino) in seen:
                    continue
                seen.add((stat.st_dev, stat.st_ino))
                with path.open('rb') as source:
                    os.fsync(source.fileno())
                    os.posix_fadvise(source.fileno(), 0, 0, os.POSIX_FADV_DONTNEED)
    after = memory_residency(memory_paths)
    if after['resident_pages'] > after['total_pages'] * 0.05:
        raise RuntimeError(f'Snapshot memory files remained resident after targeted eviction: {after}')
    return dict(before=before, after=after)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--url', required=True)
    parser.add_argument('--key-file', type=Path, required=True)
    parser.add_argument('--template', required=True)
    parser.add_argument('--state-root', type=Path, required=True)
    parser.add_argument('--repository-root', type=Path, required=True)
    parser.add_argument('--runtime-root', type=Path, required=True)
    parser.add_argument('--serial-dir', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--variant', choices=('baseline', 'candidate'), required=True)
    parser.add_argument('--pair', type=int, required=True)
    parser.add_argument('--memory-mib', type=int, default=512)
    parser.add_argument('--generations', type=int, default=8)
    parser.add_argument('--scenario', choices=('idle', 'rotating', 'hotspot', 'dense'), required=True)
    args = parser.parse_args()
    key = args.key_file.read_text().strip()
    live = set()
    snapshots = []

    def api(method, path, data=None, status=200):
        body = None if data is None else json.dumps(data).encode()
        request = urllib.request.Request(args.url + path, data=body, method=method,
            headers={'Content-Type': 'application/json', 'X-API-Key': key})
        try:
            with urllib.request.urlopen(request, timeout=300) as response:
                body = response.read()
                if response.status != status:
                    raise RuntimeError(f'Expected HTTP {status}, received {response.status}')
                return json.loads(body) if body else None
        except urllib.error.HTTPError as error:
            raise RuntimeError(f'HTTP {error.code}: ' + error.read().decode().replace(key, '[redacted]')) from error

    def guest(sandbox, command):
        return guest_run(args.url + '/proxy', command, {
            'X-API-Key': key, 'x-agentenv-sandbox-id': sandbox,
            'x-agentenv-target-port': '49983'}).strip()

    def create(template):
        sandbox = api('POST', '/sandboxes', dict(templateID=template, timeout=3600,
            autoPause=False, secure=False), 201)['sandboxID']
        live.add(sandbox)
        return sandbox

    def delete(sandbox):
        api('DELETE', '/sandboxes/' + sandbox, status=204)
        live.remove(sandbox)

    args.output.parent.mkdir(parents=True, exist_ok=True)
    with args.output.open('x') as output:
        def record(row):
            row.update(variant=args.variant, pair=args.pair, memory_mib=args.memory_mib,
                scenario=args.scenario, generations=args.generations)
            output.write(json.dumps(row, sort_keys=True) + '\n')
            output.flush()
        try:
            initial = tree_space(args.state_root, (args.runtime_root, args.serial_dir))
            input_referenced_layers = referenced_layer_space(args.repository_root)
            source = create(args.template)
            # A bounded tmpfs payload makes writes guest-memory updates; the
            # mounted volume and workload are identical in each paired run.
            payload = (args.memory_mib * 1024 * 1024 // 2) // 4096 * 4096
            chunk = max(4096, (args.memory_mib * 1024 * 1024 // 100) // 4096 * 4096)
            guest(source, f'mkdir -p /mnt/clear-bench && mount -t tmpfs -o size={payload + 1048576} '
                f'tmpfs /mnt/clear-bench && truncate -s {payload} /mnt/clear-bench/payload')
            for generation in range(args.generations):
                if args.scenario != 'idle':
                    length = payload if args.scenario == 'dense' else chunk
                    offset = (generation * chunk) % (payload - length + 1) if args.scenario == 'rotating' else 0
                    offset = offset // 4096 * 4096
                    byte = generation % 251 + 1
                    guest(source, f"head -c {length} /dev/zero | tr '\\000' '\\{byte:03o}' | "
                        f'dd of=/mnt/clear-bench/payload bs=4096 seek={offset // 4096} conv=notrunc status=none')
                expected = guest(source, 'sha256sum /mnt/clear-bench/payload').split()[0]
                started = time.perf_counter_ns()
                snapshot = api('POST', f'/sandboxes/{source}/snapshots', {}, 201)['snapshotID']
                save_ms = (time.perf_counter_ns() - started) / 1e6
                snapshots.append(snapshot)
                row = dict(kind='save', generation=generation, save_ms=save_ms,
                    repository=tree_space(args.repository_root), state=tree_space(args.state_root, (args.runtime_root, args.serial_dir)),
                    input_state=initial, **pause_window(args.serial_dir / source / 'firecracker.log'))
                record(row)
                # Restore/read checks stay outside save timings. They use the
                # public launch API, including repository resolution and envd readiness.
                if generation == args.generations - 1:
                    started = time.perf_counter_ns()
                    restored = create(snapshot)
                    ready = time.perf_counter_ns()
                    actual = guest(restored, 'sha256sum /mnt/clear-bench/payload').split()[0]
                    read = time.perf_counter_ns()
                    if actual != expected:
                        raise RuntimeError('Restored payload checksum mismatch')
                    record(dict(kind='restore', generation=generation, cache_mode='warm',
                        ready_ms=(ready-started)/1e6, first_read_ms=(read-ready)/1e6,
                        ready_and_read_ms=(read-started)/1e6, checksum=actual))
                    delete(restored)
                    committed = json.loads((args.repository_root/'catalog/records'/(snapshot+'.json')).read_text())['committed']
                    memory_paths = [args.repository_root/'managed-layers'/
                        (layer['digest'].replace(':', '_')+'.overlaybd.commit')
                        for layer in committed['memory_layers']]
                    residency = evict_snapshot_data(args.repository_root, args.state_root, memory_paths)
                    started = time.perf_counter_ns()
                    restored = create(snapshot)
                    ready = time.perf_counter_ns()
                    actual = guest(restored, 'sha256sum /mnt/clear-bench/payload').split()[0]
                    read = time.perf_counter_ns()
                    if actual != expected:
                        raise RuntimeError('Cold restored payload checksum mismatch')
                    record(dict(kind='restore', generation=generation, cache_mode='cold_snapshot_files',
                        ready_ms=(ready-started)/1e6, first_read_ms=(read-ready)/1e6,
                        ready_and_read_ms=(read-started)/1e6, checksum=actual, residency=residency))
                    delete(restored)
            delete(source)
            for snapshot in snapshots[:-1]:
                api('DELETE', '/snapshots/' + snapshot, status=204)
            restored = create(snapshots[-1])
            if guest(restored, 'sha256sum /mnt/clear-bench/payload').split()[0] != expected:
                raise RuntimeError('Deleting earlier snapshots invalidated latest snapshot')
            delete(restored)
            record(dict(kind='complete', retained_repository=tree_space(args.repository_root),
                input_referenced_layers=input_referenced_layers,
                retained_referenced_layers=referenced_layer_space(args.repository_root),
                retained_state=tree_space(args.state_root, (args.runtime_root, args.serial_dir))))
        finally:
            for sandbox in list(live):
                try:
                    delete(sandbox)
                except Exception as error:
                    print('Task sandbox cleanup failed:', str(error).replace(key, '[redacted]'))


if __name__ == '__main__':
    main()
