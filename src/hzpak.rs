//! HZPAK container writer and verifier.
//!
//! The byte layout follows the public Horizon NX format: HZPAK001, packed file
//! payloads, a little-endian index, then (index offset, index length, HZPAKEND).
//! Files over MAX_PACKED remain loose beside the pack.

use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

pub const MAX_PACKED: u64 = 8 << 20;
const MAGIC: &[u8; 8] = b"HZPAK001";
const END: &[u8; 8] = b"HZPAKEND";
const FOOTER: usize = 24;
const MAX_INDEX_BYTES: u64 = 512 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Dir,
    Packed(u64),
    Loose,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    pub kind: Kind,
    pub size: u64,
    pub mtime: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Top {
    pub name: String,
    pub dir: bool,
    pub size: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PackSummary {
    pub entries: u32,
    pub top_level_items: u32,
    pub index_bytes: u64,
}

pub struct Writer<W: Write> {
    out: W,
    offset: u64,
    entries: Vec<(String, Entry)>,
}

impl<W: Write> Writer<W> {
    pub fn begin(mut out: W, root_mtime: i64) -> io::Result<Self> {
        out.write_all(MAGIC)?;
        Ok(Self {
            out,
            offset: MAGIC.len() as u64,
            entries: vec![(
                String::new(),
                Entry {
                    kind: Kind::Dir,
                    size: 0,
                    mtime: root_mtime,
                },
            )],
        })
    }

    pub fn file(&mut self, relative: &str, data: &[u8], mtime: i64) -> io::Result<()> {
        validate_name(relative)?;
        self.out.write_all(data)?;
        let offset = self.offset;
        self.offset = self
            .offset
            .checked_add(data.len() as u64)
            .ok_or_else(|| invalid("pack size overflow"))?;
        self.entries.push((
            relative.to_owned(),
            Entry {
                kind: Kind::Packed(offset),
                size: data.len() as u64,
                mtime,
            },
        ));
        Ok(())
    }

    pub fn entry(&mut self, relative: &str, entry: Entry) -> io::Result<()> {
        validate_name(relative)?;
        self.entries.push((relative.to_owned(), entry));
        Ok(())
    }

    pub fn finish(mut self, top: &[Top]) -> io::Result<W> {
        if self.entries.len() > u32::MAX as usize || top.len() > u32::MAX as usize {
            return Err(invalid("too many entries for HZPAK"));
        }
        let index = encode(&self.entries, top)?;
        self.out.write_all(&index)?;
        self.out.write_all(&self.offset.to_le_bytes())?;
        self.out.write_all(&(index.len() as u64).to_le_bytes())?;
        self.out.write_all(END)?;
        self.out.flush()?;
        Ok(self.out)
    }
}

fn validate_name(name: &str) -> io::Result<()> {
    if name.as_bytes().len() > u16::MAX as usize {
        return Err(invalid("a relative path is longer than the HZPAK path limit"));
    }
    if name.contains('\n') || name.contains('\r') {
        return Err(invalid("a relative path contains a line break"));
    }
    Ok(())
}

fn put_str(out: &mut Vec<u8>, value: &str) -> io::Result<()> {
    validate_name(value)?;
    out.extend_from_slice(&(value.len() as u16).to_le_bytes());
    out.extend_from_slice(value.as_bytes());
    Ok(())
}

fn encode(entries: &[(String, Entry)], top: &[Top]) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    out.extend_from_slice(&(entries.len() as u32).to_le_bytes());

    for (relative, entry) in entries {
        put_str(&mut out, relative)?;
        let (kind, offset) = match entry.kind {
            Kind::Dir => (0u8, 0u64),
            Kind::Packed(offset) => (1, offset),
            Kind::Loose => (2, 0),
        };
        out.push(kind);
        out.extend_from_slice(&offset.to_le_bytes());
        out.extend_from_slice(&entry.size.to_le_bytes());
        out.extend_from_slice(&entry.mtime.to_le_bytes());
    }

    out.extend_from_slice(&(top.len() as u32).to_le_bytes());
    for item in top {
        put_str(&mut out, &item.name)?;
        out.push(u8::from(item.dir));
        out.extend_from_slice(&item.size.to_le_bytes());
    }
    Ok(out)
}

pub fn verify(path: &Path) -> io::Result<PackSummary> {
    let mut file = std::fs::File::open(path)?;
    let total = file.metadata()?.len();
    if total < (MAGIC.len() + FOOTER) as u64 {
        return Err(invalid("pack is too short"));
    }

    let mut magic = [0u8; 8];
    file.read_exact(&mut magic)?;
    if &magic != MAGIC {
        return Err(invalid("incorrect HZPAK header"));
    }

    file.seek(SeekFrom::End(-(FOOTER as i64)))?;
    let mut footer = [0u8; FOOTER];
    file.read_exact(&mut footer)?;
    if &footer[16..] != END {
        return Err(invalid("missing HZPAK footer"));
    }

    let index_at = u64::from_le_bytes(footer[..8].try_into().unwrap());
    let index_len = u64::from_le_bytes(footer[8..16].try_into().unwrap());
    if index_at < MAGIC.len() as u64
        || index_at.checked_add(index_len).and_then(|v| v.checked_add(FOOTER as u64)) != Some(total)
        || index_len > MAX_INDEX_BYTES
        || index_len > usize::MAX as u64
    {
        return Err(invalid("HZPAK index bounds are invalid"));
    }

    file.seek(SeekFrom::Start(index_at))?;
    let mut index = vec![0u8; index_len as usize];
    file.read_exact(&mut index)?;
    decode_and_check(&index, index_at)
}

fn decode_and_check(index: &[u8], data_end: u64) -> io::Result<PackSummary> {
    let mut cursor = 0usize;
    let entry_count = read_num(index, &mut cursor, 4)? as u32;

    for _ in 0..entry_count {
        let _relative = read_string(index, &mut cursor)?;
        let kind = read_num(index, &mut cursor, 1)? as u8;
        let offset = read_num(index, &mut cursor, 8)?;
        let size = read_num(index, &mut cursor, 8)?;
        let _mtime = read_num(index, &mut cursor, 8)? as i64;
        match kind {
            0 | 2 => {}
            1 => {
                if offset < MAGIC.len() as u64
                    || offset.checked_add(size).is_none_or(|end| end > data_end)
                {
                    return Err(invalid("packed file points outside the payload area"));
                }
            }
            _ => return Err(invalid("unknown HZPAK entry kind")),
        }
    }

    let top_count = read_num(index, &mut cursor, 4)? as u32;
    for _ in 0..top_count {
        let _name = read_string(index, &mut cursor)?;
        let _is_dir = read_num(index, &mut cursor, 1)?;
        let _size = read_num(index, &mut cursor, 8)?;
    }

    if cursor != index.len() {
        return Err(invalid("unexpected bytes after HZPAK index"));
    }

    Ok(PackSummary {
        entries: entry_count,
        top_level_items: top_count,
        index_bytes: index.len() as u64,
    })
}

fn read_num(bytes: &[u8], cursor: &mut usize, count: usize) -> io::Result<u64> {
    let end = cursor
        .checked_add(count)
        .ok_or_else(|| invalid("index cursor overflow"))?;
    let slice = bytes
        .get(*cursor..end)
        .ok_or_else(|| invalid("truncated HZPAK index"))?;
    *cursor = end;
    let mut value = [0u8; 8];
    value[..count].copy_from_slice(slice);
    Ok(u64::from_le_bytes(value))
}

fn read_string(bytes: &[u8], cursor: &mut usize) -> io::Result<String> {
    let len = read_num(bytes, cursor, 2)? as usize;
    let end = cursor
        .checked_add(len)
        .ok_or_else(|| invalid("index cursor overflow"))?;
    let slice = bytes
        .get(*cursor..end)
        .ok_or_else(|| invalid("truncated HZPAK path"))?;
    *cursor = end;
    String::from_utf8(slice.to_vec()).map_err(|_| invalid("HZPAK path is not UTF-8"))
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_and_verifies_a_pack() {
        let mut writer = Writer::begin(Vec::new(), 1).unwrap();
        writer.file("assets/example.bin", b"payload", 2).unwrap();
        writer
            .entry(
                "data",
                Entry {
                    kind: Kind::Dir,
                    size: 0,
                    mtime: 3,
                },
            )
            .unwrap();
        let bytes = writer
            .finish(&[Top {
                name: "assets".into(),
                dir: true,
                size: 0,
            }])
            .unwrap();
        assert_eq!(&bytes[..8], MAGIC);
        assert_eq!(&bytes[bytes.len() - 8..], END);

        let path = std::env::temp_dir().join(format!(
            "nx-toolkit-hzpak-test-{}.hzpak",
            std::process::id()
        ));
        std::fs::write(&path, bytes).unwrap();
        let summary = verify(&path).unwrap();
        assert_eq!(summary.entries, 3);
        assert_eq!(summary.top_level_items, 1);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn rejects_truncated_pack() {
        let path = std::env::temp_dir().join(format!(
            "nx-toolkit-hzpak-short-{}.hzpak",
            std::process::id()
        ));
        std::fs::write(&path, b"HZPAK001").unwrap();
        assert!(verify(&path).is_err());
        let _ = std::fs::remove_file(path);
    }
}
