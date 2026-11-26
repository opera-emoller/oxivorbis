//! Minimal Ogg page/packet parser for localizing the first divergence.

#[derive(Debug, Clone)]
pub struct Page {
    pub offset: usize,
    pub header_len: usize,
    pub body_len: usize,
    pub flags: u8,
    pub granule: i64,
    pub seqno: u32,
    pub lacing: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct Packet {
    pub data: Vec<u8>,
    pub first_page: usize,
    pub last_page: usize,
}

pub fn parse_pages(b: &[u8]) -> Result<Vec<Page>, String> {
    let mut pages = Vec::new();
    let mut off = 0;
    while off < b.len() {
        if b.len() - off < 27 || &b[off..off + 4] != b"OggS" {
            return Err(format!("bad page capture at byte {off}"));
        }
        let nseg = b[off + 26] as usize;
        if b.len() - off < 27 + nseg {
            return Err(format!("truncated segment table at byte {off}"));
        }
        let lacing = b[off + 27..off + 27 + nseg].to_vec();
        let body_len: usize = lacing.iter().map(|&x| x as usize).sum();
        let header_len = 27 + nseg;
        if b.len() - off < header_len + body_len {
            return Err(format!("truncated page body at byte {off}"));
        }
        pages.push(Page {
            offset: off,
            header_len,
            body_len,
            flags: b[off + 5],
            granule: i64::from_le_bytes(b[off + 6..off + 14].try_into().unwrap()),
            seqno: u32::from_le_bytes(b[off + 18..off + 22].try_into().unwrap()),
            lacing,
        });
        off += header_len + body_len;
    }
    Ok(pages)
}

pub fn packets(b: &[u8], pages: &[Page]) -> Vec<Packet> {
    let mut out = Vec::new();
    let mut cur: Vec<u8> = Vec::new();
    let mut cur_first: Option<usize> = None;
    for (pi, p) in pages.iter().enumerate() {
        let mut pos = p.offset + p.header_len;
        for &l in &p.lacing {
            if cur_first.is_none() {
                cur_first = Some(pi);
            }
            cur.extend_from_slice(&b[pos..pos + l as usize]);
            pos += l as usize;
            if l < 255 {
                out.push(Packet {
                    data: std::mem::take(&mut cur),
                    first_page: cur_first.take().unwrap(),
                    last_page: pi,
                });
            }
        }
    }
    out
}

/// Location of a byte offset within a parsed stream.
pub fn locate(b: &[u8], pages: &[Page], off: usize) -> String {
    let mut pkt_idx = 0usize;
    for (pi, p) in pages.iter().enumerate() {
        let end = p.offset + p.header_len + p.body_len;
        if off < p.offset + p.header_len && off >= p.offset {
            let rel = off - p.offset;
            let field = match rel {
                0..=3 => "capture",
                4 => "version",
                5 => "header_type flags",
                6..=13 => "granule_pos",
                14..=17 => "serial",
                18..=21 => "page_seqno",
                22..=25 => "crc",
                26 => "segment count",
                _ => "segment table",
            };
            return format!(
                "page {pi} header byte {rel} ({field}); page granule={} seqno={}",
                p.granule, p.seqno
            );
        }
        if off < end && off >= p.offset {
            let mut pos = p.offset + p.header_len;
            for &l in &p.lacing {
                let l = l as usize;
                if off < pos + l {
                    return format!(
                        "page {pi} body, packet {pkt_idx} (byte {} within this page's chunk of it)",
                        off - pos
                    );
                }
                pos += l;
                if l < 255 {
                    pkt_idx += 1;
                }
            }
        }
        for &l in &p.lacing {
            if l < 255 {
                pkt_idx += 1;
            }
        }
    }
    let _ = b;
    format!("past end of stream (len {})", b.len())
}

pub fn first_diff(a: &[u8], b: &[u8]) -> usize {
    a.iter()
        .zip(b.iter())
        .position(|(x, y)| x != y)
        .unwrap_or(a.len().min(b.len()))
}

/// Describe an audio packet's mode: bit 0 = packet type, bit 1 = mode number
/// (Q5 setups have two modes: 0 = short block, 1 = long block).
fn describe_audio(pkt: &[u8]) -> String {
    match pkt.first() {
        None => "empty packet".into(),
        Some(&b0) if b0 & 1 != 0 => format!("non-audio packet (type byte {b0:#04x})"),
        Some(&b0) => {
            let mode = (b0 >> 1) & 1;
            format!(
                "mode {mode} ({}), {} bytes",
                if mode == 1 { "long" } else { "short" },
                pkt.len()
            )
        }
    }
}

pub struct DiffReport {
    pub first_byte: usize,
    pub lewtoff_len: usize,
    pub oracle_len: usize,
    pub location: String,
    pub first_packet: Option<usize>,
    pub block_index: Option<usize>,
    /// Coarse position class used to bucket failures: header / start /
    /// final / mid / framing / unparseable.
    pub position: &'static str,
    pub summary: String,
}

pub fn analyze(lw: &[u8], or: &[u8]) -> DiffReport {
    let first_byte = first_diff(lw, or);
    let mut report = DiffReport {
        first_byte,
        lewtoff_len: lw.len(),
        oracle_len: or.len(),
        location: String::new(),
        first_packet: None,
        block_index: None,
        position: "unparseable",
        summary: String::new(),
    };
    let (op, lp) = match (parse_pages(or), parse_pages(lw)) {
        (Ok(o), Ok(l)) => (o, l),
        (o, l) => {
            report.location = format!(
                "unparseable stream (oracle: {:?}, lewtoff: {:?})",
                o.err(),
                l.err()
            );
            return report;
        }
    };
    report.location = format!(
        "oracle: {}; lewtoff: {}",
        locate(or, &op, first_byte),
        locate(lw, &lp, first_byte)
    );
    let opk = packets(or, &op);
    let lpk = packets(lw, &lp);
    let mut s = format!(
        "pages: oracle {} lewtoff {}; packets: oracle {} lewtoff {}",
        op.len(),
        lp.len(),
        opk.len(),
        lpk.len()
    );
    let n = opk.len().min(lpk.len());
    let first_pkt = (0..n)
        .find(|&i| opk[i].data != lpk[i].data)
        .or(if opk.len() != lpk.len() {
            Some(n)
        } else {
            None
        });
    if let Some(i) = first_pkt {
        report.first_packet = Some(i);
        if i >= 3 {
            report.block_index = Some(i - 3);
        }
        let last = opk.len().max(lpk.len()).saturating_sub(1);
        report.position = if i < 3 {
            "header"
        } else if i >= last.saturating_sub(1) {
            "final"
        } else if i <= 5 {
            "start"
        } else {
            "mid"
        };
        let od = opk.get(i).map(|p| describe_audio(&p.data));
        let ld = lpk.get(i).map(|p| describe_audio(&p.data));
        s += &format!(
            "\nfirst differing packet: #{i}{} oracle=[{}] lewtoff=[{}]",
            if i >= 3 {
                format!(" (audio block {})", i - 3)
            } else {
                " (header)".into()
            },
            od.unwrap_or_else(|| "missing".into()),
            ld.unwrap_or_else(|| "missing".into()),
        );
        if let (Some(o), Some(l)) = (opk.get(i), lpk.get(i)) {
            let d = first_diff(&o.data, &l.data);
            s += &format!(
                "\n  first differing byte within packet: {d} (bit ~{}); pages oracle {}..{} lewtoff {}..{}",
                d * 8,
                o.first_page,
                o.last_page,
                l.first_page,
                l.last_page
            );
        }
        // Mode sequence around the divergence, useful for spotting block-size
        // decision mismatches.
        let modes = |pk: &[Packet]| -> String {
            let lo = i.saturating_sub(4).max(3);
            let hi = (i + 4).min(pk.len());
            (lo..hi)
                .map(|k| match pk[k].data.first() {
                    Some(b) if b & 1 == 0 => {
                        if (b >> 1) & 1 == 1 {
                            'L'
                        } else {
                            's'
                        }
                    }
                    _ => '?',
                })
                .collect()
        };
        s += &format!(
            "\n  block modes around it (from packet {}): oracle {} lewtoff {}",
            i.saturating_sub(4).max(3),
            modes(&opk),
            modes(&lpk)
        );
    } else {
        // Packets identical: framing difference (granule, flags, page split).
        report.position = "framing";
        let np = op.len().min(lp.len());
        let first_page = (0..np).find(|&k| {
            let (a, b) = (&op[k], &lp[k]);
            a.granule != b.granule || a.flags != b.flags || a.lacing != b.lacing
        });
        match first_page {
            Some(k) => {
                s += &format!(
                    "\npackets identical; framing differs at page {k}: oracle granule={} flags={:#x} segs={} | lewtoff granule={} flags={:#x} segs={}",
                    op[k].granule,
                    op[k].flags,
                    op[k].lacing.len(),
                    lp[k].granule,
                    lp[k].flags,
                    lp[k].lacing.len()
                )
            }
            None => s += "\npackets identical; page count differs",
        }
    }
    report.summary = s;
    report
}

pub fn extract_serial(b: &[u8]) -> Option<u32> {
    b.get(14..18)
        .map(|s| u32::from_le_bytes(s.try_into().unwrap()))
}

/// (vendor, first user comment) from the comment header (packet 1).
pub fn extract_comment_strings(b: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    let pages = parse_pages(b).ok()?;
    let pk = packets(b, &pages);
    let p = &pk.get(1)?.data;
    let rd = |o: usize| -> Option<usize> {
        Some(u32::from_le_bytes(p.get(o..o + 4)?.try_into().unwrap()) as usize)
    };
    let mut off = 7;
    let vlen = rd(off)?;
    off += 4;
    let vendor = p.get(off..off + vlen)?.to_vec();
    off += vlen;
    let count = rd(off)?;
    off += 4;
    let tag = if count > 0 {
        let clen = rd(off)?;
        off += 4;
        p.get(off..off + clen)?.to_vec()
    } else {
        Vec::new()
    };
    Some((vendor, tag))
}
