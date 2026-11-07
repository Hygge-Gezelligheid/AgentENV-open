#!/usr/bin/env python3
"""Small client for the checked-in envd process.proto Start RPC over HTTP/1.1."""
import argparse
import struct
import urllib.request

def varint(n):
    out = bytearray()
    while n > 127:
        out.append((n & 127) | 128)
        n >>= 7
    out.append(n)
    return bytes(out)

def field(n, data):
    if isinstance(data, str):
        data = data.encode()
    return varint(n*8+2) + varint(len(data)) + data

def fields(data):
    i = 0
    def number():
        nonlocal i
        value = shift = 0
        while True:
            b = data[i]
            i += 1
            value |= (b & 127) << shift
            if not b & 128:
                return value
            shift += 7
    while i < len(data):
        tag = number()
        wire = tag & 7
        if wire == 0:
            value = number()
        elif wire == 2:
            length = number()
            value = data[i:i+length]
            i += length
        else:
            raise ValueError(f'unexpected protobuf wire type {wire}')
        yield tag >> 3, value

def run(base, command, headers=None):
    process = field(1, '/bin/sh') + field(2, '-c') + field(2, command)
    request = field(1, process) + b'\x20\x00'  # stdin=false
    data = b'\x00' + struct.pack('>I', len(request)) + request
    h = {'Content-Type': 'application/grpc', 'TE': 'trailers', 'X-User': 'root'}
    h.update(headers or {})
    req = urllib.request.Request(base.rstrip('/')+'/process.Process/Start', data=data, headers=h)
    output = bytearray()
    exit_code = None
    with urllib.request.urlopen(req, timeout=60) as response:
        while True:
            frame = response.read(5)
            if not frame:
                break
            if len(frame) != 5 or frame[0] != 0:
                raise ValueError('invalid/unexpected gRPC frame')
            length = struct.unpack('>I', frame[1:])[0]
            payload = response.read(length)
            if len(payload) != length:
                raise ValueError('truncated gRPC frame')
            for n, event in fields(payload):
                if n != 1:
                    continue
                for kind, item in fields(event):
                    if kind == 2:
                        for stream, chunk in fields(item):
                            if stream in (1, 2):
                                output.extend(chunk)
                    elif kind == 3:
                        end = dict(fields(item))
                        code = end.get(1, 0)
                        exit_code = (code >> 1) ^ -(code & 1)
                        if end.get(4):
                            raise RuntimeError(end[4].decode())
                        # Match the native client: End is terminal even if
                        # the transport has not closed its response stream.
                        if exit_code != 0:
                            raise RuntimeError(f'Guest command exit={exit_code}: {output.decode(errors="replace")}')
                        return output.decode()
    if exit_code != 0:
        raise RuntimeError(f'Guest command exit={exit_code}: {output.decode(errors="replace")}')
    return output.decode()

if __name__ == '__main__':
    p = argparse.ArgumentParser()
    p.add_argument('url')
    p.add_argument('command')
    args = p.parse_args()
    print(run(args.url, args.command), end='')
