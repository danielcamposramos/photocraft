//! Stereo pairs stored in one file, read and written as the specifications that define them say.
//!
//! - **MPO**: CIPA DC-007-2009 (Multi-Picture Format). Individual Images of the MP Type Disparity
//!   (020002.H, §5.2.3.3.1 Table 4) are views of one scene; their MP Individual Image Numbers count
//!   the viewpoints from the leftmost one (§6.2.2.2), so viewpoint 1 is the left eye.
//! - **JPS**: the VRex "General Purpose Stereoscopic Data Descriptor" (1997), an APP3 segment
//!   `_JPSJPS_` with a 32-bit descriptor written most significant byte first: media type (bits
//!   0–7, 1 = stereo), layout (bits 8–15: 1 interleaved, 2 side by side, 3 over/under, 4 anaglyph),
//!   flags (bit 16 half height, bit 17 half width, bit 18 left field first) and the separation
//!   between the fields in pixels (bits 24–31). A `.jps` file without the descriptor is side by
//!   side, full height, right field first (the specification's "Assumptions").
//! - **PNG**: the `sTER` chunk (Extensions to the PNG 1.2 Specification, version 1.4.0, §3.6),
//!   before the first `IDAT`: mode 0 is cross-fuse (right eye on the left), mode 1 diverging-fuse
//!   (left eye on the left); the right subimage starts on a column divisible by eight, with up to
//!   seven padding columns between the two.
//!
//! [`layout`] says where a file keeps its two eyes and [`decode_pair`] reads them as one full
//! side-by-side picture: the left eye in the left half, the right eye in the right half, each at
//! its stored resolution. [`encode_mpo`], [`encode_jps`] and [`encode_png`] write such a picture back.

use std::ops::Range;

use crate::error::CodecError;
use crate::format::{Format, detect};
use crate::image::Image;
use crate::options::{DecodeOptions, EncodeOptions, PngStereo};

/// MP Type Code of a Disparity image (CIPA DC-007-2009 Table 4): one view of a stereo scene.
const MP_TYPE_DISPARITY: u32 = 0x02_0002;
/// Representative Image Flag of an Individual Image Attribute (§5.2.3.3.1, Figure 8).
const MP_REPRESENTATIVE: u32 = 0x2000_0000;
/// MP Index IFD and MP Attribute IFD tags (§5.2.3, §5.2.4 Table 5).
const MPF_VERSION: u16 = 0xB000;
const MPF_NUMBER_OF_IMAGES: u16 = 0xB001;
const MPF_ENTRY: u16 = 0xB002;
const MPF_INDIVIDUAL_NUM: u16 = 0xB101;
const MPF_BASE_VIEWPOINT: u16 = 0xB204;
/// JPS descriptor fields (VRex, "Media Type – SD_MTYPE_STEREOSCOPIC_IMAGE").
const JPS_STEREO: u32 = 0x01;
const JPS_INTERLEAVED: u32 = 0x01;
const JPS_SIDE_BY_SIDE: u32 = 0x02;
const JPS_OVER_UNDER: u32 = 0x03;
const JPS_ANAGLYPH: u32 = 0x04;
const JPS_HALF_HEIGHT: u32 = 0x01;
const JPS_HALF_WIDTH: u32 = 0x02;
const JPS_LEFT_FIRST: u32 = 0x04;
/// MP Entries read at most (an MP file lists one per Individual Image).
const MAX_MP_ENTRIES: usize = 64;

/// The eye a packed pair stores first: in the left half (side by side), the top half (over/under)
/// or the first row (interleaved).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FirstEye {
    Left,
    Right,
}

/// Where a stereo file keeps its two eyes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StereoLayout {
    /// Two Individual Images of an MPO: the byte ranges of the left and the right eye's JPEG.
    Mpo { left: Range<usize>, right: Range<usize> },
    /// An MP file that cannot be represented by this two-view document.
    UnsupportedMpo { reason: &'static str },
    /// One picture with both eyes side by side, `gap` columns apart. `squeezed`: each eye is stored
    /// at half its width (JPS "half width").
    SideBySide { first: FirstEye, gap: u32, squeezed: bool },
    /// One picture with both eyes one above the other, `gap` rows apart. `squeezed`: each eye is
    /// stored at half its height (JPS "half height").
    OverUnder { first: FirstEye, gap: u32, squeezed: bool },
    /// One picture whose rows alternate between the eyes (JPS "interleaved"), starting with `first`.
    Rows { first: FirstEye },
    /// A red/cyan anaglyph. The descriptor identifies the red channel's eye, but the
    /// missing colour channels cannot be recovered as two original full-colour views.
    Anaglyph { red: FirstEye },
    /// A stereo descriptor whose layout or scaling is not implemented; never silently mono.
    UnsupportedJps { descriptor: u32 },
    /// Additional JPS sample-shape flags on a supported packing. Pixels stay at their stored size.
    Scaled { layout: Box<StereoLayout>, pixel_aspect: (u32, u32) },
}

/// The stereo layout `bytes` declare, or `None` for an ordinary (or undecidable) file: an MPO with
/// two Disparity images, a JPEG with a stereo JPS descriptor, or a PNG with `sTER`. `jps` says the
/// file is named `.jps`, which makes a JPEG without a descriptor a cross-eyed side-by-side pair.
pub fn layout(bytes: &[u8], jps: bool) -> Option<StereoLayout> {
    match detect(bytes)? {
        Format::Jpeg => {
            let m = markers(bytes);
            if let Some(l) = m.mpf.as_ref().and_then(|(base, data)| mpo_layout(bytes, *base, data)) {
                return Some(l);
            }
            match m.jps {
                Some(d) => jps_layout(d),
                None if jps => Some(StereoLayout::SideBySide { first: FirstEye::Right, gap: 0, squeezed: false }),
                None => None,
            }
        }
        Format::Png => png_layout(bytes),
        _ => None,
    }
}

/// The APP2 `MPF` data (with the file offset of its MP Endian field) and the APP3 JPS descriptor
/// of a JPEG, read from its marker segments up to the first scan.
#[derive(Default)]
struct Markers<'a> {
    mpf: Option<(usize, &'a [u8])>,
    jps: Option<u32>,
}

fn markers(b: &[u8]) -> Markers<'_> {
    let mut m = Markers::default();
    let mut i = 2usize;
    while let Some(&[ff, marker]) = b.get(i..i + 2).and_then(|s| <&[u8; 2]>::try_from(s).ok()) {
        if ff != 0xFF {
            break;
        }
        if marker == 0xFF {
            i += 1;
            continue;
        }
        if marker == 0xD8 || marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
            i += 2;
            continue;
        }
        if marker == 0xDA || marker == 0xD9 {
            break;
        }
        let Some(len) = b.get(i + 2..i + 4).map(|s| usize::from(u16::from_be_bytes([s[0], s[1]]))) else { break };
        let Some(seg) = (len >= 2).then(|| b.get(i + 4..i + 2 + len)).flatten() else { break };
        match marker {
            0xE2 if m.mpf.is_none() && seg.starts_with(b"MPF\0") => m.mpf = seg.get(4..).map(|d| (i + 8, d)),
            // "_JPSJPS_", block length, then the descriptor (most significant byte first)
            0xE3 if m.jps.is_none() && seg.starts_with(b"_JPSJPS_") => {
                if let Some(n) = seg.get(8..10).map(|d| usize::from(u16::from_be_bytes([d[0], d[1]]))) {
                    m.jps = (n >= 4).then(|| seg.get(10..10 + n)).flatten().and_then(|d| d.get(..4)).map(|d| u32::from_be_bytes([d[0], d[1], d[2], d[3]]));
                }
            }
            _ => {}
        }
        i += 2 + len;
    }
    m
}

/// A TIFF structure (the MP Extensions use one, §5.2): byte order and bounded reads.
struct Tiff<'a> {
    data: &'a [u8],
    le: bool,
}

/// One IFD entry: tag, type, count, and the value field's offset in the data.
struct Entry {
    tag: u16,
    typ: u16,
    count: u32,
    at: usize,
}

impl<'a> Tiff<'a> {
    fn new(data: &'a [u8]) -> Option<Self> {
        let le = match data.get(..4)? {
            [b'I', b'I', 42, 0] => true,
            [b'M', b'M', 0, 42] => false,
            _ => return None,
        };
        Some(Tiff { data, le })
    }
    fn u16(&self, at: usize) -> Option<u16> {
        let b = self.data.get(at..at.checked_add(2)?)?;
        Some(if self.le { u16::from_le_bytes([b[0], b[1]]) } else { u16::from_be_bytes([b[0], b[1]]) })
    }
    fn u32(&self, at: usize) -> Option<u32> {
        let b = self.data.get(at..at.checked_add(4)?)?;
        let a = [b[0], b[1], b[2], b[3]];
        Some(if self.le { u32::from_le_bytes(a) } else { u32::from_be_bytes(a) })
    }
    fn first_ifd(&self) -> Option<usize> {
        usize::try_from(self.u32(4)?).ok()
    }
    /// The entries of the IFD at `at` and the offset of the next IFD (0 when there is none).
    fn ifd(&self, at: usize) -> Option<(Vec<Entry>, usize)> {
        let n = usize::from(self.u16(at)?);
        let entries = (0..n.min(256))
            .map(|k| {
                let e = at.checked_add(2 + 12 * k)?;
                Some(Entry { tag: self.u16(e)?, typ: self.u16(e.checked_add(2)?)?, count: self.u32(e.checked_add(4)?)?, at: e.checked_add(8)? })
            })
            .collect::<Option<Vec<_>>>()?;
        let next = self.u32(at.checked_add(2 + 12 * n)?).and_then(|v| usize::try_from(v).ok()).unwrap_or(0);
        Some((entries, next))
    }
    /// An UNDEFINED (type 7) value of `count` bytes: inline when it fits in four bytes, else at the
    /// offset the entry holds.
    fn bytes(&self, e: &Entry) -> Option<&'a [u8]> {
        let n = usize::try_from(e.count).ok()?;
        if e.typ != 7 {
            return None;
        }
        let at = if n <= 4 { e.at } else { usize::try_from(self.u32(e.at)?).ok()? };
        self.data.get(at..at.checked_add(n)?)
    }
    /// A LONG (type 4) value.
    fn long(&self, e: &Entry) -> Option<u32> {
        (e.typ == 4 && e.count >= 1).then(|| self.u32(e.at)).flatten()
    }
}

/// The MP Individual Image Number in the MP Attribute IFD at `pos`, if there is one.
fn individual_number(t: &Tiff, pos: usize) -> Option<u32> {
    if pos == 0 {
        return None;
    }
    let (entries, _) = t.ifd(pos)?;
    t.long(entries.iter().find(|e| e.tag == MPF_INDIVIDUAL_NUM)?)
}

/// The left and right eye of an MPO: exactly two Disparity images in viewpoint order. A number
/// of 0 marks a composite of several views and FFFFFFFF.H an unknown position (§6.2.2.2):
/// composites are not eyes, and images of unknown (or missing) number keep their order in the MP
/// Index, after the numbered ones.
fn mpo_layout(bytes: &[u8], base: usize, data: &[u8]) -> Option<StereoLayout> {
    let t = Tiff::new(data)?;
    let (entries, attributes) = t.ifd(t.first_ifd()?)?;
    let list = t.bytes(entries.iter().find(|e| e.tag == MPF_ENTRY)?)?;
    if list.len() / 16 > MAX_MP_ENTRIES {
        return Some(StereoLayout::UnsupportedMpo { reason: "MPO has too many images to inspect safely" });
    }
    let mut views: Vec<(u32, usize, Range<usize>)> = Vec::new();
    for (i, rec) in list.as_chunks::<16>().0.iter().enumerate().take(MAX_MP_ENTRIES) {
        let field = |o: usize| {
            let a = [rec[o], rec[o + 1], rec[o + 2], rec[o + 3]];
            if t.le { u32::from_le_bytes(a) } else { u32::from_be_bytes(a) }
        };
        if field(0) & 0xFF_FFFF != MP_TYPE_DISPARITY {
            continue;
        }
        let size = usize::try_from(field(4)).ok()?;
        // the First Individual Image's offset is 0; the others count from the MP header (§5.2.3.3.3)
        let start = if i == 0 { 0 } else { base.checked_add(usize::try_from(field(8)).ok()?)? };
        let range = start..start.checked_add(size)?;
        let Some(image) = bytes.get(range.clone()).filter(|b| b.starts_with(&[0xFF, 0xD8])) else { continue };
        let number = if i == 0 {
            individual_number(&t, attributes)
        } else {
            markers(image).mpf.and_then(|(_, d)| {
                let ti = Tiff::new(d)?;
                individual_number(&ti, ti.first_ifd()?)
            })
        };
        match number {
            Some(0) => continue,
            n => views.push((n.unwrap_or(u32::MAX), i, range)),
        }
    }
    if views.len() > 2 {
        return Some(StereoLayout::UnsupportedMpo { reason: "MPO contains more than two disparity views; a stereo document would discard views" });
    }
    views.sort_by_key(|v| (v.0, v.1));
    let mut views = views.into_iter().map(|v| v.2);
    Some(StereoLayout::Mpo { left: views.next()?, right: views.next()? })
}

/// The packed layout a JPS descriptor declares; `None` for non-stereoscopic media.
fn jps_layout(d: u32) -> Option<StereoLayout> {
    if d & 0xFF != JPS_STEREO {
        return None;
    }
    let flags = (d >> 16) & 0xFF;
    let first = if flags & JPS_LEFT_FIRST != 0 { FirstEye::Left } else { FirstEye::Right };
    let gap = d >> 24;
    let packing = (d >> 8) & 0xFF;
    if packing == JPS_SIDE_BY_SIDE && flags & JPS_HALF_HEIGHT != 0 {
        let plain = d & !((JPS_HALF_HEIGHT | JPS_HALF_WIDTH) << 16);
        return jps_layout(plain).map(|layout| StereoLayout::Scaled {
            layout: Box::new(layout),
            pixel_aspect: (if flags & JPS_HALF_WIDTH != 0 { 2 } else { 1 }, if flags & JPS_HALF_HEIGHT != 0 { 2 } else { 1 }),
        });
    }
    if (packing == JPS_OVER_UNDER && flags & JPS_HALF_WIDTH != 0) || (packing == JPS_INTERLEAVED && flags & (JPS_HALF_HEIGHT | JPS_HALF_WIDTH) != 0) {
        return Some(StereoLayout::UnsupportedJps { descriptor: d });
    }
    match packing {
        JPS_SIDE_BY_SIDE => Some(StereoLayout::SideBySide { first, gap, squeezed: flags & JPS_HALF_WIDTH != 0 }),
        JPS_OVER_UNDER => Some(StereoLayout::OverUnder { first, gap, squeezed: flags & JPS_HALF_HEIGHT != 0 }),
        JPS_INTERLEAVED => Some(StereoLayout::Rows { first }),
        JPS_ANAGLYPH => Some(StereoLayout::Anaglyph { red: first }),
        _ => Some(StereoLayout::UnsupportedJps { descriptor: d }),
    }
}

/// A PNG's `sTER` chunk (before the first `IDAT`) as a side-by-side layout: the padding between
/// the subimages follows from the image width, and a width that needs more than seven padding
/// columns is an error (the chunk is then ignored).
fn png_layout(bytes: &[u8]) -> Option<StereoLayout> {
    // Reuse the upstream CRC and payload validation; geometry still follows §3.6.
    let pairing = crate::png_stereo(bytes)?;
    let mut width = 0u32;
    let mut p = 8usize;
    while let Some(head) = bytes.get(p..p.checked_add(8)?) {
        let len = usize::try_from(u32::from_be_bytes([head[0], head[1], head[2], head[3]])).ok()?;
        let data = bytes.get(p + 8..p.checked_add(8)?.checked_add(len)?)?;
        match &head[4..8] {
            b"IHDR" => width = u32::from_be_bytes([*data.first()?, *data.get(1)?, *data.get(2)?, *data.get(3)?]),
            b"sTER" => {
                if data.len() != 1 || data[0] != pairing.mode() {
                    return None;
                }
                let first = match pairing {
                    PngStereo::CrossFuse => FirstEye::Right,
                    PngStereo::DivergingFuse => FirstEye::Left,
                };
                // padding := 15 - ((W - 1) mod 16); more than 7 is an error
                let gap = 15 - (width.checked_sub(1)? % 16);
                return (gap <= 7 && width >= gap + 2).then_some(StereoLayout::SideBySide { first, gap, squeezed: false });
            }
            b"IDAT" | b"IEND" => return None,
            _ => {}
        }
        p = p.checked_add(12)?.checked_add(len)?;
    }
    None
}

fn bad(format: Format, what: &str) -> CodecError {
    CodecError::malformed(format, what)
}

/// Decode both eyes of a stereo file as one full side-by-side picture: the left eye in the left
/// half, the right eye in the right half. An MPO's Individual Images are decoded (and turned
/// upright) one by one and must have the same size; a packed picture is decoded as stored and
/// split without resampling, and each eye turned upright unless `opts.keep_orientation`.
/// Half-resolution samples keep their intended shape as `Metadata::pixel_aspect`.
/// The metadata (ICC profile, EXIF, XMP, resolution) is that of the first image.
pub fn decode_pair(bytes: &[u8], layout: &StereoLayout, opts: &DecodeOptions) -> Result<Image, CodecError> {
    let (layout, pixel_aspect) = match layout {
        StereoLayout::Scaled { layout, pixel_aspect } => (layout.as_ref(), Some(*pixel_aspect)),
        _ => (layout, None),
    };
    if let StereoLayout::UnsupportedMpo { reason } = layout {
        return Err(CodecError::unsupported(Format::Jpeg, *reason));
    }
    if let StereoLayout::Anaglyph { .. } | StereoLayout::UnsupportedJps { .. } = layout {
        return Err(CodecError::unsupported(Format::Jpeg, "JPS anaglyph or unsupported layout/scaling descriptor"));
    }
    let (left, right, icc, meta, warnings) = match layout {
        StereoLayout::Mpo { left, right } => {
            let get = |r: &Range<usize>| bytes.get(r.clone()).ok_or_else(|| bad(Format::Jpeg, "MPO image outside the file"));
            let (mut l, mut r) = (crate::decode_as_with(Format::Jpeg, get(left)?, opts)?, crate::decode_as_with(Format::Jpeg, get(right)?, opts)?);
            if l.dimensions() != r.dimensions() || l.layout() != r.layout() || l.icc != r.icc || l.meta.pixel_aspect != r.meta.pixel_aspect {
                return Err(CodecError::Unsupported {
                    format: Format::Jpeg,
                    reason: format!(
                        "the MPO's two views differ in geometry, layout or ICC profile ({}×{} and {}×{})",
                        l.width(),
                        l.height(),
                        r.width(),
                        r.height()
                    ),
                });
            }
            // Keep metadata from the first physical image, but report damage to either eye.
            let mut warnings = std::mem::take(&mut l.warnings);
            warnings.append(&mut r.warnings);
            let first = if left.start <= right.start { &mut l } else { &mut r };
            let (icc, meta) = (first.icc.take(), std::mem::take(&mut first.meta));
            (l, r, icc, meta, warnings)
        }
        _ => {
            let format = detect(bytes).ok_or(CodecError::UnknownFormat)?;
            let mut whole = crate::decode_as_with(format, bytes, &DecodeOptions { keep_orientation: true, ..opts.clone() })?;
            // Check the native output before splitting either view.
            let (width, height) = pair_dimensions(whole.width(), whole.height(), layout, format)?;
            opts.limits.check(width, height, whole.layout(), whole.sample_type())?;
            let orientation = whole.meta.exif.as_deref().map_or(1, crate::exif_orientation);
            if !opts.keep_orientation && (5..=8).contains(&orientation) {
                let upright_width = height.checked_mul(2).ok_or_else(|| CodecError::LimitExceeded("stereo pair too wide".into()))?;
                opts.limits.check(upright_width, width / 2, whole.layout(), whole.sample_type())?;
            }
            let (mut l, mut r) = split(&whole, layout, format)?;
            // Orient each view separately: rotating the packed canvas would turn SBS into
            // top/bottom, and mirroring it would exchange the eyes. Clear the source tag too.
            l.meta = std::mem::take(&mut whole.meta);
            l.meta.pixel_aspect = pixel_aspect.or(match layout {
                StereoLayout::SideBySide { squeezed: true, .. } => Some((2, 1)),
                StereoLayout::OverUnder { squeezed: true, .. } | StereoLayout::Rows { .. } => Some((1, 2)),
                _ => l.meta.pixel_aspect,
            });
            if !opts.keep_orientation {
                l = l.oriented(orientation)?;
                r = r.oriented(orientation)?;
            }
            let meta = std::mem::take(&mut l.meta);
            (l, r, whole.icc.take(), meta, std::mem::take(&mut whole.warnings))
        }
    };
    let (w, h) = left.dimensions();
    let full_w = w.checked_mul(2).ok_or_else(|| CodecError::LimitExceeded("stereo pair too wide".into()))?;
    opts.limits.check(full_w, h, left.layout(), left.sample_type())?;
    let mut pair = hstack(&[&left, &right], 0)?;
    pair.icc = icc;
    pair.meta = meta;
    pair.warnings = warnings;
    Ok(pair)
}

fn pair_dimensions(w: u32, h: u32, layout: &StereoLayout, format: Format) -> Result<(u32, u32), CodecError> {
    let invalid = || bad(format, "stereo picture must contain two equal, nonempty views");
    let twice = |n: u32| n.checked_mul(2).ok_or_else(|| CodecError::LimitExceeded("stereo pair dimensions overflow".into()));
    match *layout {
        StereoLayout::SideBySide { gap, .. } => {
            let width = w.checked_sub(gap).filter(|n| *n >= 2 && n.is_multiple_of(2)).ok_or_else(invalid)?;
            Ok((width, h))
        }
        StereoLayout::OverUnder { gap, .. } => {
            let height = h.checked_sub(gap).filter(|n| *n >= 2 && n.is_multiple_of(2)).ok_or_else(invalid)?;
            Ok((twice(w)?, height / 2))
        }
        StereoLayout::Rows { .. } if h >= 2 && h.is_multiple_of(2) => Ok((twice(w)?, h / 2)),
        StereoLayout::Anaglyph { .. } => Err(CodecError::unsupported(format, "anaglyph JPS cannot be recovered as two original full-colour views")),
        _ => Err(invalid()),
    }
}

/// The eyes of a packed picture, as (left, right).
fn split(img: &Image, layout: &StereoLayout, format: Format) -> Result<(Image, Image), CodecError> {
    let (w, h) = img.dimensions();
    let too_small = || bad(format, "stereo picture too small for its layout");
    let (a, b, first) = match *layout {
        StereoLayout::SideBySide { first, gap, .. } => {
            let eye = w.checked_sub(gap).ok_or_else(too_small)? / 2;
            if eye == 0 {
                return Err(too_small());
            }
            let (a, b) = (columns(img, 0, eye)?, columns(img, eye + gap, eye)?);
            (a, b, first)
        }
        StereoLayout::OverUnder { first, gap, .. } => {
            let eye = h.checked_sub(gap).ok_or_else(too_small)? / 2;
            if eye == 0 {
                return Err(too_small());
            }
            let (a, b) = (rows(img, 0, eye, 1)?, rows(img, eye + gap, eye, 1)?);
            (a, b, first)
        }
        StereoLayout::Rows { first } => {
            if h < 2 {
                return Err(too_small());
            }
            let eye = h / 2;
            (rows(img, 0, eye, 2)?, rows(img, 1, eye, 2)?, first)
        }
        StereoLayout::Scaled { ref layout, .. } => return split(img, layout, format),
        StereoLayout::Mpo { .. } | StereoLayout::UnsupportedMpo { .. } => return Err(bad(Format::Jpeg, "an MPO is not one packed picture")),
        StereoLayout::Anaglyph { .. } | StereoLayout::UnsupportedJps { .. } => return Err(CodecError::unsupported(format, "JPS layout")),
    };
    Ok(match first {
        FirstEye::Left => (a, b),
        FirstEye::Right => (b, a),
    })
}

/// Bytes per pixel of `img`.
fn pixel_bytes(img: &Image) -> usize {
    img.layout().channels() * img.sample_type().bytes()
}

/// Columns `x0 .. x0 + w` of `img`.
fn columns(img: &Image, x0: u32, w: u32) -> Result<Image, CodecError> {
    let (iw, h) = img.dimensions();
    let px = pixel_bytes(img);
    let (iw, x0u, wu) = (iw as usize, x0 as usize, w as usize);
    if x0u + wu > iw {
        return Err(CodecError::InvalidImage(format!("columns {x0}..{} outside a {iw}-pixel picture", x0u + wu)));
    }
    let mut out = Vec::with_capacity(wu * h as usize * px);
    for row in img.data().chunks_exact(iw * px) {
        out.extend_from_slice(&row[x0u * px..(x0u + wu) * px]);
    }
    Image::from_raw(w, h, img.layout(), img.sample_type(), out)
}

/// `n` rows of `img` starting at `y0`, `step` rows apart.
fn rows(img: &Image, y0: u32, n: u32, step: u32) -> Result<Image, CodecError> {
    let (w, h) = img.dimensions();
    let row = w as usize * pixel_bytes(img);
    let mut out = Vec::with_capacity(row * n as usize);
    for k in 0..n {
        let y = y0 as usize + (k * step) as usize;
        if y >= h as usize {
            return Err(CodecError::InvalidImage(format!("row {y} outside a {h}-row picture")));
        }
        out.extend_from_slice(&img.data()[y * row..(y + 1) * row]);
    }
    Image::from_raw(w, n, img.layout(), img.sample_type(), out)
}

/// `parts` side by side, `gap` zero columns between neighbours. They must share height, layout
/// and sample type.
fn hstack(parts: &[&Image], gap: u32) -> Result<Image, CodecError> {
    let first = parts.first().ok_or_else(|| CodecError::InvalidImage("nothing to put side by side".into()))?;
    let (h, layout, sample) = (first.height(), first.layout(), first.sample_type());
    if parts.iter().any(|p| p.height() != h || p.layout() != layout || p.sample_type() != sample) {
        return Err(CodecError::InvalidImage("the eyes of a pair differ in height or format".into()));
    }
    let px = pixel_bytes(first);
    let too_large = || CodecError::LimitExceeded("stereo pair size overflows or cannot be allocated".into());
    let gaps = (gap as usize).checked_mul(parts.len().saturating_sub(1)).ok_or_else(too_large)?;
    let w = parts.iter().try_fold(gaps, |n, p| n.checked_add(p.width() as usize)).ok_or_else(too_large)?;
    let out_w = u32::try_from(w).map_err(|_| too_large())?;
    let len = w.checked_mul(h as usize).and_then(|n| n.checked_mul(px)).ok_or_else(too_large)?;
    let mut out = Vec::new();
    out.try_reserve_exact(len).map_err(|_| too_large())?;
    for y in 0..h as usize {
        for (k, p) in parts.iter().enumerate() {
            if k > 0 {
                out.resize(out.len() + gap as usize * px, 0);
            }
            let row = p.width() as usize * px;
            let data = p.data().get(y * row..(y + 1) * row).ok_or_else(|| CodecError::InvalidImage("stereo row outside the image".into()))?;
            out.extend_from_slice(data);
        }
    }
    let mut img = Image::from_raw(out_w, h, layout, sample, out)?;
    img.icc = first.icc.clone();
    img.meta = first.meta.clone();
    Ok(img)
}

// ---------------------------------------------------------------------------------------------
// Writing

/// The two eyes of a full side-by-side picture (left eye in the left half).
fn eyes(sbs: &Image) -> Result<(Image, Image), CodecError> {
    let w = sbs.width();
    if w < 2 || !w.is_multiple_of(2) {
        return Err(CodecError::InvalidImage(format!("a side-by-side pair needs an even width, got {w}")));
    }
    let eye = w / 2;
    let (mut l, mut r) = (columns(sbs, 0, eye)?, columns(sbs, eye, eye)?);
    for e in [&mut l, &mut r] {
        e.icc = sbs.icc.clone();
        e.meta = sbs.meta.clone();
    }
    Ok((l, r))
}

/// Splice `segment` into a JPEG where CIPA DC-007 §4.1 puts the MP Extensions, right after the
/// Exif APP1 segment (else after a leading JFIF APP0, else after SOI). Returns the new JPEG and
/// the segment's offset.
fn splice_after_exif(jpeg: &[u8], segment: &[u8]) -> Result<(Vec<u8>, usize), CodecError> {
    if !jpeg.starts_with(&[0xFF, 0xD8]) {
        return Err(CodecError::encode(Format::Jpeg, "not a JPEG"));
    }
    let mut at = 2usize;
    let mut p = 2usize;
    while let Some(s) = jpeg.get(p..p + 10) {
        if s[0] != 0xFF || !matches!(s[1], 0xE0 | 0xE1) {
            break;
        }
        p += 2 + usize::from(u16::from_be_bytes([s[2], s[3]]));
        if s[1] == 0xE0 || &s[4..10] == b"Exif\0\0" {
            at = p;
        }
        if s[1] == 0xE1 && &s[4..10] == b"Exif\0\0" {
            break;
        }
    }
    let at = at.min(jpeg.len());
    let mut out = Vec::with_capacity(jpeg.len() + segment.len());
    out.extend_from_slice(&jpeg[..at]);
    out.extend_from_slice(segment);
    out.extend_from_slice(&jpeg[at..]);
    Ok((out, at))
}

/// A little-endian IFD writer for the MP Extensions (tags in ascending order; values of more
/// than four bytes go after the IFD).
struct Ifd {
    entries: Vec<(u16, u16, u32, Vec<u8>)>,
}

impl Ifd {
    fn new() -> Self {
        Ifd { entries: Vec::new() }
    }
    fn add(&mut self, tag: u16, typ: u16, count: u32, value: Vec<u8>) -> &mut Self {
        self.entries.push((tag, typ, count, value));
        self
    }
    fn long(&mut self, tag: u16, v: u32) -> &mut Self {
        self.add(tag, 4, 1, v.to_le_bytes().to_vec())
    }
    /// The IFD at offset `at` (from the MP header), its next-IFD field set to `next`.
    fn bytes(&self, at: usize, next: u32) -> Vec<u8> {
        let head = 2 + self.entries.len() * 12 + 4;
        let mut extra: Vec<u8> = Vec::new();
        let mut out = (self.entries.len() as u16).to_le_bytes().to_vec();
        for (tag, typ, count, value) in &self.entries {
            out.extend_from_slice(&tag.to_le_bytes());
            out.extend_from_slice(&typ.to_le_bytes());
            out.extend_from_slice(&count.to_le_bytes());
            if value.len() <= 4 {
                let mut v = value.clone();
                v.resize(4, 0);
                out.extend_from_slice(&v);
            } else {
                out.extend_from_slice(&((at + head + extra.len()) as u32).to_le_bytes());
                extra.extend_from_slice(value);
            }
        }
        out.extend_from_slice(&next.to_le_bytes());
        out.extend_from_slice(&extra);
        out
    }
}

/// MP Attribute IFD of a Disparity image (§5.2.4, Table 8): MP Format Version, MP Individual
/// Image Number (`number`, counted from the leftmost viewpoint, §6.2.2.2) and Base Viewpoint
/// Number 1. Convergence Angle and Baseline Length are only recommended and unknown here, so they
/// are left out.
fn attribute_ifd(number: u32) -> Ifd {
    let mut ifd = Ifd::new();
    ifd.add(MPF_VERSION, 7, 4, b"0100".to_vec()).long(MPF_INDIVIDUAL_NUM, number).long(MPF_BASE_VIEWPOINT, 1);
    ifd
}

/// An APP2 `MPF` segment around an MP header ("II", offset to first IFD 8) and `body`.
fn mpf_segment(body: &[u8]) -> Vec<u8> {
    let mut seg = vec![0xFF, 0xE2];
    seg.extend_from_slice(&((2 + 4 + 8 + body.len()) as u16).to_be_bytes());
    seg.extend_from_slice(b"MPF\0II*\0");
    seg.extend_from_slice(&8u32.to_le_bytes());
    seg.extend_from_slice(body);
    seg
}

fn jpeg_of(img: &Image, opts: &EncodeOptions) -> Result<Vec<u8>, CodecError> {
    crate::encode(img, Format::Jpeg, opts)
}

/// Encode a full side-by-side pair (`sbs`, left eye in the left half) as an MPO (CIPA DC-007): two
/// JPEG Individual Images of the MP Type Disparity, the left eye first (Representative Image,
/// viewpoint 1, with the MP Index IFD) and the right eye second (viewpoint 2). The metadata goes
/// into both.
pub fn encode_mpo(sbs: &Image, opts: &EncodeOptions) -> Result<Vec<u8>, CodecError> {
    let (l, r) = eyes(sbs)?;
    let left = jpeg_of(&l, opts)?;
    let right = jpeg_of(&r, opts)?;
    // the second image: MP header + MP Attribute IFD (Figure 3)
    let (right, _) = splice_after_exif(&right, &mpf_segment(&attribute_ifd(2).bytes(8, 0)))?;
    // the first image: MP header + MP Index IFD + MP Attribute IFD (Figure 2). Its size is fixed
    // before the offsets are known, so lay it out once with zero sizes and once more for real.
    let first = |sizes: (u32, u32, u32)| -> Vec<u8> {
        let mut entries = Vec::with_capacity(32);
        for (attr, size, offset) in [(MP_REPRESENTATIVE | MP_TYPE_DISPARITY, sizes.0, 0), (MP_TYPE_DISPARITY, sizes.1, sizes.2)] {
            entries.extend_from_slice(&attr.to_le_bytes());
            entries.extend_from_slice(&size.to_le_bytes());
            entries.extend_from_slice(&offset.to_le_bytes());
            entries.extend_from_slice(&[0, 0, 0, 0]);
        }
        let mut index = Ifd::new();
        index.add(MPF_VERSION, 7, 4, b"0100".to_vec()).long(MPF_NUMBER_OF_IMAGES, 2).add(MPF_ENTRY, 7, 32, entries);
        let index_len = index.bytes(8, 0).len();
        let mut body = index.bytes(8, (8 + index_len) as u32);
        body.extend_from_slice(&attribute_ifd(1).bytes(8 + index_len, 0));
        mpf_segment(&body)
    };
    let (probe, at) = splice_after_exif(&left, &first((0, 0, 0)))?;
    let total = probe.len();
    // offsets count from the MP Endian field: segment marker (2) + length (2) + "MPF\0" (4)
    let header = at + 8;
    let too_large = || CodecError::encode(Format::Jpeg, "MPO too large");
    let offset = u32::try_from(total - header).map_err(|_| too_large())?;
    let size = |n: usize| u32::try_from(n).map_err(|_| too_large());
    let (mut out, _) = splice_after_exif(&left, &first((size(total)?, size(right.len())?, offset)))?;
    out.extend_from_slice(&right);
    Ok(out)
}

/// Encode a full side-by-side pair (`sbs`, left eye in the left half) as a JPS: one JPEG holding
/// the right eye on the left and the left eye on the right, the cross-eyed arrangement readers
/// assume without a descriptor. The APP3 descriptor carries the eye order and applicable
/// half-width/half-height flags; other sample shapes use the JPEG's own density fields.
pub fn encode_jps(sbs: &Image, opts: &EncodeOptions) -> Result<Vec<u8>, CodecError> {
    let (l, r) = eyes(sbs)?;
    let mut cross = hstack(&[&r, &l], 0)?;
    cross.meta = sbs.meta.clone();
    let shape = match sbs.meta.pixel_aspect {
        Some((2, 1)) => JPS_HALF_WIDTH,
        Some((1, 2)) => JPS_HALF_HEIGHT,
        _ => 0,
    };
    if shape != 0 {
        cross.meta.pixel_aspect = None;
    }
    let jpeg = jpeg_of(&cross, opts)?;
    let descriptor = JPS_STEREO | (JPS_SIDE_BY_SIDE << 8) | (shape << 16);
    let mut seg = vec![0xFF, 0xE3];
    let body_len = 8 + 2 + 4 + 2;
    seg.extend_from_slice(&((2 + body_len) as u16).to_be_bytes());
    seg.extend_from_slice(b"_JPSJPS_");
    seg.extend_from_slice(&4u16.to_be_bytes());
    seg.extend_from_slice(&descriptor.to_be_bytes());
    seg.extend_from_slice(&0u16.to_be_bytes()); // no comment
    Ok(splice_after_exif(&jpeg, &seg)?.0)
}

/// Encode a full side-by-side pair (`sbs`, left eye in the left half) as a PNG with `sTER` mode 1
/// (diverging-fuse, left eye on the left). When the eye width is not a multiple of eight, the
/// padding columns the chunk requires (zero samples) separate the two subimages.
pub fn encode_png(sbs: &Image, opts: &EncodeOptions) -> Result<Vec<u8>, CodecError> {
    let (l, r) = eyes(sbs)?;
    // padding := 7 - ((subimage_width - 1) mod 8)
    let gap = 7 - ((l.width() - 1) % 8);
    let mut packed = hstack(&[&l, &r], gap)?;
    packed.meta = sbs.meta.clone();
    packed.icc = sbs.icc.clone();
    // The ordinary PNG encoder now owns sTER writing (#6285). Override the source's
    // byte after normalization; inserting another chunk would leave conflicting eye orders.
    let opts = EncodeOptions { png_stereo: Some(PngStereo::DivergingFuse), ..opts.clone() };
    crate::encode(&packed, Format::Png, &opts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SampleType;

    #[test]
    fn jps_descriptor_values() {
        assert_eq!(jps_layout(0x0000_0201), Some(StereoLayout::SideBySide { first: FirstEye::Right, gap: 0, squeezed: false }));
        assert_eq!(jps_layout(0x0004_0201), Some(StereoLayout::SideBySide { first: FirstEye::Left, gap: 0, squeezed: false }));
        assert_eq!(jps_layout(0x0602_0201), Some(StereoLayout::SideBySide { first: FirstEye::Right, gap: 6, squeezed: true }));
        assert_eq!(jps_layout(0x0005_0301), Some(StereoLayout::OverUnder { first: FirstEye::Left, gap: 0, squeezed: true }));
        assert_eq!(jps_layout(0x0000_0101), Some(StereoLayout::Rows { first: FirstEye::Right }));
        // Anaglyph is stereo too; bit 18 identifies its red field's eye.
        assert_eq!(jps_layout(0x0000_0401), Some(StereoLayout::Anaglyph { red: FirstEye::Right }));
        assert_eq!(jps_layout(0x0004_0401), Some(StereoLayout::Anaglyph { red: FirstEye::Left }));
        assert_eq!(jps_layout(0x0000_0000), None);
        assert_eq!(jps_layout(0x0000_0901), Some(StereoLayout::UnsupportedJps { descriptor: 0x0000_0901 }));
    }

    #[test]
    fn reduced_resolution_views_keep_their_sample_grids() {
        use crate::ChannelLayout;
        let cases = [
            (6, 2, StereoLayout::SideBySide { first: FirstEye::Left, gap: 0, squeezed: true }, [0, 1, 2, 6, 7, 8], [3, 4, 5, 9, 10, 11]),
            (3, 4, StereoLayout::OverUnder { first: FirstEye::Left, gap: 0, squeezed: true }, [0, 1, 2, 3, 4, 5], [6, 7, 8, 9, 10, 11]),
            (3, 4, StereoLayout::Rows { first: FirstEye::Left }, [0, 1, 2, 6, 7, 8], [3, 4, 5, 9, 10, 11]),
        ];
        for sample in [SampleType::U8, SampleType::U16, SampleType::F16, SampleType::F32] {
            let pixels: Vec<u8> = (0..12u8)
                .flat_map(|i| match sample {
                    SampleType::U8 => vec![i],
                    SampleType::U16 => (u16::from(i) * 1000).to_ne_bytes().to_vec(),
                    SampleType::F16 => half::f16::from_f32(f32::from(i) / 16.0).to_bits().to_ne_bytes().to_vec(),
                    SampleType::F32 => (f32::from(i) / 16.0).to_ne_bytes().to_vec(),
                })
                .collect();
            for (w, h, layout, left, right) in &cases {
                assert_eq!(pair_dimensions(*w, *h, layout, Format::Jpeg).unwrap(), (6, 2));
                let image = Image::from_raw(*w, *h, ChannelLayout::Gray, sample, pixels.clone()).unwrap();
                let (l, r) = split(&image, layout, Format::Jpeg).unwrap();
                for (eye, indices) in [(&l, left), (&r, right)] {
                    assert_eq!(eye.dimensions(), (3, 2), "{layout:?} {sample:?}");
                    let expected: Vec<_> = indices
                        .iter()
                        .flat_map(|i| {
                            let offset = *i as usize * sample.bytes();
                            pixels[offset..offset + sample.bytes()].iter().copied()
                        })
                        .collect();
                    assert_eq!(eye.data(), expected, "{layout:?} {sample:?}");
                }
            }
        }
    }
}
