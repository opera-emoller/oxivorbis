//! parity-diff: structured diff of two Ogg Vorbis files at page and packet
//! level.
//!
//! Usage: parity-diff [--ignore-serial] <lewtoff.ogg> <oracle.ogg>
//!
//! Both files are parsed strictly (shares tools/parity-fuzz/src/oggdiff.rs).
//! Every page's CRC is verified, then the two streams are compared page by
//! page (version, header-type flags, granule position, serial, sequence
//! number, CRC, lacing values, body bytes) and packet by packet (payload).
//! `--ignore-serial` skips the serial and, because the CRC covers the
//! serial, the CRC equality check (each page's own CRC is still verified).
//!
//! Exit status: 0 when the files are identical (modulo the serial with
//! `--ignore-serial`), 1 on any difference, 2 on usage, I/O or parse errors.

#[path = "../../parity-fuzz/src/oggdiff.rs"]
#[allow(dead_code)]
mod oggdiff;

use oggdiff::{Page, crc_of_page, packets, parse_pages};

fn usage() -> ! {
    eprintln!("usage: parity-diff [--ignore-serial] <lewtoff.ogg> <oracle.ogg>");
    std::process::exit(2)
}

fn read(path: &str) -> Vec<u8> {
    std::fs::read(path).unwrap_or_else(|e| {
        eprintln!("parity-diff: cannot read {path}: {e}");
        std::process::exit(2)
    })
}

fn packet_label(idx: usize) -> &'static str {
    match idx {
        0 => "id header",
        1 => "comment header",
        2 => "setup header",
        _ => "audio",
    }
}

fn hex_ctx(b: &[u8], at: usize) -> String {
    let lo = at.saturating_sub(8).min(b.len());
    let hi = (at + 16).min(b.len());
    format!("[{lo}..{hi}] {:02x?}", &b[lo..hi])
}

/// Field-level differences between two page headers.
fn header_diffs(a: &Page, b: &Page, ignore_serial: bool) -> Vec<String> {
    let mut d = Vec::new();
    let mut f = |name: &str, x: String, y: String| {
        if x != y {
            d.push(format!("{name}: lewtoff {x} | oracle {y}"));
        }
    };
    f("version", a.version.to_string(), b.version.to_string());
    f(
        "flags",
        format!("{:#04x}", a.flags),
        format!("{:#04x}", b.flags),
    );
    f("granule", a.granule.to_string(), b.granule.to_string());
    if !ignore_serial {
        f(
            "serial",
            format!("{:#010x}", a.serial),
            format!("{:#010x}", b.serial),
        );
        f(
            "crc",
            format!("{:#010x}", a.crc),
            format!("{:#010x}", b.crc),
        );
    }
    f("seqno", a.seqno.to_string(), b.seqno.to_string());
    f(
        "segments",
        a.lacing.len().to_string(),
        b.lacing.len().to_string(),
    );
    if a.lacing.len() == b.lacing.len() && a.lacing != b.lacing {
        let i = a.lacing.iter().zip(&b.lacing).position(|(x, y)| x != y);
        d.push(format!("lacing differs at segment {}", i.unwrap()));
    }
    d
}

fn main() {
    let mut ignore_serial = false;
    let mut files = Vec::new();
    for a in std::env::args().skip(1) {
        match a.as_str() {
            "--ignore-serial" => ignore_serial = true,
            "-h" | "--help" => usage(),
            s if s.starts_with("--") => usage(),
            _ => files.push(a),
        }
    }
    if files.len() != 2 {
        usage();
    }
    let lw = read(&files[0]);
    let or = read(&files[1]);
    println!("lewtoff: {} bytes\noracle:  {} bytes", lw.len(), or.len());

    let parse = |name: &str, b: &[u8]| -> Vec<Page> {
        parse_pages(b).unwrap_or_else(|e| {
            eprintln!("parity-diff: {name}: {e}");
            std::process::exit(2)
        })
    };
    let lp = parse("lewtoff", &lw);
    let op = parse("oracle", &or);

    let mut differ = false;
    let mut errors = false;

    // Each page's CRC must be valid on its own.
    for (name, b, pages) in [("lewtoff", &lw, &lp), ("oracle", &or, &op)] {
        for (i, p) in pages.iter().enumerate() {
            let want = crc_of_page(b, p);
            if want != p.crc {
                println!(
                    "{name}: page {i} has a bad CRC (stored {:#010x}, computed {want:#010x})",
                    p.crc
                );
                errors = true;
            }
        }
    }

    // Page-level comparison.
    println!("pages: lewtoff {} oracle {}", lp.len(), op.len());
    if lp.len() != op.len() {
        differ = true;
    }
    let mut first_page_diff = None;
    for (i, (a, b)) in lp.iter().zip(&op).enumerate() {
        let mut d = header_diffs(a, b, ignore_serial);
        let ab = &lw[a.offset + a.header_len..a.offset + a.header_len + a.body_len];
        let bb = &or[b.offset + b.header_len..b.offset + b.header_len + b.body_len];
        if ab != bb {
            let k = oggdiff::first_diff(ab, bb);
            d.push(format!(
                "body differs at byte {k} (lewtoff {} bytes, oracle {} bytes)",
                ab.len(),
                bb.len()
            ));
        }
        if !d.is_empty() {
            differ = true;
            if first_page_diff.is_none() {
                first_page_diff = Some(i);
                println!(
                    "page {i} differs (lewtoff offset {}, oracle offset {}):",
                    a.offset, b.offset
                );
                for line in &d {
                    println!("  {line}");
                }
            }
        }
    }
    if first_page_diff.is_none() && lp.len() == op.len() {
        println!(
            "all {} pages match{}",
            lp.len(),
            if ignore_serial {
                " (serial/CRC ignored)"
            } else {
                ""
            }
        );
    } else if first_page_diff.is_none() {
        println!(
            "first {} pages match; page count differs",
            lp.len().min(op.len())
        );
    }

    // Packet-level comparison.
    let lpk = packets(&lw, &lp);
    let opk = packets(&or, &op);
    println!("packets: lewtoff {} oracle {}", lpk.len(), opk.len());
    let n = lpk.len().max(opk.len());
    let mut first_pkt = None;
    for i in 0..n {
        let label = packet_label(i);
        match (lpk.get(i), opk.get(i)) {
            (Some(a), Some(b)) if a.data == b.data => {}
            (Some(a), Some(b)) => {
                let k = oggdiff::first_diff(&a.data, &b.data);
                println!(
                    "packet {i} ({label}): DIVERGES at byte {k} (lewtoff {} bytes, oracle {} bytes; pages lewtoff {}..{} oracle {}..{})",
                    a.data.len(),
                    b.data.len(),
                    a.first_page,
                    a.last_page,
                    b.first_page,
                    b.last_page
                );
                println!("  lewtoff {}", hex_ctx(&a.data, k));
                println!("  oracle  {}", hex_ctx(&b.data, k));
                first_pkt = Some(i);
            }
            (None, Some(b)) => {
                println!(
                    "packet {i} ({label}): MISSING in lewtoff ({} bytes in oracle)",
                    b.data.len()
                );
                first_pkt = Some(i);
            }
            (Some(a), None) => {
                println!(
                    "packet {i} ({label}): EXTRA in lewtoff ({} bytes)",
                    a.data.len()
                );
                first_pkt = Some(i);
            }
            (None, None) => unreachable!(),
        }
        if first_pkt.is_some() {
            differ = true;
            break;
        }
    }
    if first_pkt.is_none() {
        println!("all {n} packet payloads match");
    }

    // Raw byte comparison (catches anything the structured views miss).
    let raw_equal = if ignore_serial {
        lp.len() == op.len()
            && lw.len() == or.len()
            && lp.iter().zip(&op).all(|(a, b)| {
                let mask = |buf: &[u8], p: &Page| {
                    let mut v = buf[p.offset..p.offset + p.header_len + p.body_len].to_vec();
                    v[14..18].fill(0);
                    v[22..26].fill(0);
                    v
                };
                mask(&lw, a) == mask(&or, b)
            })
    } else {
        lw == or
    };
    if !raw_equal {
        differ = true;
        if !ignore_serial {
            let k = oggdiff::first_diff(&lw, &or);
            println!(
                "raw bytes: first difference at byte {k}: lewtoff {}; oracle {}",
                oggdiff::locate(&lw, &lp, k),
                oggdiff::locate(&or, &op, k)
            );
        }
    }

    if errors {
        std::process::exit(2);
    }
    if differ {
        println!("RESULT: files differ");
        std::process::exit(1);
    }
    println!("RESULT: identical");
}
