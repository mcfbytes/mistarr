//! Builds one MRA `<rom>` the way MiSTer's loader does; see `docs/PLATFORMS.md` "MRA assembly".

use std::io::{self, Read};

use mistarr_core::hash::Md5Stream;

use super::mra::{Interleave, MraRom, Part, Patch, RomItem};
use crate::{Error, Result};

/// Largest rom the assembler builds or hashes, a guard against runaway `repeat` values.
pub const MAX_ROM_BYTES: u64 = 512 * 1024 * 1024;

/// Largest `repeat` a part may carry; MRAs use small counts to fill padding.
pub const MAX_REPEAT: u64 = 4096;

/// Bytes read at a time when [`md5`] streams a part from its zip.
const STREAM_CHUNK: usize = 64 * 1024;

/// Where named parts are read from.
pub trait PartSource {
    /// Opens member `name` of zip `zip` (an MRA zip name, e.g. `exblast.zip`), or, when no
    /// member has that name, the member whose CRC32 is `crc`. `Ok(None)` when either is absent.
    ///
    /// # Errors
    ///
    /// An I/O error when the zip exists but cannot be read.
    fn open(
        &mut self,
        zip: &str,
        name: &str,
        crc: Option<u32>,
    ) -> io::Result<Option<Box<dyn Read + '_>>>;
}

/// An assembled rom and its MiSTer MD5.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assembled {
    /// The bytes MiSTer would send to the core.
    pub data: Vec<u8>,
    /// MD5 of the part bytes in document order, before interleaving and without patches,
    /// which is what MiSTer compares with the `md5` attribute.
    pub md5: String,
}

/// Builds `rom` in memory from `src`.
///
/// # Errors
///
/// [`Error::MraUnsupported`] for content outside the implemented subset, [`Error::MissingPart`]
/// when a named part is in none of its zips, [`Error::Io`] when a zip cannot be read.
///
/// ```
/// use mistarr_mister::adapter::arcade::{assemble, mra};
/// let rom = &mra::parse(b"<m><rom><part>01 02</part></rom></m>").unwrap().roms[0];
/// let out = assemble::assemble(rom, &mut assemble::NoParts).unwrap();
/// assert_eq!(out.data, [1, 2]);
/// ```
pub fn assemble(rom: &MraRom, src: &mut dyn PartSource) -> Result<Assembled> {
    let mut w = Walker::new(true);
    w.run(rom, src)?;
    let len = usize::try_from(w.lens[0]).unwrap_or(usize::MAX);
    let mut data = w.data.unwrap_or_default();
    data.resize(len, 0);
    Ok(Assembled {
        data,
        md5: w.md5.finish(),
    })
}

/// The MiSTer MD5 of `rom`, streamed from `src` without holding the rom in memory.
/// Refuses and reports exactly what [`assemble`] does.
///
/// # Errors
///
/// As [`assemble`].
///
/// ```
/// use mistarr_mister::adapter::arcade::{assemble, mra};
/// let rom = &mra::parse(b"<m><rom><part>616263</part></rom></m>").unwrap().roms[0];
/// assert_eq!(assemble::md5(rom, &mut assemble::NoParts).unwrap(), "900150983cd24fb0d6963f7d28e17f72");
/// ```
pub fn md5(rom: &MraRom, src: &mut dyn PartSource) -> Result<String> {
    let mut w = Walker::new(false);
    w.run(rom, src)?;
    Ok(w.md5.finish())
}

/// A [`PartSource`] with no zips, for roms made only of inline data.
///
/// ```
/// use mistarr_mister::adapter::arcade::assemble::{NoParts, PartSource};
/// assert!(NoParts.open("exblast.zip", "a.bin", None).unwrap().is_none());
/// ```
#[derive(Debug, Clone, Copy, Default)]
pub struct NoParts;

impl PartSource for NoParts {
    fn open(&mut self, _: &str, _: &str, _: Option<u32>) -> io::Result<Option<Box<dyn Read + '_>>> {
        Ok(None)
    }
}

fn refuse(reason: impl Into<String>) -> Error {
    Error::MraUnsupported(reason.into())
}

fn too_large() -> Error {
    refuse(format!("rom is larger than {MAX_ROM_BYTES} bytes"))
}

/// `a + b`, refused on overflow.
fn add(a: u64, b: u64) -> Result<u64> {
    a.checked_add(b)
        .ok_or_else(|| refuse("offset arithmetic overflows"))
}

/// `a + b` as a memory index, refused on overflow.
fn add_usize(a: usize, b: usize) -> Result<usize> {
    a.checked_add(b)
        .ok_or_else(|| refuse("offset arithmetic overflows"))
}

/// Where each input byte of a part lands inside an output word.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Layout {
    /// Which of the eight write cursors the part advances.
    idx: usize,
    /// Output offset of the k-th input byte of each word.
    offsets: Vec<u64>,
    /// Output word width in bytes.
    unit: u64,
}

/// Reads a `map` the way MiSTer's `rom_data` does: nibbles from the least significant
/// up, each non-zero nibble placing the next input byte, zero nibbles after the first leaving gaps.
fn layout(map: Option<&str>, unit: u64) -> Result<Layout> {
    let raw = map.unwrap_or("").trim();
    if raw.len() > 16 {
        return Err(refuse(format!("map {raw:?} is longer than 16 digits")));
    }
    let mut value = if raw.is_empty() {
        0
    } else {
        u64::from_str_radix(raw, 16).map_err(|_| refuse(format!("map {raw:?} is not hex")))?
    };
    if value == 0 {
        value = 1;
    }
    let nibble = |i: u64| (value >> (4 * i)) & 0xf;
    let idx = (0..unit)
        .find(|&i| nibble(i) != 0)
        .ok_or_else(|| refuse(format!("map {raw:?} selects no byte of a {unit}-byte word")))?;
    let (mut offsets, mut gaps, mut first) = (Vec::new(), 0, true);
    for i in 0..unit {
        let n = nibble(i);
        if n != 0 {
            let at = idx + n - 1 + gaps;
            if at >= unit {
                return Err(refuse(format!(
                    "map {raw:?} writes past a {unit}-byte word"
                )));
            }
            offsets.push(at);
            first = false;
        } else if !first {
            gaps += 1;
        }
    }
    Ok(Layout {
        idx: usize::try_from(idx).unwrap_or(0),
        offsets,
        unit,
    })
}

fn interleave_unit(il: &Interleave) -> Result<u64> {
    if il.input != 8 {
        return Err(refuse(format!("interleave input {} is not 8", il.input)));
    }
    if !(8..=64).contains(&il.output) || il.output % 8 != 0 {
        return Err(refuse(format!(
            "interleave output {} is not 8 to 64 in steps of 8",
            il.output
        )));
    }
    Ok(u64::from(il.output / 8))
}

/// Refuses anything outside the subset before a byte is read.
fn check(rom: &MraRom) -> Result<()> {
    for item in &rom.items {
        match item {
            RomItem::Unsupported(r) => return Err(refuse(r.clone())),
            RomItem::Part(p) if p.map.is_some() => {
                return Err(refuse("map outside <interleave>"));
            }
            RomItem::Interleave(il) => {
                let unit = interleave_unit(il)?;
                for p in &il.parts {
                    layout(p.map.as_deref(), unit)?;
                }
            }
            RomItem::Part(_) | RomItem::Patch(_) => {}
        }
    }
    Ok(())
}

struct Walker {
    md5: Md5Stream,
    data: Option<Vec<u8>>,
    /// MiSTer's `romlen`: one write cursor per starting byte of a word; `lens[0]` is the rom length.
    lens: [u64; 8],
    fed: u64,
}

impl Walker {
    fn new(build: bool) -> Self {
        Self {
            md5: Md5Stream::new(),
            data: build.then(Vec::new),
            lens: [0; 8],
            fed: 0,
        }
    }

    fn run(&mut self, rom: &MraRom, src: &mut dyn PartSource) -> Result<()> {
        check(rom)?;
        let plain = Layout {
            idx: 0,
            offsets: vec![0],
            unit: 1,
        };
        for item in &rom.items {
            match item {
                RomItem::Part(p) => self.part(p, &rom.zips, &plain, src)?,
                RomItem::Interleave(il) => {
                    let unit = interleave_unit(il)?;
                    let base = self.lens[0];
                    self.lens.iter_mut().skip(1).for_each(|l| *l = base);
                    for p in &il.parts {
                        self.part(p, &rom.zips, &layout(p.map.as_deref(), unit)?, src)?;
                    }
                }
                RomItem::Patch(p) => self.patch(p)?,
                RomItem::Unsupported(r) => return Err(refuse(r.clone())),
            }
        }
        Ok(())
    }

    fn part(
        &mut self,
        p: &Part,
        rom_zips: &[String],
        layout: &Layout,
        src: &mut dyn PartSource,
    ) -> Result<()> {
        if p.repeat > MAX_REPEAT {
            return Err(refuse(format!(
                "part repeat {} is above {MAX_REPEAT}",
                p.repeat
            )));
        }
        if p.repeat == 0 {
            return Ok(());
        }
        if let (Some(name), None) = (&p.name, &self.data) {
            return self.stream_named(p, name, rom_zips, layout, src);
        }
        let named;
        let bytes: &[u8] = match &p.name {
            None => &p.data,
            Some(name) => {
                named = self.read_named(p, name, rom_zips, src)?;
                &named
            }
        };
        if bytes.is_empty() && p.repeat > 1 {
            return Err(refuse(format!(
                "part {} is empty and repeated {} times",
                p.name.as_deref().unwrap_or("(inline)"),
                p.repeat
            )));
        }
        let total = (bytes.len() as u64)
            .checked_mul(p.repeat)
            .ok_or_else(too_large)?;
        if add(self.fed, total)? > MAX_ROM_BYTES {
            return Err(too_large());
        }
        let mut k = 0;
        for _ in 0..p.repeat {
            self.feed(bytes, layout, &mut k)?;
        }
        Self::whole_words(k)
    }

    /// Reads a named part's bytes once: the member from the first of its zips holding it,
    /// after `offset`, up to `length`, never more than the rom may still grow by.
    fn read_named(
        &self,
        p: &Part,
        name: &str,
        rom_zips: &[String],
        src: &mut dyn PartSource,
    ) -> Result<Vec<u8>> {
        let zips = if p.zips.is_empty() { rom_zips } else { &p.zips };
        if zips.is_empty() {
            return Err(refuse(format!("part {name} names no zip")));
        }
        let budget = MAX_ROM_BYTES.saturating_sub(self.fed);
        for zip in zips {
            let Some(mut reader) = src.open(zip, name, p.crc)? else {
                continue;
            };
            let skipped = io::copy(&mut (&mut reader).take(p.offset), &mut io::sink())?;
            if skipped < p.offset {
                return Err(refuse(format!("offset of part {name} is past its end")));
            }
            let over = budget.saturating_add(1);
            let limit = p.length.map_or(over, |l| l.min(over));
            let mut bytes = Vec::new();
            reader.take(limit).read_to_end(&mut bytes)?;
            if bytes.len() as u64 > budget {
                return Err(too_large());
            }
            return Ok(bytes);
        }
        Err(Error::MissingPart {
            part: name.to_owned(),
            zips: zips.join("|"),
        })
    }

    /// Feeds a named part straight from its member in [`STREAM_CHUNK`] pieces, opening the
    /// member again for each repeat, for a digest that keeps no rom bytes; refuses exactly
    /// what [`Walker::part`] does when it reads the part whole.
    fn stream_named(
        &mut self,
        p: &Part,
        name: &str,
        rom_zips: &[String],
        layout: &Layout,
        src: &mut dyn PartSource,
    ) -> Result<()> {
        let zips = if p.zips.is_empty() { rom_zips } else { &p.zips };
        if zips.is_empty() {
            return Err(refuse(format!("part {name} names no zip")));
        }
        let mut k = 0;
        for rep in 0..p.repeat {
            let before = self.fed;
            self.stream_once(p, name, zips, layout, src, &mut k)?;
            if rep > 0 {
                continue;
            }
            let once = self.fed - before;
            if once == 0 && p.repeat > 1 {
                return Err(refuse(format!(
                    "part {name} is empty and repeated {} times",
                    p.repeat
                )));
            }
            let rest = once.checked_mul(p.repeat - 1).ok_or_else(too_large)?;
            if add(self.fed, rest)? > MAX_ROM_BYTES {
                return Err(too_large());
            }
        }
        Self::whole_words(k)
    }

    /// Feeds one pass of a named part from the first of `zips` holding it.
    fn stream_once(
        &mut self,
        p: &Part,
        name: &str,
        zips: &[String],
        layout: &Layout,
        src: &mut dyn PartSource,
        k: &mut usize,
    ) -> Result<()> {
        for zip in zips {
            let Some(mut reader) = src.open(zip, name, p.crc)? else {
                continue;
            };
            let skipped = io::copy(&mut (&mut reader).take(p.offset), &mut io::sink())?;
            if skipped < p.offset {
                return Err(refuse(format!("offset of part {name} is past its end")));
            }
            let mut reader = reader.take(p.length.unwrap_or(u64::MAX));
            let mut buf = vec![0; STREAM_CHUNK];
            loop {
                let n = match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => n,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(e) => return Err(e.into()),
                };
                self.feed(&buf[..n], layout, k)?;
            }
            return Ok(());
        }
        Err(Error::MissingPart {
            part: name.to_owned(),
            zips: zips.join("|"),
        })
    }

    fn whole_words(k: usize) -> Result<()> {
        if k == 0 {
            Ok(())
        } else {
            Err(refuse(
                "part length is not a whole number of interleaved words",
            ))
        }
    }

    fn feed(&mut self, chunk: &[u8], layout: &Layout, k: &mut usize) -> Result<()> {
        self.fed = add(self.fed, chunk.len() as u64)?;
        if self.fed > MAX_ROM_BYTES {
            return Err(too_large());
        }
        self.md5.update(chunk);
        for &b in chunk {
            let at = add(self.lens[layout.idx], layout.offsets[*k])?;
            if let Some(data) = &mut self.data {
                let at = usize::try_from(at).map_err(|_| refuse("rom does not fit in memory"))?;
                if data.len() <= at {
                    data.resize(add_usize(at, 1)?, 0);
                }
                data[at] = b;
            }
            *k += 1;
            if *k == layout.offsets.len() {
                *k = 0;
                self.lens[layout.idx] = add(self.lens[layout.idx], layout.unit)?;
            }
        }
        Ok(())
    }

    fn patch(&mut self, p: &Patch) -> Result<()> {
        let end = add(p.offset, p.data.len() as u64)?;
        if end > self.lens[0] {
            return Err(refuse(format!(
                "patch at {} runs past the rom end {}",
                p.offset, self.lens[0]
            )));
        }
        if let Some(data) = &mut self.data {
            let start = usize::try_from(p.offset).map_err(|_| refuse("patch offset too large"))?;
            let end = add_usize(start, p.data.len())?;
            if data.len() < end {
                data.resize(end, 0);
            }
            for (slot, b) in data[start..].iter_mut().zip(&p.data) {
                *slot = if p.xor { *slot ^ b } else { *b };
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
