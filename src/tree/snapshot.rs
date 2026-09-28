//! The assembled namespace as one flat, interned, immutable byte layout
//! (DEC-060).
//!
//! Every string is interned once into a table; everything else is a `u32`.
//! Names are sorted by FQN, their sites, mixins and extends are ranges into
//! flat arrays, and an open-addressed table maps an FQN to its name. Nothing
//! points at anything, so the same bytes serve as the tree in memory and as a
//! file mapped read-only — loading one is a validation, not a decode.
//!
//! The layout carries no `HashMap` order: names are written sorted and strings
//! interned in the order they are first met, so one namespace always encodes
//! to the same bytes. Two processes that build it at once write one file.

use super::{Entry, MixinKind, Site, Target, Written};
use std::collections::HashMap;

const MAGIC: &[u8; 8] = b"trekrtre";
/// Bumped whenever the layout below changes shape.
pub(super) const FORMAT: u32 = 2;
const NONE: u32 = u32::MAX;

// Sections, in file order. Each starts 8-byte aligned.
const STR_BYTES: usize = 0;
const STR_OFFS: usize = 1;
const NAMES: usize = 2;
const SITES: usize = 3;
const MIXINS: usize = 4;
const EXTENDS: usize = 5;
const TARGETS: usize = 6;
const NEST_OFFS: usize = 7;
const NEST_ITEMS: usize = 8;
const INDEX: usize = 9;
const SECTIONS: usize = 10;

/// A name: fqn, kind, sites (start, len), mixins (start, len), extends
/// (start, len), superclass target, alias target.
const NAME_WORDS: usize = 10;
/// A site: path, line, col, kind.
const SITE_WORDS: usize = 4;
/// A mixin: kind (0 prepend, 1 include), target.
const MIXIN_WORDS: usize = 2;
/// A singleton mixin: kind (0 extend, 1 singleton prepend), target.
const EXTEND_WORDS: usize = 2;
/// A target: name, nesting list.
const TARGET_WORDS: usize = 2;

/// magic, format + pad, key (32), length, checksum, then the section table.
const HEADER: usize = 8 + 8 + 32 + 8 + 8 + SECTIONS * 16;

/// What a snapshot was built from. Stored in the header, so a file that
/// answers to the wrong inputs is refused even under the right name.
pub(super) type Key = [u8; 20];

/// Why a file was not used. Any of these means "build it again".
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Invalid {
    Magic,
    Format(u32),
    Key,
    /// Shorter or longer than its header says — a torn or truncated write.
    Length,
    Checksum,
    Layout,
}

pub(super) enum Bytes {
    Owned(Vec<u8>),
    /// A file's pages, shared with every process that maps it.
    Mapped(memmap2::Mmap),
}

impl std::ops::Deref for Bytes {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        match self {
            Bytes::Owned(v) => v,
            Bytes::Mapped(m) => m,
        }
    }
}

pub(super) struct Snapshot {
    bytes: Bytes,
    /// (offset, length) of each section, in bytes.
    sections: [(usize, usize); SECTIONS],
}

impl Snapshot {
    /// Check a snapshot's bytes against the key it should answer to. Reads
    /// every byte once, for the checksum: a mapped file's pages are the page
    /// cache's, so that costs faults, not private memory.
    pub(super) fn parse(bytes: Bytes, key: &Key) -> Result<Snapshot, Invalid> {
        let b: &[u8] = &bytes;
        if b.len() < HEADER || &b[..8] != MAGIC {
            return Err(Invalid::Magic);
        }
        let format = u32_at(b, 8);
        if format != FORMAT {
            return Err(Invalid::Format(format));
        }
        if b[16..16 + key.len()] != key[..] {
            return Err(Invalid::Key);
        }
        if u64_at(b, 48) != b.len() as u64 {
            return Err(Invalid::Length);
        }
        if u64_at(b, 56) != checksum(&b[HEADER..]) {
            return Err(Invalid::Checksum);
        }
        let mut sections = [(0, 0); SECTIONS];
        for (i, section) in sections.iter_mut().enumerate() {
            let at = 64 + i * 16;
            let (off, len) = (u64_at(b, at) as usize, u64_at(b, at + 8) as usize);
            let fits = off.checked_add(len).is_some_and(|end| end <= b.len());
            if !fits || off % 8 != 0 || (i != STR_BYTES && len % 4 != 0) {
                return Err(Invalid::Layout);
            }
            *section = (off, len);
        }
        let snapshot = Snapshot { bytes, sections };
        let index = snapshot.count(INDEX, 1);
        let whole =
            |section: usize, words: usize| snapshot.sections[section].1.is_multiple_of(4 * words);
        if !index.is_power_of_two()
            || snapshot.count(STR_OFFS, 1) == 0
            || snapshot.count(NEST_OFFS, 1) == 0
            || !whole(NAMES, NAME_WORDS)
            || !whole(SITES, SITE_WORDS)
            || !whole(MIXINS, MIXIN_WORDS)
            || !whole(TARGETS, TARGET_WORDS)
        {
            return Err(Invalid::Layout);
        }
        Ok(snapshot)
    }

    /// Whether this one is a file mapping rather than bytes on the heap.
    #[cfg(test)]
    pub(super) fn is_mapped(&self) -> bool {
        matches!(self.bytes, Bytes::Mapped(_))
    }

    fn count(&self, section: usize, words: usize) -> usize {
        self.sections[section].1 / (4 * words)
    }

    fn word(&self, section: usize, i: usize) -> u32 {
        u32_at(&self.bytes, self.sections[section].0 + 4 * i)
    }

    fn raw(&self, id: u32) -> &[u8] {
        let (start, end) = (
            self.word(STR_OFFS, id as usize),
            self.word(STR_OFFS, id as usize + 1),
        );
        let base = self.sections[STR_BYTES].0;
        &self.bytes[base + start as usize..base + end as usize]
    }

    fn str(&self, id: u32) -> &str {
        std::str::from_utf8(self.raw(id)).expect("snapshot strings are written from `str`s")
    }

    pub(super) fn len(&self) -> usize {
        self.count(NAMES, NAME_WORDS)
    }

    pub(super) fn find(&self, fqn: &str) -> Option<NameRef<'_>> {
        let mask = self.count(INDEX, 1) - 1;
        let mut slot = fnv(fqn.as_bytes()) as usize & mask;
        // Bounded, so even a table with no empty slot cannot spin.
        for _ in 0..=mask {
            let id = self.word(INDEX, slot);
            if id == NONE {
                return None;
            }
            let name = NameRef { snap: self, id };
            if self.raw(name.field(0)) == fqn.as_bytes() {
                return Some(name);
            }
            slot = (slot + 1) & mask;
        }
        None
    }

    pub(super) fn names(&self) -> impl Iterator<Item = NameRef<'_>> {
        (0..self.len() as u32).map(|id| NameRef { snap: self, id })
    }

    fn target(&self, id: u32) -> Written<'_> {
        let base = id as usize * TARGET_WORDS;
        let nesting = self.word(TARGETS, base + 1) as usize;
        let (start, end) = (
            self.word(NEST_OFFS, nesting) as usize,
            self.word(NEST_OFFS, nesting + 1) as usize,
        );
        Written {
            name: self.str(self.word(TARGETS, base)),
            nesting: (start..end)
                .map(|i| self.str(self.word(NEST_ITEMS, i)))
                .collect(),
        }
    }
}

/// One name's entry, read in place.
#[derive(Clone, Copy)]
pub(super) struct NameRef<'a> {
    snap: &'a Snapshot,
    id: u32,
}

impl<'a> NameRef<'a> {
    fn field(self, f: usize) -> u32 {
        self.snap.word(NAMES, self.id as usize * NAME_WORDS + f)
    }

    fn range(self, f: usize) -> std::ops::Range<usize> {
        let start = self.field(f) as usize;
        start..start + self.field(f + 1) as usize
    }

    fn optional(self, f: usize) -> Option<Written<'a>> {
        let id = self.field(f);
        (id != NONE).then(|| self.snap.target(id))
    }

    pub(super) fn fqn(self) -> &'a str {
        self.snap.str(self.field(0))
    }

    pub(super) fn kind(self) -> &'a str {
        self.snap.str(self.field(1))
    }

    pub(super) fn sites(self) -> Vec<Site> {
        let s = self.snap;
        self.range(2)
            .map(|i| {
                let at = i * SITE_WORDS;
                Site {
                    path: s.str(s.word(SITES, at)).to_string(),
                    line: s.word(SITES, at + 1),
                    col: s.word(SITES, at + 2),
                    kind: s.str(s.word(SITES, at + 3)).to_string(),
                }
            })
            .collect()
    }

    pub(super) fn mixins(self) -> Vec<(MixinKind, Written<'a>)> {
        let s = self.snap;
        self.range(4)
            .map(|i| {
                let kind = match s.word(MIXINS, i * MIXIN_WORDS) {
                    0 => MixinKind::Prepend,
                    _ => MixinKind::Include,
                };
                (kind, s.target(s.word(MIXINS, i * MIXIN_WORDS + 1)))
            })
            .collect()
    }

    /// `extend`s, or with `prepended` the singleton class's prepends.
    pub(super) fn extends(self, prepended: bool) -> Vec<Written<'a>> {
        let s = self.snap;
        self.range(6)
            .filter(|i| (s.word(EXTENDS, i * EXTEND_WORDS) == 1) == prepended)
            .map(|i| s.target(s.word(EXTENDS, i * EXTEND_WORDS + 1)))
            .collect()
    }

    pub(super) fn superclass(self) -> Option<Written<'a>> {
        self.optional(8)
    }

    pub(super) fn alias_of(self) -> Option<Written<'a>> {
        self.optional(9)
    }
}

/// Lay a namespace out flat.
pub(super) fn encode(names: &HashMap<String, Entry>, key: &Key) -> anyhow::Result<Vec<u8>> {
    let mut order: Vec<(&String, &Entry)> = names.iter().collect();
    order.sort_unstable_by(|a, b| a.0.cmp(b.0));

    let mut w = Encoder::default();
    w.nest_offs.push(0);
    w.str_offs.push(0);
    let mut records: Vec<u32> = Vec::with_capacity(order.len() * NAME_WORDS);
    for (fqn, entry) in &order {
        let fqn = w.string(fqn)?;
        let kind = w.string(&entry.kind)?;
        let sites = w.sites.len() / SITE_WORDS;
        for site in &entry.sites {
            let path = w.string(&site.path)?;
            let kind = w.string(&site.kind)?;
            w.sites.extend([path, site.line, site.col, kind]);
        }
        let mixins = w.mixins.len() / MIXIN_WORDS;
        for mixin in &entry.mixins {
            let kind = match mixin.kind {
                MixinKind::Prepend => 0,
                MixinKind::Include => 1,
            };
            let target = w.target(&mixin.target.name, &mixin.target.nesting)?;
            w.mixins.extend([kind, target]);
        }
        let extends = w.extends.len() / EXTEND_WORDS;
        let singleton = entry.extends.iter().map(|t| (0, t));
        let prepended = entry.singleton_prepends.iter().map(|t| (1, t));
        for (kind, target) in singleton.chain(prepended) {
            let target = w.target(&target.name, &target.nesting)?;
            w.extends.extend([kind, target]);
        }
        let superclass = w.optional(&entry.superclass)?;
        let alias = w.optional(&entry.alias_of)?;
        records.extend([
            fqn,
            kind,
            id(sites)?,
            id(entry.sites.len())?,
            id(mixins)?,
            id(entry.mixins.len())?,
            id(extends)?,
            id(entry.extends.len() + entry.singleton_prepends.len())?,
            superclass,
            alias,
        ]);
    }

    // Half full at most, so a probe is short.
    let slots = (order.len() * 2).next_power_of_two().max(2);
    let mut index = vec![NONE; slots];
    for (i, (fqn, _)) in order.iter().enumerate() {
        let mut slot = fnv(fqn.as_bytes()) as usize & (slots - 1);
        while index[slot] != NONE {
            slot = (slot + 1) & (slots - 1);
        }
        index[slot] = id(i)?;
    }

    let words: [&[u32]; SECTIONS - 1] = [
        &w.str_offs,
        &records,
        &w.sites,
        &w.mixins,
        &w.extends,
        &w.targets,
        &w.nest_offs,
        &w.nest_items,
        &index,
    ];
    let total = HEADER + w.strings.len() + words.iter().map(|v| 4 * v.len() + 8).sum::<usize>() + 8;
    let mut out = Vec::with_capacity(total);
    out.resize(HEADER, 0);
    out[..8].copy_from_slice(MAGIC);
    out[8..12].copy_from_slice(&FORMAT.to_le_bytes());
    out[16..16 + key.len()].copy_from_slice(key);
    let section = |i: usize, out: &mut Vec<u8>, write: &dyn Fn(&mut Vec<u8>)| {
        out.resize(out.len().next_multiple_of(8), 0);
        let start = out.len();
        write(out);
        let len = (out.len() - start) as u64;
        let at = 64 + i * 16;
        out[at..at + 8].copy_from_slice(&(start as u64).to_le_bytes());
        out[at + 8..at + 16].copy_from_slice(&len.to_le_bytes());
    };
    section(STR_BYTES, &mut out, &|out| {
        out.extend_from_slice(&w.strings)
    });
    for (i, v) in words.into_iter().enumerate() {
        section(i + 1, &mut out, &|out| {
            for word in v {
                out.extend_from_slice(&word.to_le_bytes());
            }
        });
    }
    let len = out.len() as u64;
    out[48..56].copy_from_slice(&len.to_le_bytes());
    let sum = checksum(&out[HEADER..]);
    out[56..64].copy_from_slice(&sum.to_le_bytes());
    Ok(out)
}

#[derive(Default)]
struct Encoder<'a> {
    strings: Vec<u8>,
    str_offs: Vec<u32>,
    interned: HashMap<&'a str, u32, Fx>,
    sites: Vec<u32>,
    mixins: Vec<u32>,
    extends: Vec<u32>,
    targets: Vec<u32>,
    nest_offs: Vec<u32>,
    nest_items: Vec<u32>,
    nestings: HashMap<Vec<u32>, u32, Fx>,
}

/// rustc's Fx hash. The interner hashes every string in the namespace, and
/// SipHash's DoS resistance buys nothing over names the store already holds.
/// Only lookups go through it: ids are assigned in first-met order, so the
/// encoded bytes do not depend on it.
#[derive(Default, Clone, Copy)]
struct Fx;

impl std::hash::BuildHasher for Fx {
    type Hasher = FxHasher;
    fn build_hasher(&self) -> FxHasher {
        FxHasher(0)
    }
}

struct FxHasher(u64);

impl std::hash::Hasher for FxHasher {
    fn write(&mut self, bytes: &[u8]) {
        let (words, rest) = bytes.as_chunks::<8>();
        for word in words {
            self.add(u64::from_le_bytes(*word));
        }
        for byte in rest {
            self.add(*byte as u64);
        }
    }
    fn write_u32(&mut self, n: u32) {
        self.add(n as u64);
    }
    fn write_u64(&mut self, n: u64) {
        self.add(n);
    }
    fn write_usize(&mut self, n: usize) {
        self.add(n as u64);
    }
    fn finish(&self) -> u64 {
        self.0
    }
}

impl FxHasher {
    fn add(&mut self, word: u64) {
        self.0 = (self.0.rotate_left(5) ^ word).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
    }
}

impl<'a> Encoder<'a> {
    fn string(&mut self, s: &'a str) -> anyhow::Result<u32> {
        if let Some(id) = self.interned.get(s) {
            return Ok(*id);
        }
        let next = id(self.str_offs.len() - 1)?;
        self.strings.extend_from_slice(s.as_bytes());
        self.str_offs.push(id(self.strings.len())?);
        self.interned.insert(s, next);
        Ok(next)
    }

    fn optional(&mut self, target: &'a Option<Target>) -> anyhow::Result<u32> {
        match target {
            Some(t) => self.target(&t.name, &t.nesting),
            None => Ok(NONE),
        }
    }

    fn target(&mut self, name: &'a str, nesting: &'a [String]) -> anyhow::Result<u32> {
        let name = self.string(name)?;
        let items = nesting
            .iter()
            .map(|s| self.string(s))
            .collect::<anyhow::Result<Vec<u32>>>()?;
        let nesting = match self.nestings.get(&items) {
            Some(id) => *id,
            None => {
                let next = id(self.nest_offs.len() - 1)?;
                self.nest_items.extend_from_slice(&items);
                self.nest_offs.push(id(self.nest_items.len())?);
                self.nestings.insert(items, next);
                next
            }
        };
        let next = id(self.targets.len() / TARGET_WORDS)?;
        self.targets.extend([name, nesting]);
        Ok(next)
    }
}

/// Every count and offset is a `u32`. 4 GiB of namespace is past any repo
/// measured, so running out is an error rather than a wider format.
fn id(n: usize) -> anyhow::Result<u32> {
    u32::try_from(n)
        .map_err(|_| anyhow::anyhow!("tree snapshot: namespace too large for format {FORMAT}"))
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().expect("a four-byte slice"))
}

fn u64_at(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().expect("an eight-byte slice"))
}

/// FNV-1a: the index's hash is part of the format, so it must not be one
/// whose output a toolchain upgrade could change (as `std`'s may).
fn fnv(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    hash
}

/// A tripwire for a torn or rotted file, not a defence against a crafted one.
/// Four independent lanes, so it runs at memory speed rather than one
/// multiply per byte.
fn checksum(bytes: &[u8]) -> u64 {
    const K: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut lanes = [1u64, 2, 3, 4];
    let (blocks, rest) = bytes.as_chunks::<32>();
    for block in blocks {
        for (lane, word) in lanes.iter_mut().zip(block.as_chunks::<8>().0) {
            *lane = (*lane ^ u64::from_le_bytes(*word))
                .wrapping_mul(K)
                .rotate_left(31);
        }
    }
    let mut hash = bytes.len() as u64;
    for lane in lanes {
        hash = (hash ^ lane).wrapping_mul(K).rotate_left(27);
    }
    for byte in rest {
        hash = (hash ^ *byte as u64).wrapping_mul(K);
    }
    hash ^ (hash >> 32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn namespace() -> HashMap<String, Entry> {
        let sources = [
            (
                "a.rb",
                "module M\nend\nmodule P\nend\nclass Base\nend\nAliased = Base\n",
            ),
            (
                "b.rb",
                "module Outer\n  class W < Aliased\n    include M\n    prepend P\n    extend M\n    singleton_class.prepend(P)\n  end\nend\n",
            ),
            ("c.rb", "class Outer::W\nend\nclass Émigré\nend\n"),
        ];
        let (mut decls, mut edges, _) = super::super::core_rows();
        for (path, source) in sources {
            let (d, e, _) = super::super::rows_from(path, source);
            decls.extend(d);
            edges.extend(e);
        }
        super::super::Tree::assemble(decls, edges, &[])
    }

    fn owned(names: &HashMap<String, Entry>, key: &Key) -> Vec<u8> {
        encode(names, key).unwrap()
    }

    fn parse(bytes: Vec<u8>, key: &Key) -> Result<Snapshot, Invalid> {
        Snapshot::parse(Bytes::Owned(bytes), key)
    }

    fn written(t: &Target) -> (String, Vec<String>) {
        (t.name.clone(), t.nesting.clone())
    }

    fn read(t: Written) -> (String, Vec<String>) {
        (
            t.name.to_string(),
            t.nesting.iter().map(|s| s.to_string()).collect(),
        )
    }

    /// The invariant everything rests on: what is read back is what was
    /// assembled, field for field, for every name.
    #[test]
    fn every_name_reads_back_as_it_was_assembled() {
        let names = namespace();
        let key = [7; 20];
        let snap = parse(owned(&names, &key), &key).unwrap();
        assert_eq!(snap.len(), names.len());
        for (fqn, entry) in &names {
            let got = snap.find(fqn).unwrap_or_else(|| panic!("{fqn} missing"));
            assert_eq!(got.fqn(), fqn);
            assert_eq!(got.kind(), entry.kind);
            let sites: Vec<_> = got
                .sites()
                .into_iter()
                .map(|s| (s.path, s.line, s.col, s.kind))
                .collect();
            let want: Vec<_> = entry
                .sites
                .iter()
                .map(|s| (s.path.clone(), s.line, s.col, s.kind.clone()))
                .collect();
            assert_eq!(sites, want, "{fqn}");
            let mixins: Vec<_> = got
                .mixins()
                .into_iter()
                .map(|(k, t)| (k, read(t)))
                .collect();
            let want: Vec<_> = entry
                .mixins
                .iter()
                .map(|m| (m.kind, written(&m.target)))
                .collect();
            assert_eq!(mixins, want, "{fqn}");
            let extends: Vec<_> = got.extends(false).into_iter().map(read).collect();
            assert_eq!(
                extends,
                entry.extends.iter().map(written).collect::<Vec<_>>()
            );
            let prepends: Vec<_> = got.extends(true).into_iter().map(read).collect();
            assert_eq!(
                prepends,
                entry
                    .singleton_prepends
                    .iter()
                    .map(written)
                    .collect::<Vec<_>>()
            );
            assert_eq!(
                got.superclass().map(read),
                entry.superclass.as_ref().map(written)
            );
            assert_eq!(
                got.alias_of().map(read),
                entry.alias_of.as_ref().map(written)
            );
        }
        assert!(snap.find("Nowhere").is_none());
        assert!(snap.find("Outer::W::Nope").is_none());
        let fixture = snap.find("Outer::W").unwrap();
        assert_eq!(fixture.sites().len(), 2, "a reopen is one name, two sites");
        assert!(fixture.superclass().is_some() && fixture.mixins().len() == 2);
        assert_eq!(fixture.extends(true).len(), 1, "a singleton prepend");
    }

    /// Racing builders must write one file: the bytes cannot depend on the
    /// order a hash map happened to hold the names in.
    #[test]
    fn one_namespace_always_encodes_to_the_same_bytes() {
        let key = [1; 20];
        let first = owned(&namespace(), &key);
        for _ in 0..4 {
            assert!(owned(&namespace(), &key) == first);
        }
    }

    #[test]
    fn a_damaged_or_foreign_file_is_refused_with_its_reason() {
        let key = [3; 20];
        let good = owned(&namespace(), &key);
        assert!(parse(good.clone(), &key).is_ok());

        let mut truncated = good.clone();
        truncated.truncate(good.len() - 100);
        assert_eq!(parse(truncated, &key).err(), Some(Invalid::Length));

        let mut newer = good.clone();
        newer[8..12].copy_from_slice(&(FORMAT + 1).to_le_bytes());
        assert_eq!(parse(newer, &key).err(), Some(Invalid::Format(FORMAT + 1)));

        assert_eq!(parse(good.clone(), &[4; 20]).err(), Some(Invalid::Key));

        let mut rotted = good.clone();
        let last = rotted.len() - 1;
        rotted[last] ^= 0x40;
        assert_eq!(parse(rotted, &key).err(), Some(Invalid::Checksum));

        assert_eq!(
            parse(b"not a tree".to_vec(), &key).err(),
            Some(Invalid::Magic)
        );
    }

    #[test]
    fn an_empty_namespace_is_a_valid_one() {
        let key = [0; 20];
        let snap = parse(owned(&HashMap::new(), &key), &key).unwrap();
        assert_eq!(snap.len(), 0);
        assert!(snap.find("Anything").is_none());
    }
}
