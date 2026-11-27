"""Tests for the ctypes binding (standard library only):

    cargo build -p oxivorbis-ffi -p oxivorbis-cli
    python3 ffi/bindings/python/test_oxivorbis.py

Compares the binding's output with the `oxivorbis` command-line encoder on
the same raw PCM and settings (byte for byte), and checks the stream
structure with a small pure-Python Ogg page parser (capture pattern, CRC,
page sequence, BOS/EOS flags, final granule position = frame count).

$OXIVORBIS_LIB selects the library (see oxivorbis.py); $OXIVORBIS_CLI the
CLI binary (default: target/debug/oxivorbis, then target/release).
"""

import array
import io
import math
import os
import struct
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import oxivorbis  # noqa: E402

ROOT = os.path.normpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", ".."))


def cli_path():
    env = os.environ.get("OXIVORBIS_CLI")
    if env:
        return env
    exe = "oxivorbis.exe" if sys.platform == "win32" else "oxivorbis"
    for profile in ("debug", "release"):
        p = os.path.join(ROOT, "target", profile, exe)
        if os.path.exists(p):
            return p
    return None


def make_signal(frames, channels):
    """Deterministic int16 interleaved test signal: tones plus LCG noise."""
    out = array.array("h")
    state = 1
    for i in range(frames):
        for c in range(channels):
            state = (state * 1103515245 + 12345) & 0xFFFFFFFF
            noise = ((state >> 16) & 0x7FF) - 0x400
            tone = 9000 * math.sin(2 * math.pi * (440 + 110 * c) * i / 44100)
            out.append(int(tone) + noise)
    return out


def le_bytes(arr):
    """Little-endian raw bytes of an array.array (what the CLI's --raw reads)."""
    if sys.byteorder != "little":
        arr = array.array(arr.typecode, arr)
        arr.byteswap()
    return arr.tobytes()


# ---- a tiny Ogg page parser --------------------------------------------------

def _crc_table():
    table = []
    for i in range(256):
        r = i << 24
        for _ in range(8):
            r = ((r << 1) ^ 0x04C11DB7) if r & 0x80000000 else (r << 1)
        table.append(r & 0xFFFFFFFF)
    return table


_CRC = _crc_table()


def ogg_crc(data):
    crc = 0
    for b in data:
        crc = ((crc << 8) & 0xFFFFFFFF) ^ _CRC[((crc >> 24) & 0xFF) ^ b]
    return crc


def ogg_pages(data):
    """Yield (flags, granule, serial, seq, packets_bytes) per page; checks CRCs."""
    pos = 0
    while pos < len(data):
        if data[pos:pos + 4] != b"OggS":
            raise ValueError("no capture pattern at %d" % pos)
        version, flags, granule, serial, seq, crc, nseg = struct.unpack_from("<BBqIIIB", data, pos + 4)
        if version != 0:
            raise ValueError("bad version")
        lacing = data[pos + 27:pos + 27 + nseg]
        size = 27 + nseg + sum(lacing)
        page = bytearray(data[pos:pos + size])
        page[22:26] = b"\0\0\0\0"
        if ogg_crc(page) != crc:
            raise ValueError("CRC mismatch in page %d" % seq)
        yield flags, granule, serial, seq, bytes(data[pos + 27 + nseg:pos + size])
        pos += size


class StreamChecks:
    def assert_valid_stream(self, ogg, frames, serial=None, rate=None, channels=None):
        pages = list(ogg_pages(ogg))
        self.assertGreaterEqual(len(pages), 2)
        self.assertEqual([p[3] for p in pages], list(range(len(pages))))  # page sequence
        self.assertTrue(pages[0][0] & 0x02, "first page must have BOS")
        self.assertTrue(pages[-1][0] & 0x04, "last page must have EOS")
        self.assertEqual(len({p[2] for p in pages}), 1)
        if serial is not None:
            self.assertEqual(pages[0][2], serial)
        ident = pages[0][4]
        self.assertEqual(ident[:7], b"\x01vorbis")
        ch, sr = struct.unpack_from("<BI", ident, 11)
        if channels is not None:
            self.assertEqual(ch, channels)
        if rate is not None:
            self.assertEqual(sr, rate)
        self.assertEqual(pages[-1][1], frames, "final granule position = frame count")


class TestBinding(unittest.TestCase, StreamChecks):
    def test_version(self):
        self.assertRegex(oxivorbis.version(), r"^\d+\.\d+\.\d+")

    def test_one_shot_and_streaming_agree(self):
        pcm = make_signal(30000, 2)
        one = oxivorbis.encode_i16(pcm, 44100, 2, quality=4, serial=5, comments=[("TITLE", "t")])
        self.assert_valid_stream(one, 30000, serial=5, rate=44100, channels=2)
        opts = oxivorbis.Options(quality=4, serial=5, comments=[("TITLE", "t")])
        with oxivorbis.Encoder(44100, 2, opts) as enc:
            out = b"".join(enc.write_i16(pcm[i:i + 999]) for i in range(0, len(pcm), 999))
            out += enc.finish()
        self.assertEqual(out, one)
        # The callback writer, to a file object.
        f = io.BytesIO()
        with oxivorbis.Writer(f, 44100, 2, opts) as w:
            for i in range(0, len(pcm), 5000):
                w.write_i16(pcm[i:i + 5000])
            w.finish()
        self.assertEqual(f.getvalue(), one)
        # Planar, from plain lists.
        left, right = list(pcm[0::2]), list(pcm[1::2])
        with oxivorbis.Encoder(44100, 2, opts) as enc:
            out = enc.write_planar_i16([left, right]) + enc.finish()
        self.assertEqual(out, one)

    def test_f32(self):
        pcm = [s / 32768.0 for s in make_signal(20000, 1)]
        ogg = oxivorbis.encode_f32(pcm, 22050, 1)
        self.assert_valid_stream(ogg, 20000, rate=22050, channels=1)
        with oxivorbis.Encoder(22050, 1) as enc:
            out = enc.write_f32(array.array("f", pcm)) + enc.finish()
        self.assertEqual(out, ogg)

    def test_errors(self):
        with self.assertRaises(oxivorbis.OxivorbisError) as cm:
            oxivorbis.encode_i16([0, 0], 0, 2)
        self.assertEqual(cm.exception.status, oxivorbis.UNSUPPORTED)
        with self.assertRaises(oxivorbis.OxivorbisError) as cm:
            oxivorbis.Options(quality=11)
        self.assertEqual(cm.exception.status, oxivorbis.INVALID_ARGUMENT)
        with self.assertRaises(oxivorbis.OxivorbisError) as cm:
            oxivorbis.Encoder(44100, 7 + 300)
        self.assertEqual(cm.exception.status, oxivorbis.UNSUPPORTED)
        enc = oxivorbis.Encoder(44100, 2)
        with self.assertRaises(oxivorbis.OxivorbisError) as cm:
            enc.write_planar_f32([[0.0] * 10])  # one plane for a stereo encoder
        self.assertEqual(cm.exception.status, oxivorbis.INVALID_ARGUMENT)
        enc.finish()
        with self.assertRaises(oxivorbis.OxivorbisError) as cm:
            enc.finish()
        self.assertEqual(cm.exception.status, oxivorbis.INVALID_STATE)
        enc.close()

    def test_writer_propagates_file_errors(self):
        class Broken:
            def write(self, _):
                raise OSError("disk full")

        with oxivorbis.Writer(Broken(), 44100, 1) as w:
            with self.assertRaises(OSError):
                w.write_i16([0] * 100)
            with self.assertRaises(oxivorbis.OxivorbisError) as cm:
                w.finish()
            self.assertEqual(cm.exception.status, oxivorbis.INVALID_STATE)


@unittest.skipIf(cli_path() is None, "oxivorbis CLI not built (cargo build -p oxivorbis-cli)")
class TestAgainstCli(unittest.TestCase, StreamChecks):
    def run_cli(self, raw, args):
        with tempfile.TemporaryDirectory() as d:
            src, dst = os.path.join(d, "in.raw"), os.path.join(d, "out.ogg")
            with open(src, "wb") as f:
                f.write(raw)
            subprocess.run([cli_path(), "-Q", "--raw", *args, "-o", dst, src], check=True)
            with open(dst, "rb") as f:
                return f.read()

    def test_s16le_matches_cli(self):
        pcm = make_signal(44100, 2)
        raw = le_bytes(pcm)
        cli = self.run_cli(raw, ["-R", "44100", "-C", "2", "-q", "6.5", "-s", "1234",
                                 "-c", "TITLE=From Python"])
        ours = oxivorbis.encode_i16(pcm, 44100, 2, quality=6.5, serial=1234,
                                    comments=[("TITLE", "From Python")])
        self.assertEqual(ours, cli)
        self.assert_valid_stream(ours, 44100, serial=1234, rate=44100, channels=2)

    def test_f32le_matches_cli(self):
        pcm = array.array("f", (s / 30000.0 for s in make_signal(16000, 1)))
        cli = self.run_cli(le_bytes(pcm), ["-R", "16000", "-C", "1", "-F", "f32le", "-q", "0"])
        ours = oxivorbis.encode_f32(pcm, 16000, 1, quality=0)
        self.assertEqual(ours, cli)
        self.assert_valid_stream(ours, 16000, rate=16000, channels=1)


if __name__ == "__main__":
    unittest.main(verbosity=2)
