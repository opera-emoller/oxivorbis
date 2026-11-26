use std::io::Cursor;

use ogg::writing::{PacketWriteEndInfo, PacketWriter};

/// libogg `nfill` default — pages flush when the accumulated body in the
/// current page would exceed this size after the just-added packet, AND at
/// least 4 packets have been completed (libogg `ogg_stream_flush_i`).
const PAGE_NFILL: usize = 4096;

/// Maximum lacing values (segments) per Ogg page.
const MAX_PAGE_SEGMENTS: usize = 255;

pub struct OggStreamWriter {
    writer: PacketWriter<'static, Cursor<Vec<u8>>>,
    serial: u32,
    /// Lacing values (segments) in the current, not-yet-flushed page.
    page_segments: usize,
    /// Bytes of packet body accumulated in the current page (not yet flushed).
    page_body_bytes: usize,
    /// Packets whose final segment lies in the current page.
    page_packets_done: usize,
}

impl OggStreamWriter {
    pub fn new(serial: u32) -> Self {
        Self {
            writer: PacketWriter::new(Cursor::new(Vec::new())),
            serial,
            page_segments: 0,
            page_body_bytes: 0,
            page_packets_done: 0,
        }
    }

    pub fn write_packet(&mut self, bytes: &[u8], granule: u64, eos: bool, force_flush: bool) {
        // Mirror the page accounting of libogg's ogg_stream_flush_i. Walk
        // the packet's lacing values exactly as the `ogg` crate will place
        // them: whenever a page reaches 255 segments the crate emits it on
        // its own (splitting the packet if needed), which matches libogg's
        // `if(vals==255) force=1`. Our counters must restart at that point,
        // or the nfill rule below fires early on the following page.
        let n_segments = bytes.len() / 255 + 1;
        let mut ended_on_page_boundary = false;
        for i in 0..n_segments {
            let seg_len = if i + 1 < n_segments {
                255
            } else {
                bytes.len() % 255
            };
            self.page_segments += 1;
            self.page_body_bytes += seg_len;
            ended_on_page_boundary = false;
            if self.page_segments == MAX_PAGE_SEGMENTS {
                self.reset_page();
                ended_on_page_boundary = true;
            }
        }
        if !ended_on_page_boundary {
            self.page_packets_done += 1;
        }

        // libogg rule: after a packet completes, if accumulated body > nfill
        // AND at least 4 packets are done on this page, flush.
        let auto_flush = self.page_body_bytes > PAGE_NFILL && self.page_packets_done >= 4;

        let end_info = if eos {
            PacketWriteEndInfo::EndStream
        } else if force_flush || auto_flush {
            PacketWriteEndInfo::EndPage
        } else {
            PacketWriteEndInfo::NormalPacket
        };
        self.writer
            .write_packet(bytes.to_vec(), self.serial, end_info, granule)
            .expect("ogg write failed");

        if end_info != PacketWriteEndInfo::NormalPacket {
            self.reset_page();
        }
    }

    fn reset_page(&mut self) {
        self.page_segments = 0;
        self.page_body_bytes = 0;
        self.page_packets_done = 0;
    }

    /// Bytes of every page completed since the last [`Self::clear_output`].
    /// Pages still being filled stay inside the writer.
    pub fn output(&self) -> &[u8] {
        self.writer.inner().get_ref()
    }

    /// Drop the completed-page bytes (keeping the allocation) so the next
    /// [`Self::output`] only shows pages completed after this call.
    pub fn clear_output(&mut self) {
        let cursor = self.writer.inner_mut();
        cursor.get_mut().clear();
        cursor.set_position(0);
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.writer.into_inner().into_inner()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// (segment count, continued-packet flag) for every page in `bytes`.
    fn page_layout(bytes: &[u8]) -> Vec<(usize, bool)> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            assert_eq!(&bytes[i..i + 4], b"OggS");
            let n = bytes[i + 26] as usize;
            let body: usize = bytes[i + 27..i + 27 + n].iter().map(|&b| b as usize).sum();
            out.push((n, bytes[i + 5] & 1 != 0));
            i += 27 + n + body;
        }
        out
    }

    fn write_all(sizes: &[usize]) -> Vec<(usize, bool)> {
        let mut w = OggStreamWriter::new(1);
        for (k, &len) in sizes.iter().enumerate() {
            let last = k + 1 == sizes.len();
            w.write_packet(&vec![0xAA; len], k as u64, last, false);
        }
        page_layout(&w.into_bytes())
    }

    /// 16-byte packets: 255 of them total 4080 bytes (< nfill), so libogg
    /// force-flushes every page at 255 segments. Before the fix the
    /// counters carried over and page 2 was cut after 2 packets.
    #[test]
    fn small_packets_flush_at_255_segments_and_reset() {
        let pages = write_all(&[16; 600]);
        assert_eq!(pages, vec![(255, false), (255, false), (90, false)]);
    }

    /// 1-byte packets (Q5 digital silence): counters must not accumulate
    /// across forced 255-segment pages (would cut a page after ~4096
    /// cumulative packets).
    #[test]
    fn one_byte_packets_never_hit_nfill() {
        let pages = write_all(&[1; 255 * 20 + 7]);
        let mut expected = vec![(255, false); 20];
        expected.push((7, false));
        assert_eq!(pages, expected);
    }

    /// A packet split across the 255-segment boundary: the continuation
    /// bytes and the completed packet count toward the next page, as in
    /// libogg (acc / packets_done restart from the remaining lacing vals).
    #[test]
    fn split_packet_counts_toward_next_page() {
        // 254 x 1 byte, then a 1000-byte packet (segments 255,255,255,235):
        // page 1 = 254 + 1 segment, page 2 starts with 3 continuation
        // segments (745 bytes, 1 packet done). Then 1-byte packets: page 2
        // fills to 255 segments (745 + 252 bytes, never > nfill).
        let mut sizes = vec![1usize; 254];
        sizes.push(1000);
        sizes.extend(std::iter::repeat_n(1, 252 + 10));
        let pages = write_all(&sizes);
        assert_eq!(pages, vec![(255, false), (255, true), (10, false)]);
    }

    /// Continuation bytes count toward nfill: after a split, 3 x 1200-byte
    /// packets push acc over 4096 with 4 packets done (continued one + 3).
    #[test]
    fn split_packet_bytes_count_toward_nfill() {
        let mut sizes = vec![1usize; 254];
        sizes.push(1000); // 745 bytes spill into page 2
        sizes.extend([1200, 1200, 1200]); // page 2 acc = 4345 > 4096, 4 done
        sizes.extend([1, 1]);
        let pages = write_all(&sizes);
        // page 2: 3 continuation segs + 3 * 5 segs = 18, then page 3: 2.
        assert_eq!(pages, vec![(255, false), (18, true), (2, false)]);
    }
}
