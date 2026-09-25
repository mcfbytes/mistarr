//! Measures CHD track decoding: `write <out.chd> <MB>` makes a synthetic image with chdman's
//! default codecs; `decode <in.chd>` times the decoder and the hash passes it runs.

use std::io::BufWriter;
use std::time::Instant;

use md5::Digest as _;
use mistarr_core::chd::{self, Decoder, Step};
use mistarr_fixture::chd::{write, Kind, Spec, TrackSpec};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("write") if args.len() == 4 => {
            let frames = u32::try_from(args[3].parse::<u64>()? * 1_000_000 / 2448)?;
            let half = frames / 2;
            let spec = Spec::new(
                "Synthetic Bench Disc",
                vec![
                    TrackSpec {
                        kind: Kind::Mode1Raw,
                        frames: half,
                        pregap: 0,
                        pregap_stored: true,
                    },
                    TrackSpec {
                        kind: Kind::Audio,
                        frames: half,
                        pregap: 150,
                        pregap_stored: true,
                    },
                ],
            );
            let out = BufWriter::new(std::fs::File::create(&args[2])?);
            let w = write(&spec, out)?;
            println!("wrote {} bytes, codecs {:?}", w.size, w.codec_use);
        }
        Some("decode") if args.len() == 3 => decode(&args[2])?,
        _ => anyhow::bail!("usage: chd_bench write <out.chd> <MB> | decode <in.chd>"),
    }
    Ok(())
}

fn rate(bytes: u64, start: Instant) -> String {
    let secs = start.elapsed().as_secs_f64();
    format!("{secs:.2} s, {:.1} MB/s", bytes as f64 / secs / 1e6)
}

fn decode(path: &str) -> anyhow::Result<()> {
    let h = chd::read_header(std::fs::File::open(path)?)?;
    let logical = h.logical_bytes;
    let mut f = std::fs::File::open(path)?;
    let layout = chd::read_layout(&mut f, &h)?;
    let start = Instant::now();
    let mut d = Decoder::new(f, h, layout)?;
    while d.step(32)? != Step::Done {}
    let tracks = d.finish()?.len();
    println!("decode {tracks} tracks: {}", rate(logical, start));

    let buf = vec![0x5au8; 1 << 20];
    let chunks = logical.div_ceil(1 << 20);
    let start = Instant::now();
    let mut s = sha1::Sha1::new();
    for _ in 0..chunks {
        s.update(&buf);
    }
    let _ = s.finalize();
    println!("one SHA1 pass: {}", rate(chunks << 20, start));
    let start = Instant::now();
    let mut m = md5::Md5::new();
    for _ in 0..chunks {
        m.update(&buf);
    }
    let _ = m.finalize();
    println!("one MD5 pass: {}", rate(chunks << 20, start));
    let start = Instant::now();
    let mut c = crc32fast::Hasher::new();
    for _ in 0..chunks {
        c.update(&buf);
    }
    let _ = c.finalize();
    println!("one CRC32 pass: {}", rate(chunks << 20, start));
    Ok(())
}
