/* Generates lewtoff's setup-header template data from the vendored
 * libvorbis 1.3.7 sources:
 *
 *   src/setup_books.bin     every static codebook reachable from the
 *                           44.1/48 kHz stereo + uncoupled templates,
 *                           each packed with libvorbis's own
 *                           vorbis_staticbook_pack() and byte-aligned.
 *   src/setup_templates.rs  Rust consts: book index (offset/bits), floor1
 *                           templates, residue templates, mapping
 *                           templates, mode template and the two
 *                           ve_setup_data_template subsets.
 *
 * The template tables are read straight out of vorbisenc.c (which this
 * file #includes so its `static` tables are visible) -- nothing is
 * hand-transcribed.
 *
 * Usage: gen <out_dir>          (normally invoked via run.sh)
 */

#include "vorbisenc.c"

#include <assert.h>
#include <stdarg.h>
#include <stdlib.h>
#include <string.h>

/* ---------------------------------------------------------------- */
/* Pointer-identity registries (same identity libvorbis uses in       */
/* book_dup_or_new).                                                   */
/* ---------------------------------------------------------------- */

#define MAXN 1024

static const static_codebook *books[MAXN];
static int nbooks;
static int books_frozen; /* set once BOOKS[] has been emitted */
static int book_id(const static_codebook *b) {
  int i;
  assert(b);
  for (i = 0; i < nbooks; i++)
    if (books[i] == b) return i;
  assert(!books_frozen);
  assert(nbooks < MAXN);
  books[nbooks] = b;
  return nbooks++;
}

static const vorbis_info_residue0 *res0s[MAXN];
static int nres0;
static int res0_id(const vorbis_info_residue0 *r) {
  int i;
  for (i = 0; i < nres0; i++)
    if (res0s[i] == r) return i;
  res0s[nres0] = r;
  return nres0++;
}

static const static_bookblock *blocks[MAXN];
static int nblocks;
static int block_id(const static_bookblock *b) {
  int i;
  for (i = 0; i < nblocks; i++)
    if (blocks[i] == b) return i;
  blocks[nblocks] = b;
  return nblocks++;
}

static const vorbis_info_mapping0 *maps[MAXN];
static int nmaps;
static int map_id(const vorbis_info_mapping0 *m) {
  int i;
  for (i = 0; i < nmaps; i++)
    if (maps[i] == m) return i;
  maps[nmaps] = m;
  return nmaps++;
}

/* A residue "template set" is the vorbis_residue_template array that a
 * vorbis_mapping_template points at (e.g. _res_44s_5); its length is the
 * number of residue numbers referenced by its mapping's submaps. */
static const vorbis_residue_template *ressets[MAXN];
static int ressets_len[MAXN];
static int nressets;
static int resset_id(const vorbis_residue_template *r, int len) {
  int i;
  for (i = 0; i < nressets; i++)
    if (ressets[i] == r) {
      assert(ressets_len[i] == len);
      return i;
    }
  ressets[nressets] = r;
  ressets_len[nressets] = len;
  return nressets++;
}

#define NFLOORS ((int)(sizeof(_floor) / sizeof(_floor[0])))
static int floor_nbooks[NFLOORS]; /* 0 = unreachable */
static int floor_book_ids[NFLOORS][64];

/* Mirrors the maxbook computation in vorbis_encode_floor_setup. */
static int floor_maxbook(const vorbis_info_floor1 *f) {
  int i, k, maxclass = -1, maxbook = -1;
  for (i = 0; i < f->partitions; i++)
    if (f->partitionclass[i] > maxclass) maxclass = f->partitionclass[i];
  for (i = 0; i <= maxclass; i++) {
    if (f->class_book[i] > maxbook) maxbook = f->class_book[i];
    for (k = 0; k < (1 << f->class_subs[i]); k++)
      if (f->class_subbook[i][k] > maxbook) maxbook = f->class_subbook[i][k];
  }
  return maxbook;
}

static const ve_setup_data_template *const templates[] = {
    &ve_setup_44_stereo,
    &ve_setup_44_uncoupled,
};
static const char *const template_names[] = {
    "SETUP_44_STEREO",
    "SETUP_44_UNCOUPLED",
};
#define NTEMPLATES 2

/* Walk the templates in the same order libvorbis consumes them, so book
 * ids come out in a stable, meaningful order. */
static void walk(void) {
  int t, i, is, m, j, k;
  for (t = 0; t < NTEMPLATES; t++) {
    const ve_setup_data_template *s = templates[t];
    assert(s->floor_books == _floor_books);
    assert(s->floor_params == _floor);
    for (is = 0; is < s->mappings; is++) {
      for (i = 0; i < s->floor_mappings; i++) {
        int fi = s->floor_mapping_list[i][is];
        assert(fi >= 0 && fi < NFLOORS);
        if (!floor_nbooks[fi]) {
          int mb = floor_maxbook(&_floor[fi]);
          assert(mb >= 0 && mb < 64);
          for (k = 0; k <= mb; k++)
            floor_book_ids[fi][k] = book_id(_floor_books[fi][k]);
          floor_nbooks[fi] = mb + 1;
        }
      }
      {
        const vorbis_mapping_template *mt = &s->maps[is];
        int nres = 0;
        map_id(mt->map);
        for (m = 0; m < 2; m++)
          for (j = 0; j < mt->map[m].submaps; j++)
            if (mt->map[m].residuesubmap[j] + 1 > nres)
              nres = mt->map[m].residuesubmap[j] + 1;
        resset_id(mt->res, nres);
        for (j = 0; j < nres; j++) {
          const vorbis_residue_template *rt = &mt->res[j];
          const vorbis_info_residue0 *r0 = rt->res;
          /* vorbis_encode_residue_setup ORs into secondstages, so we
             rely on the templates carrying zeros there. booklist is
             overwritten from index 0 and only the populated prefix is
             packed/used, so its template value ({-1}) is irrelevant. */
          for (k = 0; k < 64; k++) assert(r0->secondstages[k] == 0);
          res0_id(r0);
          block_id(rt->books_base);
          book_id(rt->book_aux);
          for (i = 0; i < r0->partitions; i++)
            for (k = 0; k < 4; k++)
              if (rt->books_base->books[i][k]) book_id(rt->books_base->books[i][k]);
        }
      }
    }
  }
}

/* ---------------------------------------------------------------- */
/* Emission helpers                                                   */
/* ---------------------------------------------------------------- */

static FILE *out;

static void p(const char *fmt, ...) {
  va_list ap;
  va_start(ap, fmt);
  vfprintf(out, fmt, ap);
  va_end(ap);
}

/* Shortest decimal that round-trips, always with a '.' or exponent so
 * Rust parses it as a float literal. */
static void fmt_double(char *buf, double v) {
  int prec;
  if (v == (double)(long)v && v > -1e15 && v < 1e15) {
    snprintf(buf, 64, "%.1f", v);
    return;
  }
  for (prec = 1; prec <= 17; prec++) {
    snprintf(buf, 64, "%.*g", prec, v);
    if (strtod(buf, NULL) == v) break;
  }
  if (!strpbrk(buf, ".eEn")) strcat(buf, ".0");
}
static void fmt_float(char *buf, float v) {
  int prec;
  if (v == (float)(long)v && v > -1e7f && v < 1e7f) {
    snprintf(buf, 64, "%.1f", (double)v);
    return;
  }
  for (prec = 1; prec <= 9; prec++) {
    snprintf(buf, 64, "%.*g", prec, (double)v);
    if (strtof(buf, NULL) == v) break;
  }
  if (!strpbrk(buf, ".eEn")) strcat(buf, ".0");
}

/* int slice with trailing zeros trimmed (C zero-fills the remainder of
 * partially initialized arrays; the Rust side zero-pads back). */
static int trimmed_len(const int *v, int n) {
  while (n > 0 && v[n - 1] == 0) n--;
  return n;
}
static void int_slice(const int *v, int n) {
  int i;
  n = trimmed_len(v, n);
  p("&[");
  for (i = 0; i < n; i++) p(i ? ", %d" : "%d", v[i]);
  p("]");
}

static void emit_floor(int fi) {
  const vorbis_info_floor1 *f = &_floor[fi];
  char a[64], b[64], c[64], d[64], e[64];
  int i, nsub;
  p("    // _floor[%d]: postlist[1]=%d, %d partitions\n", fi, f->postlist[1], f->partitions);
  p("    Floor1Template {\n");
  p("        partitions: %d,\n", f->partitions);
  p("        partitionclass: ");
  int_slice(f->partitionclass, VIF_PARTS);
  p(",\n        class_dim: ");
  int_slice(f->class_dim, VIF_CLASS);
  p(",\n        class_subs: ");
  int_slice(f->class_subs, VIF_CLASS);
  p(",\n        class_book: ");
  int_slice(f->class_book, VIF_CLASS);
  p(",\n        class_subbook: &[");
  for (nsub = VIF_CLASS; nsub > 0 && trimmed_len(f->class_subbook[nsub - 1], 8) == 0; nsub--);
  for (i = 0; i < nsub; i++) {
    if (i) p(", ");
    int_slice(f->class_subbook[i], 8);
  }
  p("],\n        mult: %d,\n        postlist: ", f->mult);
  int_slice(f->postlist, VIF_POSIT + 2);
  fmt_float(a, f->maxover);
  fmt_float(b, f->maxunder);
  fmt_float(c, f->maxerr);
  fmt_float(d, f->twofitweight);
  fmt_float(e, f->twofitatten);
  p(",\n        maxover: %s,\n        maxunder: %s,\n        maxerr: %s,\n"
    "        twofitweight: %s,\n        twofitatten: %s,\n    },\n",
    a, b, c, d, e);
}

static void emit_map0(const vorbis_info_mapping0 *m) {
  p("        Mapping0Template {\n            submaps: %d,\n            chmuxlist: ", m->submaps);
  int_slice(m->chmuxlist, 256);
  p(",\n            floorsubmap: ");
  int_slice(m->floorsubmap, 16);
  p(",\n            residuesubmap: ");
  int_slice(m->residuesubmap, 16);
  p(",\n            coupling_steps: %d,\n            coupling_mag: ", m->coupling_steps);
  int_slice(m->coupling_mag, 256);
  p(",\n            coupling_ang: ");
  int_slice(m->coupling_ang, 256);
  p(",\n        },\n");
}

static void emit_doubles(const double *v, int n) {
  char buf[64];
  int i;
  p("&[");
  for (i = 0; i < n; i++) {
    fmt_double(buf, v[i]);
    p(i ? ", %s" : "%s", buf);
  }
  p("]");
}

static int map_index(const vorbis_info_mapping0 *m) {
  int i;
  for (i = 0; i < nmaps; i++)
    if (maps[i] == m) return i;
  abort();
}
static int resset_index(const vorbis_residue_template *r) {
  int i;
  for (i = 0; i < nressets; i++)
    if (ressets[i] == r) return i;
  abort();
}

int main(int argc, char **argv) {
  char path[4096];
  FILE *bin;
  long offset = 0;
  int i, j, k, t, is;

  if (argc != 2) {
    fprintf(stderr, "usage: %s <out_dir>\n", argv[0]);
    return 2;
  }

  walk();

  /* ---- packed books ---- */
  snprintf(path, sizeof path, "%s/setup_books.bin", argv[1]);
  bin = fopen(path, "wb");
  if (!bin) {
    perror(path);
    return 1;
  }
  snprintf(path, sizeof path, "%s/setup_templates.rs", argv[1]);
  out = fopen(path, "w");
  if (!out) {
    perror(path);
    return 1;
  }

  p("// @generated by tools/gen-setup-templates (just regen-setup-templates).\n");
  p("// DO NOT EDIT. Source: vendored libvorbis 1.3.7 vorbisenc.c +\n");
  p("// modes/{setup_44,setup_44u,floor_all,residue_44,residue_44u}.h and\n");
  p("// books/*. Codebooks are packed by libvorbis's vorbis_staticbook_pack.\n");
  p("// Integer arrays are emitted with trailing zeros trimmed; consumers\n");
  p("// zero-pad back to the C array length (C zero-fills partial\n");
  p("// initializers).\n\n");
  p("use crate::setup_gen::{\n"
    "    Floor1Template, MapResTemplate, Mapping0Template, ModeTemplate, NB,\n"
    "    Residue0Template, ResidueTemplate, SetupTemplate,\n"
    "};\n\n");
  p("/// Every reachable static codebook, each packed by\n"
    "/// `vorbis_staticbook_pack` and padded to a byte boundary.\n");
  p("pub(crate) static BOOKS_PACKED: &[u8] = include_bytes!(\"setup_books.bin\");\n\n");
  p("/// Book id -> (byte offset into BOOKS_PACKED, packed length in bits).\n");
  p("pub(crate) static BOOKS: [(u32, u32); %d] = [\n", nbooks);
  for (i = 0; i < nbooks; i++) {
    oggpack_buffer opb;
    long bits, bytes;
    oggpack_writeinit(&opb);
    if (vorbis_staticbook_pack(books[i], &opb)) {
      fprintf(stderr, "pack failed for book %d\n", i);
      return 1;
    }
    bits = oggpack_bits(&opb);
    bytes = oggpack_bytes(&opb);
    fwrite(oggpack_get_buffer(&opb), 1, bytes, bin);
    p("    (%ld, %ld), // %d: dim %ld, entries %ld, maptype %d\n",
      offset, bits, i, books[i]->dim, books[i]->entries, books[i]->maptype);
    offset += bytes;
    oggpack_writeclear(&opb);
  }
  p("];\n\n");
  fclose(bin);
  books_frozen = 1;

  /* ---- floors ---- */
  p("/// `_floor[]` from modes/floor_all.h (all entries, reachable or not).\n");
  p("pub(crate) static FLOORS: [Floor1Template; %d] = [\n", NFLOORS);
  for (i = 0; i < NFLOORS; i++) emit_floor(i);
  p("];\n\n");
  p("/// `_floor_books[fi][0..=maxbook]` as book ids; empty for floors no\n"
    "/// 44.1/48 kHz template reaches.\n");
  p("pub(crate) static FLOOR_BOOKS: [&[u16]; %d] = [\n", NFLOORS);
  for (i = 0; i < NFLOORS; i++) {
    p("    &[");
    for (k = 0; k < floor_nbooks[i]; k++) p(k ? ", %d" : "%d", floor_book_ids[i][k]);
    p("],\n");
  }
  p("];\n\n");

  /* ---- residue0 bases ---- */
  p("/// `vorbis_info_residue0` bases (e.g. `_residue_44_mid`).\n");
  p("pub(crate) static RESIDUE0_BASES: [Residue0Template; %d] = [\n", nres0);
  for (i = 0; i < nres0; i++) {
    const vorbis_info_residue0 *r = res0s[i];
    p("    Residue0Template {\n        begin: %ld,\n        end: %ld,\n        grouping: %d,\n"
      "        partitions: %d,\n        partvals: %d,\n        groupbook: %d,\n"
      "        classmetric1: ",
      r->begin, r->end, r->grouping, r->partitions, r->partvals, r->groupbook);
    int_slice(r->classmetric1, 64);
    p(",\n        classmetric2: ");
    int_slice(r->classmetric2, 64);
    p(",\n    },\n");
  }
  p("];\n\n");

  /* ---- bookblocks ---- */
  p("/// `static_bookblock` contents as book ids (`NB` = NULL).\n");
  p("pub(crate) static BOOKBLOCKS: [[[u16; 4]; 12]; %d] = [\n", nblocks);
  for (i = 0; i < nblocks; i++) {
    p("    [\n");
    for (j = 0; j < 12; j++) {
      p("        [");
      for (k = 0; k < 4; k++) {
        const static_codebook *b = blocks[i]->books[j][k];
        if (k) p(", ");
        if (b)
          p("%d", book_id(b));
        else
          p("NB");
      }
      p("],\n");
    }
    p("    ],\n");
  }
  p("];\n\n");

  /* ---- residue template sets ---- */
  p("/// `vorbis_residue_template` arrays (e.g. `_res_44s_5`), unmanaged\n"
    "/// fields only; indices into RESIDUE0_BASES / BOOKS / BOOKBLOCKS.\n");
  p("pub(crate) static RESIDUE_SETS: [&[ResidueTemplate]; %d] = [\n", nressets);
  for (i = 0; i < nressets; i++) {
    p("    &[\n");
    for (j = 0; j < ressets_len[i]; j++) {
      const vorbis_residue_template *rt = &ressets[i][j];
      p("        ResidueTemplate {\n            res_type: %d,\n            limit_type: %d,\n"
        "            grouping: %d,\n            res: %d,\n            book_aux: %d,\n"
        "            books_base: %d,\n        },\n",
        rt->res_type, rt->limit_type, rt->grouping, res0_id(rt->res), book_id(rt->book_aux),
        block_id(rt->books_base));
    }
    p("    ],\n");
  }
  p("];\n\n");

  /* ---- mapping templates ---- */
  p("/// `vorbis_info_mapping0[2]` templates (`_map_nominal`, `_map_nominal_u`).\n");
  p("pub(crate) static MAPPINGS: [[Mapping0Template; 2]; %d] = [\n", nmaps);
  for (i = 0; i < nmaps; i++) {
    p("    [\n");
    emit_map0(&maps[i][0]);
    emit_map0(&maps[i][1]);
    p("    ],\n");
  }
  p("];\n\n");

  p("/// vorbisenc.c `_mode_template`.\n");
  p("pub(crate) static MODE_TEMPLATE: [ModeTemplate; 2] = [\n");
  for (i = 0; i < 2; i++)
    p("    ModeTemplate {\n        blockflag: %d,\n        windowtype: %d,\n"
      "        transformtype: %d,\n        mapping: %d,\n    },\n",
      _mode_template[i].blockflag, _mode_template[i].windowtype,
      _mode_template[i].transformtype, _mode_template[i].mapping);
  p("];\n");

  /* ---- setup templates ---- */
  for (t = 0; t < NTEMPLATES; t++) {
    const ve_setup_data_template *s = templates[t];
    p("\npub(crate) static %s: SetupTemplate = SetupTemplate {\n", template_names[t]);
    p("    mappings: %d,\n    quality_mapping: ", s->mappings);
    emit_doubles(s->quality_mapping, s->mappings + 1);
    p(",\n    coupling_restriction: %d,\n    samplerate_min_restriction: %ld,\n"
      "    samplerate_max_restriction: %ld,\n    blocksize_short: ",
      s->coupling_restriction, s->samplerate_min_restriction, s->samplerate_max_restriction);
    p("&[");
    for (i = 0; i < s->mappings; i++) p(i ? ", %d" : "%d", s->blocksize_short[i]);
    p("],\n    blocksize_long: &[");
    for (i = 0; i < s->mappings; i++) p(i ? ", %d" : "%d", s->blocksize_long[i]);
    p("],\n    psy_lowpass: ");
    emit_doubles(s->psy_lowpass, s->mappings + 1);
    p(",\n    floor_mapping_list: &[\n");
    for (i = 0; i < s->floor_mappings; i++) {
      p("        &[");
      for (is = 0; is < s->mappings; is++) p(is ? ", %d" : "%d", s->floor_mapping_list[i][is]);
      p("],\n");
    }
    p("    ],\n    maps: &[\n");
    for (is = 0; is < s->mappings; is++)
      p("        MapResTemplate { map: %d, res: %d },\n",
        map_index(s->maps[is].map), resset_index(s->maps[is].res));
    p("    ],\n};\n");
  }

  fclose(out);
  fprintf(stderr, "gen-setup-templates: %d books (%ld bytes packed), %d floors, %d residue bases, "
                  "%d bookblocks, %d residue sets, %d mappings\n",
          nbooks, offset, NFLOORS, nres0, nblocks, nressets, nmaps);
  return 0;
}
