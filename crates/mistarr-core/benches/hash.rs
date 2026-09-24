//! Benchmarks `hash_reader` over 64 MiB of deterministic bytes, with and
//! without the N64 byte-swap transform. See `docs/VERIFICATION.md` "Hashing".

use std::hint::black_box;
use std::io::Cursor;

use criterion::{criterion_group, criterion_main, Criterion};
use mistarr_core::hash::{hash_reader, HeaderRule};

const SIZE: usize = 64 * 1024 * 1024;

// Truncating casts are the point: a cheap, deterministic byte-filler pattern.
#[allow(clippy::cast_possible_truncation)]
fn deterministic_bytes(first_four: [u8; 4]) -> Vec<u8> {
    let mut buf = vec![0u8; SIZE];
    buf[..4].copy_from_slice(&first_four);
    for (i, b) in buf.iter_mut().enumerate().skip(4) {
        *b = (i as u32).wrapping_mul(2_654_435_761) as u8;
    }
    buf
}

fn bench_hash(c: &mut Criterion) {
    let plain = deterministic_bytes([0, 0, 0, 0]);
    let v64 = deterministic_bytes([0x37, 0x80, 0x40, 0x12]);

    c.bench_function("hash_reader 64MiB rule=None", |b| {
        b.iter(|| hash_reader(Cursor::new(black_box(&plain)), HeaderRule::None).unwrap());
    });

    c.bench_function("hash_reader 64MiB rule=N64", |b| {
        b.iter(|| hash_reader(Cursor::new(black_box(&v64)), HeaderRule::N64).unwrap());
    });
}

criterion_group!(benches, bench_hash);
criterion_main!(benches);
