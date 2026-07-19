#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use divan::{Bencher, black_box};
use nixdex_core::frcode::{Decoder, Encoder};

fn main() {
    divan::main();
}

fn make_paths(count: usize) -> Vec<Vec<u8>> {
    (0..count)
        .map(|i| format!("/nix/store/{i:032x}-pkg{i}/bin/cmd").into_bytes())
        .collect()
}

#[divan::bench(args = [100, 1_000, 10_000])]
fn encode(b: Bencher, count: usize) {
    let paths = make_paths(count);
    b.bench(|| {
        let mut out = Vec::new();
        let mut enc = Encoder::new(&mut out, b"p".to_vec(), b"{}".to_vec()).unwrap();
        for path in &paths {
            enc.write_meta(b"1r").unwrap();
            enc.write_path(path.clone()).unwrap();
        }
        enc.finish().unwrap();
        black_box(out);
    });
}

#[divan::bench(args = [100, 1_000, 10_000])]
fn decode(b: Bencher, count: usize) {
    let paths = make_paths(count);
    let mut encoded = Vec::new();
    {
        let mut enc = Encoder::new(&mut encoded, b"p".to_vec(), b"{}".to_vec()).unwrap();
        for path in &paths {
            enc.write_meta(b"1r").unwrap();
            enc.write_path(path.clone()).unwrap();
        }
        enc.finish().unwrap();
    }

    b.bench(|| {
        let mut dec = Decoder::new(std::io::Cursor::new(&encoded));
        let mut total = 0usize;
        loop {
            let block = dec.decode().unwrap();
            if block.is_empty() {
                break;
            }
            total = total.wrapping_add(block.len());
        }
        black_box(total);
    });
}
