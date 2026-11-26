#!/usr/bin/env python3
"""Proof-of-concept: regenerate libvorbis 1.3.7's Vorbis setup header
(third header packet) for VBR mode at 44.1/48 kHz, mono/stereo, any base
quality in -0.1..1.0, purely from the vendored template sources:

    modes/setup_44.h, setup_44u.h      (quality_mapping, floor mapping)
    modes/residue_44.h, residue_44u.h  (residue templates, bookblocks)
    modes/floor_all.h                  (floor1 templates, floor book lists)
    modes/psych_44.h                   (_psy_lowpass_44)
    books/{coupled,uncoupled,floor}/*.h (static codebooks)

It ports the bits of vorbisenc.c that matter for the setup header
(get_setup_template, vorbis_encode_floor_setup,
vorbis_encode_residue_setup incl. the lowpass -> residue `end` rule,
vorbis_encode_map_n_res_setup) and the packers (_vorbis_pack_books,
vorbis_staticbook_pack, floor1_pack, res0_pack, mapping0_pack).

Usage:
    setupgen.py gen <rate> <channels> <q> <out.bin>   write one header
    setupgen.py check <dump_dir>                      compare vs dumps
                                                      (dump_setups.py)
    setupgen.py describe <rate> <channels> <q>        print structure
    setupgen.py stats                                 book data sizes
"""
import math
import os
import re
import struct
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
VEND = os.path.join(HERE, "..", "debug-libvorbis-dump", "vendored-libvorbis")


def f32(x):
    """Round a Python float (double) to IEEE single precision."""
    return struct.unpack("<f", struct.pack("<f", x))[0]


# ---------------------------------------------------------------------------
# Minimal C static-initializer parser
# ---------------------------------------------------------------------------

TOKEN_RE = re.compile(r"""
    (?P<ws>\s+|/\*.*?\*/|//[^\n]*)
  | (?P<cast>\(\s*(?:const\s+)?(?:char|long|int|static_codebook)\s*\*\s*\))
  | (?P<num>[-+]?(?:\d+\.\d*|\.\d+|\d+)(?:[eE][-+]?\d+)?[fFlL]?)
  | (?P<id>[A-Za-z_]\w*)
  | (?P<p>[{}=,;&\[\]*()])
""", re.S | re.X)


def tokenize(src):
    out = []
    pos = 0
    while pos < len(src):
        m = TOKEN_RE.match(src, pos)
        if not m:
            raise ValueError("tokenize error near %r" % src[pos:pos + 40])
        pos = m.end()
        if m.lastgroup in ("ws", "cast"):
            continue
        out.append((m.lastgroup, m.group(m.lastgroup)))
    return out


def parse_init(toks, i):
    """Parse an initializer starting at toks[i]. Returns (value, next_i).
    Braced lists -> Python lists; numbers -> int/float; &name / name ->
    ('ref', name)."""
    kind, val = toks[i]
    if val == "{":
        items = []
        i += 1
        while toks[i][1] != "}":
            v, i = parse_init(toks, i)
            items.append(v)
            if toks[i][1] == ",":
                i += 1
        return items, i + 1
    if val == "&":
        return ("ref", toks[i + 1][1]), i + 2
    if kind == "num":
        s = val.rstrip("fFlL")
        if any(c in s for c in ".eE"):
            return float(s), i + 1
        return int(s), i + 1
    if kind == "id":
        if val == "NULL":
            return None, i + 1
        return ("ref", val), i + 1
    raise ValueError("unexpected token %r" % (toks[i],))


def parse_decls(path, db):
    """Collect every `static const <type> [*const ...] NAME[...]= init;`."""
    src = open(path).read()
    # drop preprocessor lines
    src = "\n".join(l for l in src.split("\n") if not l.lstrip().startswith("#"))
    toks = tokenize(src)
    i = 0
    n = len(toks)
    while i < n:
        if toks[i][1] == "static":
            # find '=' before ';'
            j = i
            while toks[j][1] not in ("=", ";"):
                j += 1
            if toks[j][1] == ";":
                i = j + 1
                continue
            # name: last identifier before '=' (skipping [..])
            k = j - 1
            if toks[k][1] == "]":
                while toks[k][1] != "[":
                    k -= 1
                k -= 1
            name = toks[k][1]
            ctype = " ".join(t[1] for t in toks[i + 1:k])
            val, i = parse_init(toks, j + 1)
            db[name] = (ctype, val)
            assert toks[i][1] == ";", (name, toks[i])
            i += 1
        else:
            i += 1
    return db


def load_db():
    db = {}
    for rel in ["books/floor/floor_books.h",
                "books/coupled/res_books_stereo.h",
                "books/uncoupled/res_books_uncoupled.h",
                "modes/floor_all.h",
                "modes/residue_44.h",
                "modes/residue_44u.h",
                "modes/psych_44.h",
                "modes/setup_44.h",
                "modes/setup_44u.h"]:
        parse_decls(os.path.join(VEND, rel), db)
    # vorbisenc.c's own _map_nominal (residue_44u.h has _map_nominal_u)
    db["_map_nominal"] = ("vorbis_info_mapping0",
                          [[1, [0, 0], [0], [0], 1, [0], [1]],
                           [1, [0, 0], [1], [1], 1, [0], [1]]])
    return db


def deref(db, v):
    while isinstance(v, tuple) and v[0] == "ref":
        v = db[v[1]][1]
    return v


def pad(lst, n, fill=0):
    lst = list(lst)
    return lst + [fill] * (n - len(lst))


# ---------------------------------------------------------------------------
# Structured views of the templates
# ---------------------------------------------------------------------------

class Book:
    def __init__(self, name, init, db):
        self.name = name
        (self.dim, self.entries, ll, self.maptype, self.q_min, self.q_delta,
         self.q_quant, self.q_sequencep, ql, _alloc) = init
        self.lengthlist = deref(db, ll)
        self.quantlist = deref(db, ql) if ql is not None else None
        assert len(self.lengthlist) == self.entries, name


def floor1_from(init):
    (partitions, pclass, cdim, csubs, cbook, csub, mult, post,
     maxover, maxunder, maxerr, twofitweight, twofitatten, n) = init
    return dict(partitions=partitions, partitionclass=pad(pclass, 31),
                class_dim=pad(cdim, 16), class_subs=pad(csubs, 16),
                class_book=pad(cbook, 16),
                class_subbook=[pad(s, 8) for s in csub] + [[0] * 8] * (16 - len(csub)),
                mult=mult, postlist=pad(post, 65),
                maxover=maxover, maxunder=maxunder, maxerr=maxerr,
                twofitweight=twofitweight, twofitatten=twofitatten, n=n)


def residue0_from(init):
    begin, end, grouping, partitions, partvals, groupbook = init[:6]
    return dict(begin=begin, end=end, grouping=grouping, partitions=partitions,
                classmetric1=pad(init[8], 64), classmetric2=pad(init[9], 64))


# ---------------------------------------------------------------------------
# vorbisenc.c port (VBR, unmanaged)
# ---------------------------------------------------------------------------

def get_setup(db, ch, rate, quality):
    """vorbis_encode_setup_vbr + get_setup_template. Returns
    (template_name, base_setting)."""
    q = f32(f32(quality) + f32(.0000001))  # float arithmetic in C
    if q >= 1.0:
        q = f32(.9999)
    req = q  # promoted to double
    tmpl = "ve_setup_44_stereo" if ch == 2 else "ve_setup_44_uncoupled"
    assert 40000 <= rate <= 50000
    t = deref(db, ("ref", tmpl))
    mappings = t[0]
    qmap = deref(db, t[2])
    assert qmap[0] <= req <= qmap[mappings]
    for j in range(mappings):
        if qmap[j] <= req < qmap[j + 1]:
            break
    else:
        j = mappings
    if j == mappings:
        base = j - .001
    else:
        low = f32(qmap[j])
        high = f32(qmap[j + 1])
        d = f32((req - low) / (high - low))
        base = j + d
    return tmpl, t, base


class Setup:
    pass


def build(db, rate, ch, quality):
    tmpl, t, base = get_setup(db, ch, rate, quality)
    S = Setup()
    S.template = tmpl
    S.base_setting = base
    is_ = int(base)
    ds = base - is_
    # template field indices in ve_setup_data_template
    blocksize_short, blocksize_long = deref(db, t[6]), deref(db, t[7])
    psy_lowpass = deref(db, t[28])
    floor_books = deref(db, t[32])
    floor_params = deref(db, t[33])
    floor_mappings = t[34]
    floor_mapping_list = deref(db, t[35])
    maps = deref(db, t[36])

    S.lowpass_kHz = psy_lowpass[is_] * (1. - ds) + psy_lowpass[is_ + 1] * ds
    S.blocksizes = [blocksize_short[is_], blocksize_long[is_]]

    S.books = []        # list of names, in header order
    S.floors = []
    S.floor_template_idx = []

    # --- vorbis_encode_floor_setup ---
    for fm in range(floor_mappings):
        x = deref(db, floor_mapping_list[fm])
        fi = x[is_]
        f = floor1_from(floor_params[fi])
        blist = deref(db, floor_books[fi])
        maxclass = max(f["partitionclass"][:f["partitions"]])
        maxbook = -1
        nb = len(S.books)
        for i in range(maxclass + 1):
            maxbook = max(maxbook, f["class_book"][i])
            f["class_book"][i] += nb
            for k in range(1 << f["class_subs"][i]):
                sb = f["class_subbook"][i][k]
                maxbook = max(maxbook, sb)
                if sb >= 0:
                    f["class_subbook"][i][k] += nb
        for i in range(maxbook + 1):
            S.books.append(blist[i][1])
        S.floors.append(f)
        S.floor_template_idx.append(fi)

    # --- vorbis_encode_map_n_res_setup ---
    map_t, res_t = maps[is_]
    map_name = map_t[1]
    res_name = res_t[1]
    map_init = deref(db, map_t)
    res_init = deref(db, res_t)
    modes = 1 if S.blocksizes[0] == S.blocksizes[1] else 2
    S.mapping_template = map_name
    S.residue_template = res_name
    S.modes = [(0, 0, 0, 0), (1, 0, 0, 1)][:modes]
    S.maps = []
    S.residues = {}
    S.residue_type = {}
    S.residue_base = {}
    for i in range(modes):
        m = map_init[i]
        submaps, chmux, floorsub, ressub, csteps, cmag, cang = m
        S.maps.append(dict(submaps=submaps, chmuxlist=pad(chmux, ch),
                           floorsubmap=floorsub, residuesubmap=ressub,
                           coupling_steps=csteps, coupling_mag=cmag,
                           coupling_ang=cang))
        for j in range(submaps):
            num = ressub[j]
            residue_setup(db, S, rate, ch, num, i, res_init[num])
    return S


def book_dup_or_new(S, name):
    if name in S.books:
        return S.books.index(name)
    S.books.append(name)
    return len(S.books) - 1


def residue_setup(db, S, rate, ch, number, block, rt):
    res_type, limit_type, grouping, res_p, aux, aux_m, base_b, base_m = rt
    r = residue0_from(deref(db, res_p))
    S.residue_base[number] = res_p[1]
    r["grouping"] = grouping
    S.residue_type[number] = res_type
    bb = deref(db, base_b)[0]  # static_bookblock { books[12][4] }
    secondstages = [0] * 64
    booklist = []
    for i in range(r["partitions"]):
        row = pad(bb[i], 4, None) if i < len(bb) else [None] * 4
        for k in range(4):
            if row[k] not in (None, 0):
                secondstages[i] |= 1 << k
    r["groupbook"] = book_dup_or_new(S, aux[1])
    for i in range(r["partitions"]):
        row = pad(bb[i], 4, None) if i < len(bb) else [None] * 4
        for k in range(4):
            if row[k] not in (None, 0):
                booklist.append(book_dup_or_new(S, row[k][1]))
    r["secondstages"] = secondstages
    r["booklist"] = booklist

    # lowpass / end
    freq = S.lowpass_kHz * 1000.
    f = S.floors[block]
    nyq = rate / 2.
    blocksize = S.blocksizes[block] >> 1
    if freq > nyq:
        freq = nyq
    f["n"] = int(freq / nyq * blocksize)
    assert limit_type == 0  # 44.1 stereo/uncoupled templates only use 0
    if res_type == 2:
        nch = 0
        for m in S.maps:
            for j in range(m["submaps"]):
                if m["residuesubmap"][j] == number:
                    nch = sum(1 for k in range(ch) if m["chmuxlist"][k] == j)
                    break
            if nch:
                break
        end = int((freq / nyq * blocksize * nch) / grouping + .9) * grouping
        if end > blocksize * nch:
            end = blocksize * nch // grouping * grouping
    else:
        end = int((freq / nyq * blocksize) / grouping + .9) * grouping
        if end > blocksize:
            end = blocksize // grouping * grouping
    if end == 0:
        end = grouping
    r["end"] = end
    S.residues[number] = r


# ---------------------------------------------------------------------------
# Packing (oggpack LSB-first)
# ---------------------------------------------------------------------------

class BitWriter:
    def __init__(self):
        self.acc = 0
        self.nbits = 0

    def write(self, v, bits):
        if bits == 0:
            return
        v &= (1 << bits) - 1
        self.acc |= v << self.nbits
        self.nbits += bits

    def bytes(self):
        n = (self.nbits + 7) // 8
        return self.acc.to_bytes(n, "little")


def ilog(v):
    r = 0
    while v > 0:
        r += 1
        v >>= 1
    return r


def maptype1_quantvals(b):
    vals = int(math.floor(math.pow(b.entries, 1. / b.dim)))
    while True:
        acc = 1
        acc1 = 1
        for _ in range(b.dim):
            acc *= vals
            acc1 *= vals + 1
        if acc <= b.entries and acc1 > b.entries:
            return vals
        if acc > b.entries:
            vals -= 1
        else:
            vals += 1


def pack_book(w, b):
    w.write(0x564342, 24)
    w.write(b.dim, 16)
    w.write(b.entries, 24)
    ll = b.lengthlist
    i = 1
    while i < b.entries:
        if ll[i - 1] == 0 or ll[i] < ll[i - 1]:
            break
        i += 1
    ordered = i == b.entries
    if ordered:
        count = 0
        w.write(1, 1)
        w.write(ll[0] - 1, 5)
        for i in range(1, b.entries):
            this, last = ll[i], ll[i - 1]
            if this > last:
                for _ in range(last, this):
                    w.write(i - count, ilog(b.entries - count))
                    count = i
        w.write(b.entries - count, ilog(b.entries - count))
    else:
        w.write(0, 1)
        if 0 not in ll:
            w.write(0, 1)
            for l in ll:
                w.write(l - 1, 5)
        else:
            w.write(1, 1)
            for l in ll:
                if l == 0:
                    w.write(0, 1)
                else:
                    w.write(1, 1)
                    w.write(l - 1, 5)
    w.write(b.maptype, 4)
    if b.maptype in (1, 2):
        w.write(b.q_min & 0xffffffff, 32)
        w.write(b.q_delta & 0xffffffff, 32)
        w.write(b.q_quant - 1, 4)
        w.write(b.q_sequencep, 1)
        qv = maptype1_quantvals(b) if b.maptype == 1 else b.entries * b.dim
        for i in range(qv):
            w.write(abs(b.quantlist[i]), b.q_quant)
    else:
        assert b.maptype == 0


def pack_floor1(w, f):
    maxposit = f["postlist"][1]
    w.write(f["partitions"], 5)
    maxclass = -1
    for j in range(f["partitions"]):
        w.write(f["partitionclass"][j], 4)
        maxclass = max(maxclass, f["partitionclass"][j])
    for j in range(maxclass + 1):
        w.write(f["class_dim"][j] - 1, 3)
        w.write(f["class_subs"][j], 2)
        if f["class_subs"][j]:
            w.write(f["class_book"][j], 8)
        for k in range(1 << f["class_subs"][j]):
            w.write(f["class_subbook"][j][k] + 1, 8)
    w.write(f["mult"] - 1, 2)
    rangebits = ilog(maxposit - 1)
    w.write(rangebits, 4)
    k = 0
    count = 0
    for j in range(f["partitions"]):
        count += f["class_dim"][f["partitionclass"][j]]
        while k < count:
            w.write(f["postlist"][k + 2], rangebits)
            k += 1


def pack_res0(w, r):
    w.write(r["begin"], 24)
    w.write(r["end"], 24)
    w.write(r["grouping"] - 1, 24)
    w.write(r["partitions"] - 1, 6)
    w.write(r["groupbook"], 8)
    acc = 0
    for j in range(r["partitions"]):
        s = r["secondstages"][j]
        if ilog(s) > 3:
            w.write(s, 3)
            w.write(1, 1)
            w.write(s >> 3, 5)
        else:
            w.write(s, 4)
        acc += bin(s).count("1")
    for j in range(acc):
        w.write(r["booklist"][j], 8)


def pack_mapping0(w, m, ch):
    if m["submaps"] > 1:
        w.write(1, 1)
        w.write(m["submaps"] - 1, 4)
    else:
        w.write(0, 1)
    if m["coupling_steps"] > 0:
        w.write(1, 1)
        w.write(m["coupling_steps"] - 1, 8)
        for i in range(m["coupling_steps"]):
            w.write(m["coupling_mag"][i], ilog(ch - 1))
            w.write(m["coupling_ang"][i], ilog(ch - 1))
    else:
        w.write(0, 1)
    w.write(0, 2)
    if m["submaps"] > 1:
        for i in range(ch):
            w.write(m["chmuxlist"][i], 4)
    for i in range(m["submaps"]):
        w.write(0, 8)
        w.write(m["floorsubmap"][i], 8)
        w.write(m["residuesubmap"][i], 8)


def pack_setup(db, S, ch):
    w = BitWriter()
    w.write(0x05, 8)
    for c in b"vorbis":
        w.write(c, 8)
    w.write(len(S.books) - 1, 8)
    for name in S.books:
        pack_book(w, Book(name, deref(db, ("ref", name)), db))
    w.write(0, 6)
    w.write(0, 16)
    w.write(len(S.floors) - 1, 6)
    for f in S.floors:
        w.write(1, 16)
        pack_floor1(w, f)
    w.write(len(S.residues) - 1, 6)
    for i in range(len(S.residues)):
        w.write(S.residue_type[i], 16)
        pack_res0(w, S.residues[i])
    w.write(len(S.maps) - 1, 6)
    for m in S.maps:
        w.write(0, 16)
        pack_mapping0(w, m, ch)
    w.write(len(S.modes) - 1, 6)
    for blockflag, wt, tt, mp in S.modes:
        w.write(blockflag, 1)
        w.write(wt, 16)
        w.write(tt, 16)
        w.write(mp, 8)
    w.write(1, 1)
    return w.bytes()


def generate(db, rate, ch, q):
    S = build(db, rate, ch, q)
    return S, pack_setup(db, S, ch)


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------

def describe(db, rate, ch, q):
    S, blob = generate(db, rate, ch, q)
    print(f"rate={rate} ch={ch} q={q}: template={S.template} "
          f"base_setting={S.base_setting:.6f} lowpass_kHz={S.lowpass_kHz:.4f} "
          f"blocksizes={S.blocksizes} bytes={len(blob)}")
    print(f"  books ({len(S.books)}): {' '.join(S.books)}")
    for i, f in enumerate(S.floors):
        print(f"  floor{i}: template _floor[{S.floor_template_idx[i]}] "
              f"postlist[1]={f['postlist'][1]} partitions={f['partitions']} "
              f"encode-only: n={f['n']} maxover={f['maxover']} maxunder={f['maxunder']} "
              f"maxerr={f['maxerr']} twofitweight={f['twofitweight']} twofitatten={f['twofitatten']}")
    for i in range(len(S.residues)):
        r = S.residues[i]
        p = r["partitions"]
        print(f"  residue{i}: type={S.residue_type[i]} base={S.residue_base[i]} "
              f"begin={r['begin']} end={r['end']} grouping={r['grouping']} "
              f"partitions={p} groupbook={r['groupbook']} "
              f"secondstages={r['secondstages'][:p]} "
              f"classmetric1={r['classmetric1'][:p]} classmetric2={r['classmetric2'][:p]}")
    for i, m in enumerate(S.maps):
        print(f"  mapping{i}: {m}")
    print(f"  modes: {S.modes}")


def stats(db):
    """Data volume of every book reachable from the 44.1/48 templates."""
    names = set()
    per_cfg = {}
    for ch in (1, 2):
        for qi in range(-1, 11):
            q = qi / 10.
            S = build(db, 44100, ch, q)
            names.update(S.books)
    ll = qv = 0
    for n in sorted(names):
        b = Book(n, deref(db, ("ref", n)), db)
        ll += b.entries
        if b.maptype:
            qv += len(b.quantlist)
    print(f"distinct books reachable (44.1/48 stereo+mono, q-1..q10): {len(names)}")
    print(f"  lengthlist entries total: {ll} (1 byte each)")
    print(f"  quantlist entries total: {qv}")


def main():
    db = load_db()
    cmd = sys.argv[1]
    if cmd == "gen":
        rate, ch, q, out = int(sys.argv[2]), int(sys.argv[3]), float(sys.argv[4]), sys.argv[5]
        _, blob = generate(db, rate, ch, q)
        open(out, "wb").write(blob)
        print(f"wrote {len(blob)} bytes to {out}")
    elif cmd == "describe":
        describe(db, int(sys.argv[2]), int(sys.argv[3]), float(sys.argv[4]))
    elif cmd == "check":
        d = sys.argv[2]
        ok = bad = 0
        for fn in sorted(os.listdir(d)):
            m = re.match(r"r(\d+)_c(\d)_q(.+)\.bin$", fn)
            if not m:
                continue
            rate, ch, q = int(m.group(1)), int(m.group(2)), float(m.group(3))
            want = open(os.path.join(d, fn), "rb").read()
            _, got = generate(db, rate, ch, q)
            if got == want:
                ok += 1
            else:
                bad += 1
                first = next((i for i in range(min(len(got), len(want)))
                              if got[i] != want[i]), min(len(got), len(want)))
                print(f"MISMATCH {fn}: got {len(got)} want {len(want)} first diff @ {first}")
        print(f"{ok} match, {bad} mismatch")
        sys.exit(1 if bad else 0)
    elif cmd == "stats":
        stats(db)
    else:
        print(__doc__)
        sys.exit(2)


if __name__ == "__main__":
    main()
