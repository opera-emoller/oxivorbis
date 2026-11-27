"""Minimal ctypes binding for liboxivorbis (the oxivorbis C API).

No dependencies beyond the standard library. Samples may be any sequence of
numbers, an ``array.array`` ('h' for int16, 'f' for float32) or anything
exposing the buffer protocol with that format (e.g. a numpy int16/float32
array), which is then passed without copying.

    import oxivorbis
    ogg = oxivorbis.encode_i16(samples, rate=44100, channels=2, quality=5)

    with oxivorbis.Encoder(48000, 1, quality=3) as enc, open("out.ogg", "wb") as f:
        for chunk in chunks:
            f.write(enc.write_f32(chunk))
        f.write(enc.finish())

The library is found via ``$OXIVORBIS_LIB`` (a path to the shared library),
then next to this file, then in the repository's target/{release,debug}, then
on the system library path.
"""

import array
import ctypes
import ctypes.util
import os
import sys

__all__ = [
    "OxivorbisError",
    "Options",
    "Encoder",
    "Writer",
    "encode_i16",
    "encode_f32",
    "version",
    "ABI_VERSION",
]

# Must match OXV_ABI_VERSION in include/oxivorbis.h.
ABI_VERSION = 1

OK, INVALID_ARGUMENT, UNSUPPORTED, IO, INVALID_STATE, PANIC = range(6)


def _lib_names():
    if sys.platform == "win32":
        return ["oxivorbis.dll"]
    if sys.platform == "darwin":
        return ["liboxivorbis.dylib"]
    return ["liboxivorbis.so"]


def _load():
    env = os.environ.get("OXIVORBIS_LIB")
    if env:
        return ctypes.CDLL(env)
    here = os.path.dirname(os.path.abspath(__file__))
    root = os.path.normpath(os.path.join(here, "..", "..", ".."))
    for d in (here, os.path.join(root, "target", "release"), os.path.join(root, "target", "debug")):
        for name in _lib_names():
            p = os.path.join(d, name)
            if os.path.exists(p):
                return ctypes.CDLL(p)
    found = ctypes.util.find_library("oxivorbis")
    if found:
        return ctypes.CDLL(found)
    raise OSError("liboxivorbis not found; build it with `cargo build --release -p oxivorbis-ffi` "
                  "or set OXIVORBIS_LIB")


_lib = _load()

_u8p = ctypes.POINTER(ctypes.c_uint8)
_status = ctypes.c_int32


class _Buffer(ctypes.Structure):
    _fields_ = [("data", _u8p), ("len", ctypes.c_size_t)]


WRITE_FN = ctypes.CFUNCTYPE(ctypes.c_int, ctypes.c_void_p, _u8p, ctypes.c_size_t)


def _sig(name, restype, *argtypes):
    f = getattr(_lib, name)
    f.restype = restype
    f.argtypes = list(argtypes)
    return f


_vp = ctypes.c_void_p
_i16p = ctypes.POINTER(ctypes.c_int16)
_f32p = ctypes.POINTER(ctypes.c_float)
_sz = ctypes.c_size_t
_u32 = ctypes.c_uint32
_out = (ctypes.POINTER(_u8p), ctypes.POINTER(_sz))

_version = _sig("oxv_version", ctypes.c_char_p)
_abi_version = _sig("oxv_abi_version", _u32)
_status_message = _sig("oxv_status_message", ctypes.c_char_p, ctypes.c_int32)
_options_new = _sig("oxv_options_new", _vp)
_options_free = _sig("oxv_options_free", None, _vp)
_options_set_quality = _sig("oxv_options_set_quality", _status, _vp, ctypes.c_float)
_options_set_serial = _sig("oxv_options_set_serial", _status, _vp, _u32)
_options_set_vendor = _sig("oxv_options_set_vendor", _status, _vp, ctypes.c_char_p)
_options_add_comment = _sig("oxv_options_add_comment", _status, _vp, ctypes.c_char_p, ctypes.c_char_p)
_oneshot = {
    "h": _sig("oxv_encode_i16", _status, _i16p, _sz, _u32, _u32, _vp, ctypes.POINTER(_Buffer)),
    "f": _sig("oxv_encode_f32", _status, _f32p, _sz, _u32, _u32, _vp, ctypes.POINTER(_Buffer)),
}
_buffer_free = _sig("oxv_buffer_free", None, ctypes.POINTER(_Buffer))
_encoder_new = _sig("oxv_encoder_new", _status, _u32, _u32, _vp, ctypes.POINTER(_vp))
_encoder_free = _sig("oxv_encoder_free", None, _vp)
_encoder_write = {
    "h": _sig("oxv_encoder_write_i16", _status, _vp, _i16p, _sz, *_out),
    "f": _sig("oxv_encoder_write_f32", _status, _vp, _f32p, _sz, *_out),
}
_encoder_write_planar = {
    "h": _sig("oxv_encoder_write_planar_i16", _status, _vp, ctypes.POINTER(_i16p), _sz, _sz, *_out),
    "f": _sig("oxv_encoder_write_planar_f32", _status, _vp, ctypes.POINTER(_f32p), _sz, _sz, *_out),
}
_encoder_finish = _sig("oxv_encoder_finish", _status, _vp, *_out)
_writer_new = _sig("oxv_writer_new", _status, _u32, _u32, _vp, WRITE_FN, _vp, ctypes.POINTER(_vp))
_writer_free = _sig("oxv_writer_free", None, _vp)
_writer_write = {
    "h": _sig("oxv_writer_write_i16", _status, _vp, _i16p, _sz),
    "f": _sig("oxv_writer_write_f32", _status, _vp, _f32p, _sz),
}
_writer_finish = _sig("oxv_writer_finish", _status, _vp)

if _abi_version() != ABI_VERSION:
    raise ImportError("liboxivorbis ABI %d, this binding expects %d" % (_abi_version(), ABI_VERSION))


class OxivorbisError(Exception):
    """A non-OK oxv_status; ``.status`` holds the code."""

    def __init__(self, status):
        self.status = status
        super().__init__("%s (oxv_status %d)" % (_status_message(status).decode(), status))


def _check(status):
    if status != OK:
        raise OxivorbisError(status)


def version():
    """liboxivorbis version string, e.g. '0.1.0'."""
    return _version().decode()


_CTYPE = {"h": ctypes.c_int16, "f": ctypes.c_float}


def _samples(samples, code):
    """(ctypes pointer, count, keepalive) for `samples` as int16 ('h') or float32 ('f')."""
    ctype = _CTYPE[code]
    try:
        mv = memoryview(samples)
    except TypeError:
        mv = None
    if mv is None or mv.format.lstrip("@=<") != code or not mv.c_contiguous:
        mv = memoryview(array.array(code, samples))
    mv = mv.cast("B").cast(code) if mv.ndim != 1 else mv
    n = mv.nbytes // ctypes.sizeof(ctype)
    if n == 0:
        return ctypes.cast(None, ctypes.POINTER(ctype)), 0, None
    try:
        buf = (ctype * n).from_buffer(mv)
    except TypeError:  # read-only buffer (bytes, ...): copy
        buf = (ctype * n).from_buffer_copy(mv)
    return ctypes.cast(buf, ctypes.POINTER(ctype)), n, buf


class Options:
    """Encoder settings. Fresh options are the library defaults (quality 5,
    fixed serial, vendor "oxivorbis <version>", no comments)."""

    def __init__(self, quality=None, serial=None, vendor=None, comments=()):
        self._h = _options_new()
        if not self._h:
            raise MemoryError("oxv_options_new failed")
        if quality is not None:
            _check(_options_set_quality(self._h, quality))
        if serial is not None:
            _check(_options_set_serial(self._h, serial))
        if vendor is not None:
            _check(_options_set_vendor(self._h, vendor.encode()))
        for key, value in comments:
            _check(_options_add_comment(self._h, key.encode(), value.encode()))

    def __del__(self):
        if getattr(self, "_h", None):
            _options_free(self._h)
            self._h = None

    @staticmethod
    def _handle(options, quality, serial, vendor, comments):
        if options is None:
            options = Options(quality, serial, vendor, comments)
        return options, options._h


def _encode(code, samples, rate, channels, options, quality, serial, vendor, comments):
    opts, h = Options._handle(options, quality, serial, vendor, comments)
    ptr, n, keep = _samples(samples, code)
    buf = _Buffer()
    _check(_oneshot[code](ptr, n, rate, channels, h, ctypes.byref(buf)))
    try:
        return ctypes.string_at(buf.data, buf.len)
    finally:
        _buffer_free(ctypes.byref(buf))
        del keep, opts


def encode_i16(samples, rate, channels, options=None, *, quality=None, serial=None, vendor=None,
               comments=()):
    """Encode interleaved int16 samples into a complete Ogg Vorbis stream (bytes)."""
    return _encode("h", samples, rate, channels, options, quality, serial, vendor, comments)


def encode_f32(samples, rate, channels, options=None, *, quality=None, serial=None, vendor=None,
               comments=()):
    """Encode interleaved float samples (nominally -1..1) into a complete Ogg Vorbis stream."""
    return _encode("f", samples, rate, channels, options, quality, serial, vendor, comments)


class Encoder:
    """Streaming encoder: each write returns the Ogg bytes it completed,
    finish() returns the rest. Not thread-safe; use one per thread."""

    def __init__(self, rate, channels, options=None, *, quality=None, serial=None, vendor=None,
                 comments=()):
        _, h = Options._handle(options, quality, serial, vendor, comments)
        self.channels = channels
        enc = _vp()
        _check(_encoder_new(rate, channels, h, ctypes.byref(enc)))
        self._h = enc

    def _out(self, call, *args):
        p, n = _u8p(), _sz()
        _check(call(self._h, *args, ctypes.byref(p), ctypes.byref(n)))
        # Copy now: the buffer is only valid until the next call.
        return ctypes.string_at(p, n.value) if n.value else b""

    def write_i16(self, samples):
        ptr, n, keep = _samples(samples, "h")
        return self._out(_encoder_write["h"], ptr, n)

    def write_f32(self, samples):
        ptr, n, keep = _samples(samples, "f")
        return self._out(_encoder_write["f"], ptr, n)

    def _planar(self, code, channels):
        bufs = [_samples(c, code) for c in channels]
        frames = min((n for _, n, _ in bufs), default=0)
        ptrs = (ctypes.POINTER(_CTYPE[code]) * len(bufs))(*[p for p, _, _ in bufs])
        return self._out(_encoder_write_planar[code], ptrs, len(bufs), frames)

    def write_planar_i16(self, channels):
        """One int16 sequence per channel; encodes as many frames as the shortest holds."""
        return self._planar("h", channels)

    def write_planar_f32(self, channels):
        """One float sequence per channel; encodes as many frames as the shortest holds."""
        return self._planar("f", channels)

    def finish(self):
        return self._out(_encoder_finish)

    def close(self):
        if getattr(self, "_h", None):
            _encoder_free(self._h)
            self._h = None

    __del__ = close

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.close()


class Writer:
    """Streaming encoder that writes its pages to a binary file object
    (anything with .write(bytes)) through the C callback API."""

    def __init__(self, fileobj, rate, channels, options=None, *, quality=None, serial=None,
                 vendor=None, comments=()):
        _, h = Options._handle(options, quality, serial, vendor, comments)
        self._file = fileobj
        self._error = None

        def write(_user, data, length):
            try:
                self._file.write(ctypes.string_at(data, length))
                return 0
            except Exception as e:  # never let an exception cross into C
                self._error = e
                return 1

        self._cb = WRITE_FN(write)  # keep the trampoline alive
        w = _vp()
        _check(_writer_new(rate, channels, h, self._cb, None, ctypes.byref(w)))
        self._h = w

    def _call(self, status):
        if status == IO and self._error is not None:
            raise self._error
        _check(status)

    def write_i16(self, samples):
        ptr, n, keep = _samples(samples, "h")
        self._call(_writer_write["h"](self._h, ptr, n))

    def write_f32(self, samples):
        ptr, n, keep = _samples(samples, "f")
        self._call(_writer_write["f"](self._h, ptr, n))

    def finish(self):
        self._call(_writer_finish(self._h))

    def close(self):
        if getattr(self, "_h", None):
            _writer_free(self._h)
            self._h = None

    __del__ = close

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.close()
