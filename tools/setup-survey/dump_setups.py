#!/usr/bin/env python3
"""Dump the Vorbis setup header (3rd Ogg packet) produced by the oracle
encoder for a sweep of base qualities x rates x channel counts.

Usage: dump_setups.py <out_dir> [q ...]

Writes <out_dir>/r<rate>_c<ch>_q<q>.bin. Requires a built
tools/oracle-encoder/oracle-encoder.
"""
import os
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ORACLE = os.path.join(HERE, "..", "oracle-encoder", "oracle-encoder")

DEFAULT_QS = ["-0.1", "-0.05", "0.0", "0.05", "0.1", "0.2", "0.25", "0.3",
              "0.4", "0.45", "0.5", "0.55", "0.6", "0.7", "0.8", "0.9",
              "0.95", "1.0"]


def packets(data):
    pos, cur = 0, b""
    while pos < len(data):
        assert data[pos:pos + 4] == b"OggS"
        nseg = data[pos + 26]
        segs = data[pos + 27:pos + 27 + nseg]
        body = pos + 27 + nseg
        for s in segs:
            cur += data[body:body + s]
            body += s
            if s < 255:
                yield cur
                cur = b""
        pos = body


def setup_packet(rate, ch, q):
    pcm = bytes(4096 * ch * 2)
    out = subprocess.run([ORACLE, str(rate), str(ch), q], input=pcm,
                         stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                         check=True).stdout
    for i, p in enumerate(packets(out)):
        if i == 2:
            assert p[:7] == b"\x05vorbis"
            return p
    raise RuntimeError("no setup packet")


def main():
    out_dir = sys.argv[1]
    qs = sys.argv[2:] or DEFAULT_QS
    os.makedirs(out_dir, exist_ok=True)
    for q in qs:
        for rate in (44100, 48000):
            for ch in (1, 2):
                p = setup_packet(rate, ch, q)
                with open(os.path.join(out_dir, f"r{rate}_c{ch}_q{q}.bin"), "wb") as f:
                    f.write(p)
                print(f"{rate} {ch} q={q}: {len(p)} bytes")


if __name__ == "__main__":
    main()
