//! gen-header-vectors: regenerates tests/vectors/headers/*.bin and
//! tests/vectors/ogg/* from the in-tree oracle-encoder (pristine libvorbis
//! 1.3.7 + libogg 1.3.5, see tools/oracle-encoder/build.sh).
//!
//! For each (rate x channels) combo it encodes 1024 frames of silence at
//! base_quality 0.5 with one user comment (`COMMENT` below) and stores:
//!   tests/vectors/headers/id_<combo>.bin       identification header packet
//!   tests/vectors/headers/comment_<combo>.bin  comment header packet
//!   tests/vectors/headers/setup.bin            setup header (mono44 only)
//!   tests/vectors/ogg/serial_<combo>.bin       stream serial (LE u32)
//!   tests/vectors/ogg/headers_<combo>.ogg      the two header pages
//!
//! Usage: cargo run -p gen-header-vectors   (or `just regen-header-vectors`)

use ogg::PacketReader;
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// User comment passed to the oracle (tests/headers.rs and
/// tests/ogg_headers.rs use the same string).
const COMMENT: &str = "encoder=oxivorbis header vectors";

const COMBOS: &[(&str, u32, u16)] = &[
    ("mono44", 44100, 1),
    ("mono48", 48000, 1),
    ("stereo44", 44100, 2),
    ("stereo48", 48000, 2),
];

fn ogg_page_size(data: &[u8], offset: usize) -> usize {
    assert_eq!(
        &data[offset..offset + 4],
        b"OggS",
        "expected OggS at offset {offset}"
    );
    let nseg = data[offset + 26] as usize;
    let body: usize = data[offset + 27..offset + 27 + nseg]
        .iter()
        .map(|&b| b as usize)
        .sum();
    27 + nseg + body
}

fn run_oracle(oracle: &Path, rate: u32, channels: u16) -> Vec<u8> {
    let silence = vec![0u8; 1024 * 2 * channels as usize];
    let mut child = Command::new(oracle)
        .args([
            &rate.to_string(),
            &channels.to_string(),
            "0.5",
            "s16",
            COMMENT,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| {
            panic!(
                "spawn {} (run tools/oracle-encoder/build.sh): {e}",
                oracle.display()
            )
        });
    child.stdin.take().unwrap().write_all(&silence).unwrap();
    let out = child.wait_with_output().expect("wait for oracle-encoder");
    assert!(
        out.status.success(),
        "oracle-encoder failed: {}",
        out.status
    );
    out.stdout
}

fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let root = root.canonicalize().unwrap_or(root);
    let oracle = root.join("tools/oracle-encoder/oracle-encoder");
    let vec_dir = root.join("tests/vectors/headers");
    let ogg_dir = root.join("tests/vectors/ogg");
    std::fs::create_dir_all(&vec_dir).unwrap();
    std::fs::create_dir_all(&ogg_dir).unwrap();

    for &(name, rate, ch) in COMBOS {
        let raw = run_oracle(&oracle, rate, ch);
        let serial = u32::from_le_bytes(raw[14..18].try_into().unwrap());
        let p0 = ogg_page_size(&raw, 0);
        let p1 = ogg_page_size(&raw, p0);
        let header_pages = &raw[..p0 + p1];

        let mut reader = PacketReader::new(Cursor::new(raw.clone()));
        let packets: Vec<Vec<u8>> = (0..3)
            .map(|_| {
                reader
                    .read_packet()
                    .expect("ogg read error")
                    .expect("stream ended before 3 header packets")
                    .data
            })
            .collect();
        for (i, ty) in [(0, 1u8), (1, 3), (2, 5)] {
            assert_eq!(packets[i][0], ty, "packet {i} type");
            assert_eq!(&packets[i][1..7], b"vorbis", "packet {i} sync");
        }

        let write = |path: PathBuf, bytes: &[u8]| {
            std::fs::write(&path, bytes).unwrap();
            println!("  {} bytes -> {}", bytes.len(), path.display());
        };
        println!("{name} (serial {serial:#010x}):");
        write(vec_dir.join(format!("id_{name}.bin")), &packets[0]);
        write(vec_dir.join(format!("comment_{name}.bin")), &packets[1]);
        if name == "mono44" {
            write(vec_dir.join("setup.bin"), &packets[2]);
        }
        write(
            ogg_dir.join(format!("serial_{name}.bin")),
            &serial.to_le_bytes(),
        );
        write(ogg_dir.join(format!("headers_{name}.ogg")), header_pages);
    }
}
