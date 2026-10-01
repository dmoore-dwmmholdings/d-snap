//! Deterministic inputs for the line-diff performance test and benchmark (DSNA-40).
//! Included with `#[path]` from `tests/diff_perf.rs` and `benches/line_diff.rs`.

/// Target size of the generated source file.
pub const SOURCE_BYTES: usize = 1024 * 1024;

/// xorshift64*: small, deterministic, good enough to scatter edits.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
}

/// About 1 MiB of source-like text: indented statements with varied, mostly unique lines.
pub fn source() -> Vec<u8> {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut out = String::with_capacity(SOURCE_BYTES + 128);
    let mut i = 0u64;
    while out.len() < SOURCE_BYTES {
        let indent = "    ".repeat((rng.next() % 4) as usize);
        let line = match i % 7 {
            0 => format!("{indent}fn function_{i}(arg: u64) -> u64 {{\n"),
            1 => format!(
                "{indent}let value_{i} = arg.wrapping_mul({});\n",
                rng.next() % 9973
            ),
            2 => format!(
                "{indent}// comment about step {i} and {}\n",
                rng.next() % 1000
            ),
            3 => format!(
                "{indent}if value_{i} > {} {{ return value_{i}; }}\n",
                rng.next() % 500
            ),
            4 => "\n".to_owned(),
            5 => format!(
                "{indent}call_something(\"string literal {i}\", {});\n",
                rng.next()
            ),
            _ => format!("{indent}}}\n"),
        };
        out.push_str(&line);
        i += 1;
    }
    out.into_bytes()
}

/// An edited copy of a file plus the size of the edit that made it.
pub struct Edited {
    /// New file content.
    pub bytes: Vec<u8>,
    /// Lines the edit added.
    pub added: u32,
    /// Lines the edit removed.
    pub removed: u32,
}

/// `source` with `percent`% of its lines changed at scattered positions: a mix of
/// modifications (most), deletions and insertions. Changed lines are unique.
pub fn modified(source: &[u8], percent: u32) -> Edited {
    edit(source, percent, |n, rng| {
        format!("changed line {n} {}\n", rng.next() % 1000)
    })
}

/// CSV-like data: `rows` lines drawn from `distinct` different values, so almost no line is
/// unique. About 6 bytes per row.
pub fn data_rows(rows: usize, distinct: u64) -> Vec<u8> {
    let mut rng = Rng(0x2545_F491_4F6C_DD1D ^ distinct);
    let mut out = String::with_capacity(rows * 8);
    for _ in 0..rows {
        let v = rng.next() % distinct;
        out.push_str(&format!("{},{}\n", v / 3, v % 3));
    }
    out.into_bytes()
}

/// `data_rows` output with `percent`% of rows edited; replacement rows repeat too.
pub fn data_modified(source: &[u8], percent: u32) -> Edited {
    edit(source, percent, |_, rng| {
        format!("{},x\n", rng.next() % 1000)
    })
}

fn edit(source: &[u8], percent: u32, mut changed: impl FnMut(usize, &mut Rng) -> String) -> Edited {
    let text = String::from_utf8_lossy(source);
    let mut rng = Rng(0xD1B5_4A32_D192_ED03 ^ u64::from(percent));
    let mut out = String::with_capacity(source.len() + source.len() / 10);
    let (mut added, mut removed) = (0, 0);
    for (n, line) in text.split_inclusive('\n').enumerate() {
        if rng.next() % 100 >= u64::from(percent) {
            out.push_str(line);
            continue;
        }
        match rng.next() % 10 {
            0 => removed += 1,
            1 => {
                out.push_str(line);
                out.push_str(&format!("inserted line after {n}\n"));
                added += 1;
            }
            _ => {
                out.push_str(&changed(n, &mut rng));
                (added, removed) = (added + 1, removed + 1);
            }
        }
    }
    Edited {
        bytes: out.into_bytes(),
        added,
        removed,
    }
}
