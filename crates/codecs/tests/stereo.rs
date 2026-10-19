//! Stereo files: where the eyes are (MPO, JPS, PNG `sTER`), reading both as one full side-by-side
//! picture, and writing them back. Fixtures are built here byte by byte from the specifications
//! (CIPA DC-007-2009, the VRex stereoscopic descriptor, PNG Extensions 1.4.0 §3.6); each eye has
//! its own colour (left red, right blue) so a swapped pair shows.

use photocraft_codecs::stereo::{self, FirstEye, StereoLayout};
use photocraft_codecs::*;

const RED: [u8; 3] = [200, 30, 30];
const BLUE: [u8; 3] = [30, 30, 200];
const GREEN: [u8; 3] = [30, 200, 30];

/// A `w`×`h` 8-bit RGB picture coloured by `f(x, y)`.
fn picture(w: u32, h: u32, f: impl Fn(u32, u32) -> [u8; 3]) -> Image {
    let f = &f;
    let data = (0..h).flat_map(|y| (0..w).flat_map(move |x| f(x, y))).collect::<Vec<u8>>();
    Image::from_u8(w, h, ChannelLayout::Rgb, data).unwrap()
}

fn filled(w: u32, h: u32, c: [u8; 3]) -> Image {
    picture(w, h, |_, _| c)
}

/// Two pictures side by side, `gap` black columns between them.
fn side_by_side(a: &Image, b: &Image, gap: u32) -> Image {
    picture(a.width() + gap + b.width(), a.height(), |x, y| {
        let px = |img: &Image, x: u32| {
            let i = ((y * img.width() + x) * 3) as usize;
            [img.data()[i], img.data()[i + 1], img.data()[i + 2]]
        };
        if x < a.width() {
            px(a, x)
        } else if x < a.width() + gap {
            [0, 0, 0]
        } else {
            px(b, x - a.width() - gap)
        }
    })
}

fn jpeg(img: &Image) -> Vec<u8> {
    encode(img, Format::Jpeg, &EncodeOptions { jpeg_quality: 95, ..Default::default() }).unwrap()
}

fn png(img: &Image) -> Vec<u8> {
    encode(img, Format::Png, &EncodeOptions::default()).unwrap()
}

/// Mean RGB of columns `x0..x1` of a picture (any layout with RGB first, 8-bit).
fn mean(img: &Image, x0: u32, x1: u32) -> [u32; 3] {
    let rgb = img.convert(ChannelLayout::Rgb, SampleType::U8);
    let (w, h) = rgb.dimensions();
    let mut sum = [0u64; 3];
    for y in 0..h {
        for x in x0..x1 {
            let i = ((y * w + x) * 3) as usize;
            for (c, s) in sum.iter_mut().enumerate() {
                *s += u64::from(rgb.data()[i + c]);
            }
        }
    }
    let n = u64::from((x1 - x0) * h).max(1);
    [(sum[0] / n) as u32, (sum[1] / n) as u32, (sum[2] / n) as u32]
}

fn is_red(m: [u32; 3]) -> bool {
    m[0] > 150 && m[2] < 80
}

fn is_blue(m: [u32; 3]) -> bool {
    m[2] > 150 && m[0] < 80
}

/// A pair read back: twice `eye` wide, the left half red, the right half blue.
fn assert_pair(img: &Image, eye: (u32, u32), what: &str) {
    assert_eq!(img.dimensions(), (eye.0 * 2, eye.1), "{what}: size");
    let (l, r) = (mean(img, 0, eye.0), mean(img, eye.0, eye.0 * 2));
    assert!(is_red(l) && is_blue(r), "{what}: left {l:?} right {r:?}");
}

fn read(bytes: &[u8], jps: bool) -> Image {
    let layout = stereo::layout(bytes, jps).expect("a stereo layout");
    stereo::decode_pair(bytes, &layout, &DecodeOptions::default()).unwrap()
}

// --- MPO (CIPA DC-007-2009) ---------------------------------------------------------------

/// An IFD (little endian) at `at`: entries (tag, type, count, value bytes); values over 4 bytes
/// follow the IFD.
fn ifd(at: usize, entries: &[(u16, u16, u32, Vec<u8>)], next: u32) -> Vec<u8> {
    let head = 2 + entries.len() * 12 + 4;
    let mut out = (entries.len() as u16).to_le_bytes().to_vec();
    let mut extra = Vec::new();
    for (tag, typ, count, v) in entries {
        out.extend_from_slice(&tag.to_le_bytes());
        out.extend_from_slice(&typ.to_le_bytes());
        out.extend_from_slice(&count.to_le_bytes());
        if v.len() <= 4 {
            let mut v = v.clone();
            v.resize(4, 0);
            out.extend_from_slice(&v);
        } else {
            out.extend_from_slice(&((at + head + extra.len()) as u32).to_le_bytes());
            extra.extend_from_slice(v);
        }
    }
    out.extend_from_slice(&next.to_le_bytes());
    out.extend_from_slice(&extra);
    out
}

/// MP Attribute IFD with an MP Individual Image Number (viewpoint) when given.
fn attributes(at: usize, number: Option<u32>) -> Vec<u8> {
    let mut e = vec![(0xB000u16, 7u16, 4u32, b"0100".to_vec())];
    if let Some(n) = number {
        e.push((0xB101, 4, 1, n.to_le_bytes().to_vec()));
        e.push((0xB204, 4, 1, 1u32.to_le_bytes().to_vec()));
    }
    ifd(at, &e, 0)
}

fn app2(body: &[u8]) -> Vec<u8> {
    let mut s = vec![0xFF, 0xE2];
    s.extend_from_slice(&((2 + 4 + 8 + body.len()) as u16).to_be_bytes());
    s.extend_from_slice(b"MPF\0II*\0");
    s.extend_from_slice(&8u32.to_le_bytes());
    s.extend_from_slice(body);
    s
}

/// Insert `seg` right after SOI.
fn after_soi(jpeg: &[u8], seg: &[u8]) -> Vec<u8> {
    [&jpeg[..2], seg, &jpeg[2..]].concat()
}

/// An MP file from (jpeg, Individual Image Attribute, MP Individual Image Number) per image.
fn mpo(images: &[(&[u8], u32, Option<u32>)]) -> Vec<u8> {
    let n = images.len();
    let first = |sizes: &[u32], offsets: &[u32]| -> Vec<u8> {
        let mut entries = Vec::new();
        for (i, img) in images.iter().enumerate() {
            entries.extend_from_slice(&img.1.to_le_bytes());
            entries.extend_from_slice(&sizes[i].to_le_bytes());
            entries.extend_from_slice(&offsets[i].to_le_bytes());
            entries.extend_from_slice(&[0, 0, 0, 0]);
        }
        let index_entries = [(0xB000u16, 7u16, 4u32, b"0100".to_vec()), (0xB001, 4, 1, (n as u32).to_le_bytes().to_vec()), (0xB002, 7, 16 * n as u32, entries)];
        let len = ifd(8, &index_entries, 0).len();
        let mut body = ifd(8, &index_entries, (8 + len) as u32);
        body.extend_from_slice(&attributes(8 + len, images[0].2));
        app2(&body)
    };
    let others: Vec<Vec<u8>> = images[1..].iter().map(|img| after_soi(img.0, &app2(&attributes(8, img.2)))).collect();
    let zeros = vec![0u32; n];
    let first_len = images[0].0.len() + first(&zeros, &zeros).len();
    let header = 2 + 8; // SOI, then the segment's FFE2 + length + "MPF\0"
    let mut sizes = vec![first_len as u32];
    let mut offsets = vec![0u32];
    let mut at = first_len;
    for o in &others {
        sizes.push(o.len() as u32);
        offsets.push((at - header) as u32);
        at += o.len();
    }
    let mut out = after_soi(images[0].0, &first(&sizes, &offsets));
    assert_eq!(out.len(), first_len);
    for o in &others {
        out.extend_from_slice(o);
    }
    out
}

const DISPARITY: u32 = 0x02_0002;
const REPRESENTATIVE: u32 = 0x2000_0000;

#[test]
fn mpo_eyes_follow_the_viewpoint_numbers() {
    let left = jpeg(&filled(64, 48, RED));
    let right = jpeg(&filled(64, 48, BLUE));
    // the usual order: viewpoint 1 (left) first
    let bytes = mpo(&[(&left, REPRESENTATIVE | DISPARITY, Some(1)), (&right, DISPARITY, Some(2))]);
    assert_pair(&read(&bytes, false), (64, 48), "viewpoints 1, 2");
    // §6.2.2.2: numbered from the leftmost viewpoint, whatever the order of the images
    let bytes = mpo(&[(&right, REPRESENTATIVE | DISPARITY, Some(2)), (&left, DISPARITY, Some(1))]);
    let Some(StereoLayout::Mpo { left: lr, right: rr }) = stereo::layout(&bytes, false) else { panic!("not an MPO layout") };
    assert!(lr.start > 0 && rr.start == 0, "the second image is viewpoint 1: {lr:?} {rr:?}");
    assert_pair(&read(&bytes, false), (64, 48), "viewpoints 2, 1");
    // no numbers: the order of the MP Index
    let bytes = mpo(&[(&left, REPRESENTATIVE | DISPARITY, None), (&right, DISPARITY, None)]);
    assert_pair(&read(&bytes, false), (64, 48), "no numbers");
    // number 0 is a composite of several views, not an eye
    let composite = jpeg(&filled(128, 48, GREEN));
    let bytes = mpo(&[(&composite, REPRESENTATIVE | DISPARITY, Some(0)), (&left, DISPARITY, Some(1)), (&right, DISPARITY, Some(2))]);
    assert_pair(&read(&bytes, false), (64, 48), "after a composite");
    let bytes = mpo(&[(&left, REPRESENTATIVE | DISPARITY, Some(1)), (&composite, DISPARITY, Some(0))]);
    assert_eq!(stereo::layout(&bytes, false), None, "one eye and a composite are not a pair");
}

#[test]
fn only_disparity_images_make_a_stereo_mpo() {
    let a = jpeg(&filled(64, 48, RED));
    let b = jpeg(&filled(64, 48, BLUE));
    // Baseline MP file (primary + Large Thumbnail), a Pillow-style MPO (primary + undefined),
    // multi-angle and panorama sets, a single disparity image: not a stereo pair
    for (t1, t2) in [(0x03_0000, 0x01_0001), (0x03_0000, 0x00_0000), (0x02_0003, 0x02_0003), (0x02_0001, 0x02_0001), (DISPARITY, 0x01_0002)] {
        let bytes = mpo(&[(&a, REPRESENTATIVE | t1, Some(1)), (&b, t2, Some(2))]);
        assert_eq!(stereo::layout(&bytes, false), None, "{t1:06X} + {t2:06X}");
    }
    assert_eq!(stereo::layout(&a, false), None, "a plain JPEG");
}

#[test]
fn mpo_views_of_different_sizes_are_refused() {
    let left = jpeg(&filled(64, 48, RED));
    let right = jpeg(&filled(80, 60, BLUE));
    let bytes = mpo(&[(&left, REPRESENTATIVE | DISPARITY, Some(1)), (&right, DISPARITY, Some(2))]);
    let layout = stereo::layout(&bytes, false).unwrap();
    let e = stereo::decode_pair(&bytes, &layout, &DecodeOptions::default()).unwrap_err();
    assert!(e.to_string().contains("differ"), "{e}");
}

// --- JPS (VRex stereoscopic descriptor) --------------------------------------------------------

/// A JPS: `picture` with an APP3 `_JPSJPS_` descriptor (most significant byte first).
fn with_descriptor(picture: &[u8], descriptor: u32) -> Vec<u8> {
    let mut seg = vec![0xFF, 0xE3, 0, 18];
    seg.extend_from_slice(b"_JPSJPS_");
    seg.extend_from_slice(&4u16.to_be_bytes());
    seg.extend_from_slice(&descriptor.to_be_bytes());
    seg.extend_from_slice(&0u16.to_be_bytes());
    after_soi(picture, &seg)
}

#[test]
fn jps_without_descriptor_is_cross_eyed_side_by_side() {
    // "If a JPEG file is thought to contain a stereo image, but a stereoscopic descriptor is not
    // present, ... Side-by-Side, Full Height, Right Field First."
    let cross = jpeg(&side_by_side(&filled(64, 48, BLUE), &filled(64, 48, RED), 0));
    let layout = stereo::layout(&cross, true).expect("a .jps");
    assert_eq!(layout, StereoLayout::SideBySide { first: FirstEye::Right, gap: 0, squeezed: false });
    assert_pair(&read(&cross, true), (64, 48), "cross-eyed");
    // the same picture named .jpg is an ordinary image
    assert_eq!(stereo::layout(&cross, false), None);
}

#[test]
fn jps_descriptor_layouts() {
    let (l, r) = (filled(64, 48, RED), filled(64, 48, BLUE));
    // stereo (01), side by side (02), left field first (bit 18): 00 04 02 01
    let parallel = with_descriptor(&jpeg(&side_by_side(&l, &r, 0)), 0x0004_0201);
    // over/under (03), left field first, on top
    let over = with_descriptor(&jpeg(&picture(64, 96, |_, y| if y < 48 { RED } else { BLUE })), 0x0004_0301);
    // separation 8 pixels between the fields (bits 24-31), right field first
    let separated = with_descriptor(&jpeg(&side_by_side(&r, &l, 8)), 0x0800_0201);
    // half width (bit 17): two squeezed 32-pixel eyes in a 64-pixel picture
    let squeezed = with_descriptor(&jpeg(&side_by_side(&filled(32, 48, RED), &filled(32, 48, BLUE), 0)), 0x0006_0201);
    for (name, bytes) in [("parallel", parallel), ("over/under", over), ("separated", separated), ("squeezed", squeezed)] {
        assert_pair(&read(&bytes, false), (64, 48), name);
    }
    // interleaved rows (01), left field first: even rows left, each eye stretched back to the
    // picture's height. The eyes differ in brightness: JPEG's chroma subsampling would smear
    // colours between single rows.
    let rows = with_descriptor(&jpeg(&picture(64, 96, |_, y| if y % 2 == 0 { [220; 3] } else { [30; 3] })), 0x0004_0101);
    assert_eq!(stereo::layout(&rows, false), Some(StereoLayout::Rows { first: FirstEye::Left }));
    let pair = read(&rows, false);
    assert_eq!(pair.dimensions(), (128, 96));
    let (le, re) = (mean(&pair, 0, 64), mean(&pair, 64, 128));
    assert!(le[0] > 180 && re[0] < 70, "left {le:?} right {re:?}");
    // A monoscopic descriptor stays flat. Anaglyph is recognized, but must not silently
    // become a mono document or claim to recover the missing colour channels.
    let flat = jpeg(&l);
    assert_eq!(stereo::layout(&with_descriptor(&flat, 0x0000_0000), true), None);
    let anaglyph = with_descriptor(&flat, 0x0004_0401);
    let layout = stereo::layout(&anaglyph, true).unwrap();
    assert_eq!(layout, StereoLayout::Anaglyph { red: FirstEye::Left });
    assert!(matches!(stereo::decode_pair(&anaglyph, &layout, &DecodeOptions::default()), Err(photocraft_codecs::CodecError::Unsupported { .. })));
}

// --- PNG sTER (PNG Extensions 1.4.0 §3.6) ---------------------------------------------------------

fn crc(data: &[u8]) -> u32 {
    let mut c = 0xFFFF_FFFFu32;
    for b in data {
        c ^= *b as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
        }
    }
    !c
}

/// Insert a `sTER` chunk before the chunk named `before`.
fn with_ster(png: &[u8], mode: u8, before: &[u8; 4]) -> Vec<u8> {
    let mut p = 8;
    loop {
        let len = u32::from_be_bytes(png[p..p + 4].try_into().unwrap()) as usize;
        if &png[p + 4..p + 8] == before {
            let mut chunk = 1u32.to_be_bytes().to_vec();
            chunk.extend_from_slice(b"sTER");
            chunk.push(mode);
            chunk.extend_from_slice(&crc(&[b"sTER".as_slice(), &[mode]].concat()).to_be_bytes());
            return [&png[..p], &chunk, &png[p..]].concat();
        }
        p += 12 + len;
    }
}

#[test]
fn png_ster_modes_and_padding() {
    let (l, r) = (filled(16, 8, RED), filled(16, 8, BLUE));
    // mode 1, diverging-fuse: the left eye on the left; mode 0, cross-fuse: the right eye on the left
    let diverging = with_ster(&png(&side_by_side(&l, &r, 0)), 1, b"IDAT");
    let cross = with_ster(&png(&side_by_side(&r, &l, 0)), 0, b"IDAT");
    for (name, bytes) in [("mode 1", diverging), ("mode 0", cross)] {
        assert_pair(&read(&bytes, false), (16, 8), name);
    }
    // 13-pixel eyes: padding = 7 - ((13 - 1) mod 8) = 3 columns, so W = 29 and the right
    // subimage starts at column 16; the decoder recovers padding = 15 - ((29 - 1) mod 16) = 3
    let odd = with_ster(&png(&side_by_side(&filled(13, 8, RED), &filled(13, 8, BLUE), 3)), 1, b"IDAT");
    assert_eq!(stereo::layout(&odd, false), Some(StereoLayout::SideBySide { first: FirstEye::Left, gap: 3, squeezed: false }));
    assert_pair(&read(&odd, false), (13, 8), "padded");
    // a width that would need more than seven padding columns is an error: not a stereo pair
    assert_eq!(stereo::layout(&with_ster(&png(&filled(33, 8, RED)), 1, b"IDAT"), false), None);
    // sTER must come before the first IDAT
    assert_eq!(stereo::layout(&with_ster(&png(&side_by_side(&l, &r, 0)), 1, b"IEND"), false), None);
    assert_eq!(stereo::layout(&png(&l), false), None);
}

#[test]
fn sixteen_bit_and_alpha_png_pairs_keep_their_samples() {
    // a 16-bit RGBA pair with 4-pixel eyes (the writer adds 4 padding columns): every sample comes
    // back unchanged
    let px = |x: u16| if x < 4 { [60000u16, 0, 5535, 40000] } else { [1000, 30000, 64535, 65535] };
    let samples: Vec<u16> = (0..2).flat_map(|_| (0..8u16).flat_map(px)).collect();
    let pair = Image::from_u16(8, 2, ChannelLayout::Rgba, &samples).unwrap();
    let bytes = stereo::encode_png(&pair, &EncodeOptions::default()).unwrap();
    let back = read(&bytes, false);
    assert_eq!((back.dimensions(), back.layout(), back.sample_type()), ((8, 2), ChannelLayout::Rgba, SampleType::U16));
    assert_eq!(back.to_u16_samples(), pair.to_u16_samples());
}

// --- writing ---------------------------------------------------------------------------------------

fn sbs(eye_w: u32, h: u32) -> Image {
    side_by_side(&filled(eye_w, h, RED), &filled(eye_w, h, BLUE), 0)
}

/// A minimal little-endian EXIF block (IFD0 with one Orientation entry).
fn exif() -> Vec<u8> {
    let mut e = b"II*\0".to_vec();
    e.extend_from_slice(&8u32.to_le_bytes());
    e.extend_from_slice(&1u16.to_le_bytes());
    e.extend_from_slice(&0x0112u16.to_le_bytes());
    e.extend_from_slice(&3u16.to_le_bytes());
    e.extend_from_slice(&1u32.to_le_bytes());
    e.extend_from_slice(&[1, 0, 0, 0]);
    e.extend_from_slice(&0u32.to_le_bytes());
    e
}

#[test]
fn mpo_written_and_read_back() {
    let mut pair = sbs(64, 48);
    pair.meta.exif = Some(exif());
    let bytes = stereo::encode_mpo(&pair, &EncodeOptions::default()).unwrap();
    let layout = stereo::layout(&bytes, false).expect("our MPO is a stereo MPO");
    let StereoLayout::Mpo { left, right } = &layout else { panic!("{layout:?}") };
    assert_eq!(left.start, 0, "the left eye is the first image");
    assert_pair(&read(&bytes, false), (64, 48), "MPO");
    // the file as ordinary JPEG readers see it: the left eye
    let first = decode(&bytes).unwrap();
    assert_eq!(first.dimensions(), (64, 48));
    assert!(is_red(mean(&first, 0, 64)));
    // CIPA DC-007 bytes: the MP Extensions follow the Exif APP1; MP Entry 1 is a Representative
    // Disparity image (20020002.H), entry 2 a Disparity image (00020002.H) starting where the
    // first image ends; each image carries its MP Individual Image Number (1, 2)
    let exif_at = bytes.windows(6).position(|w| w == b"Exif\0\0").unwrap();
    let exif_end = exif_at - 2 + u16::from_be_bytes([bytes[exif_at - 2], bytes[exif_at - 1]]) as usize;
    assert_eq!(&bytes[exif_end..exif_end + 2], &[0xFF, 0xE2], "APP2 right after the Exif APP1");
    assert_eq!(&bytes[exif_end + 4..exif_end + 8], b"MPF\0");
    let le = |o: usize| u32::from_le_bytes(bytes[o..o + 4].try_into().unwrap());
    let header = exif_end + 8;
    let entries = header + 8 + 2 + 3 * 12 + 4;
    assert_eq!(le(entries), 0x2002_0002);
    assert_eq!(le(entries + 16), 0x0002_0002);
    assert_eq!(le(entries + 16 + 8) as usize + header, right.start);
    assert_eq!(le(entries + 4) as usize, right.start, "the first image's size covers it whole");
    let mpfs: Vec<usize> = bytes.windows(4).enumerate().filter(|(_, w)| *w == b"MPF\0").map(|(i, _)| i).collect();
    assert_eq!(mpfs.len(), 2, "one MPF segment per image");
    assert!(mpfs[1] > right.start);
    let num_at = |from: usize| {
        let p = from + bytes[from..].windows(2).position(|w| w == [0x01, 0xB1]).unwrap();
        le(p + 8)
    };
    assert_eq!(num_at(entries + 32), 1);
    assert_eq!(num_at(mpfs[1]), 2);
}

#[test]
fn jps_written_and_read_back() {
    let bytes = stereo::encode_jps(&sbs(64, 48), &EncodeOptions::default()).unwrap();
    // APP3 "_JPSJPS_", block length 4, descriptor 00 00 02 01 (stereo, side by side, full size,
    // right field first, no separation), most significant byte first
    let at = bytes.windows(8).position(|w| w == b"_JPSJPS_").expect("JPS descriptor");
    assert_eq!(&bytes[at - 4..at - 2], &[0xFF, 0xE3]);
    assert_eq!(&bytes[at + 8..at + 14], &[0, 4, 0, 0, 2, 1]);
    // the picture itself is cross-eyed: right eye on the left
    let whole = decode(&bytes).unwrap();
    assert_eq!(whole.dimensions(), (128, 48));
    assert!(is_blue(mean(&whole, 0, 64)), "right eye first");
    assert_pair(&read(&bytes, false), (64, 48), "JPS");
}

#[test]
fn png_written_and_read_back() {
    for eye in [16u32, 13] {
        let bytes = stereo::encode_png(&sbs(eye, 8), &EncodeOptions::default()).unwrap();
        // sTER mode 1 before the first IDAT, with a valid CRC
        let s = bytes.windows(4).position(|w| w == b"sTER").unwrap();
        let idat = bytes.windows(4).position(|w| w == b"IDAT").unwrap();
        assert!(s < idat);
        assert_eq!(&bytes[s - 4..s], &1u32.to_be_bytes());
        assert_eq!(bytes[s + 4], 1);
        assert_eq!(&bytes[s + 5..s + 9], &crc(&bytes[s..s + 5]).to_be_bytes());
        // the padding the chunk needs: none for 16-pixel eyes, 3 columns for 13-pixel ones
        let whole = decode(&bytes).unwrap();
        assert_eq!(whole.width(), 2 * eye + 7 - ((eye - 1) % 8), "eye {eye}");
        assert_pair(&read(&bytes, false), (eye, 8), "PNG");
    }
}

#[test]
fn normalized_png_overrides_the_source_mode_and_writes_only_one_ster() {
    for mode in [PngStereo::CrossFuse, PngStereo::DivergingFuse] {
        let options = EncodeOptions { png_stereo: Some(mode), ..Default::default() };
        let bytes = stereo::encode_png(&sbs(13, 8), &options).unwrap();
        let mut offset = 8;
        let mut marks = Vec::new();
        while offset + 12 <= bytes.len() {
            let len = u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
            if &bytes[offset + 4..offset + 8] == b"sTER" {
                marks.push(bytes[offset + 8..offset + 8 + len].to_vec());
            }
            offset += len + 12;
        }
        assert_eq!(marks, vec![vec![1]], "one diverging-fuse marker for normalized pixels");
        assert_pair(&read(&bytes, false), (13, 8), "source settings must not swap the eyes");
    }
}

#[test]
fn damaged_ster_is_not_used_to_rearrange_pixels() {
    let mut bytes = with_ster(&png(&sbs(16, 8)), 1, b"IDAT");
    let at = bytes.windows(4).position(|w| w == b"sTER").unwrap();
    bytes[at + 8] ^= 1; // corrupt only the CRC
    assert_eq!(stereo::layout(&bytes, false), None);
}

#[test]
fn writers_need_a_side_by_side_pair() {
    let odd = filled(9, 4, GREEN);
    let o = EncodeOptions::default();
    assert!(stereo::encode_mpo(&odd, &o).is_err());
    assert!(stereo::encode_jps(&odd, &o).is_err());
    assert!(stereo::encode_png(&odd, &o).is_err());
}

#[test]
fn truncated_and_damaged_stereo_files_never_panic() {
    let pair = sbs(32, 16);
    let o = EncodeOptions::default();
    let files = [stereo::encode_mpo(&pair, &o).unwrap(), stereo::encode_jps(&pair, &o).unwrap(), stereo::encode_png(&pair, &o).unwrap()];
    for f in &files {
        for cut in (0..f.len()).step_by(7) {
            let b = &f[..cut];
            if let Some(l) = stereo::layout(b, true) {
                let _ = stereo::decode_pair(b, &l, &DecodeOptions::default());
            }
        }
        // a flipped bit in each of the first 600 bytes (markers, MPF IFDs, chunk headers)
        for bit in 0..(f.len().min(600) * 8) {
            let mut m = f.clone();
            m[bit / 8] ^= 0x80 >> (bit % 8);
            if let Some(l) = stereo::layout(&m, true) {
                let _ = stereo::decode_pair(&m, &l, &DecodeOptions::default());
            }
        }
    }
}

#[test]
fn unsupported_jps_scaling_is_not_silently_discarded() {
    let image = jpeg(&sbs(32, 16));
    for descriptor in [0x0001_0201, 0x0002_0301, 0x0001_0101, 0x0002_0101, 0x0000_0901] {
        let file = with_descriptor(&image, descriptor);
        let layout = stereo::layout(&file, true).unwrap();
        assert_eq!(layout, StereoLayout::UnsupportedJps { descriptor });
        assert!(matches!(stereo::decode_pair(&file, &layout, &DecodeOptions::default()), Err(CodecError::Unsupported { .. })));
    }
}

#[test]
fn packed_geometry_and_expanded_limits_are_checked_before_splitting() {
    let ordinary = png(&filled(9, 7, RED));
    for layout in [
        StereoLayout::SideBySide { first: FirstEye::Left, gap: 0, squeezed: false },
        StereoLayout::OverUnder { first: FirstEye::Left, gap: 0, squeezed: false },
        StereoLayout::Rows { first: FirstEye::Left },
    ] {
        assert!(stereo::decode_pair(&ordinary, &layout, &DecodeOptions::default()).is_err());
    }
    let packed = jpeg(&sbs(32, 16));
    let layout = StereoLayout::SideBySide { first: FirstEye::Left, gap: 0, squeezed: true };
    let opts = DecodeOptions { limits: Limits { max_width: 100, ..Limits::default() }, ..DecodeOptions::default() };
    assert!(matches!(stereo::decode_pair(&packed, &layout, &opts), Err(CodecError::LimitExceeded(_))));
}

/// Insert EXIF after encoding: the ordinary writers deliberately reset orientation to 1.
fn with_packed_exif(bytes: &[u8], metadata: &[u8], png: bool) -> Vec<u8> {
    if png {
        let at = bytes.windows(4).position(|w| w == b"IDAT").unwrap() - 4;
        let mut chunk = (metadata.len() as u32).to_be_bytes().to_vec();
        let mut body = b"eXIf".to_vec();
        body.extend_from_slice(metadata);
        let mut crc = flate2::Crc::new();
        crc.update(&body);
        chunk.extend_from_slice(&body);
        chunk.extend_from_slice(&crc.sum().to_be_bytes());
        [&bytes[..at], &chunk, &bytes[at..]].concat()
    } else {
        let mut segment = vec![0xff, 0xe1];
        segment.extend_from_slice(&((metadata.len() + 8) as u16).to_be_bytes());
        segment.extend_from_slice(b"Exif\0\0");
        segment.extend_from_slice(metadata);
        after_soi(bytes, &segment)
    }
}

#[test]
fn packed_exif_orientation_applies_to_each_eye_and_is_cleared() {
    for orientation in 2u16..=8 {
        let pair = picture(16, 4, |x, y| if x < 8 { [150 + (x * 3) as u8, (y * 30) as u8, 0] } else { [0, (y * 30) as u8, 150 + (x * 3) as u8] });
        let mut metadata = exif();
        metadata[18..20].copy_from_slice(&orientation.to_le_bytes());
        for (kind, encoded) in
            [("PNG", stereo::encode_png(&pair, &EncodeOptions::default()).unwrap()), ("JPS", stereo::encode_jps(&pair, &EncodeOptions::default()).unwrap())]
        {
            let bytes = with_packed_exif(&encoded, &metadata, kind == "PNG");
            let stored = decode_with(&bytes, &DecodeOptions { keep_orientation: true, ..DecodeOptions::default() }).unwrap();
            assert_eq!(exif_orientation(stored.meta.exif.as_deref().unwrap()), orientation, "fixture must carry the requested orientation");
            let unrotated = read(&encoded, false).convert(ChannelLayout::Rgb, SampleType::U8);
            let got = read(&bytes, false);
            let (eye_w, eye_h) = if orientation >= 5 { (4, 8) } else { (8, 4) };
            assert_eq!(got.dimensions(), (eye_w * 2, eye_h), "{kind} orientation {orientation}");
            assert!(is_red(mean(&got, 0, eye_w)), "{kind} keeps left eye");
            assert!(is_blue(mean(&got, eye_w, eye_w * 2)), "{kind} keeps right eye");
            let rgb = got.convert(ChannelLayout::Rgb, SampleType::U8);
            for eye in 0..2usize {
                for y in 0..4usize {
                    for x in 0..8usize {
                        let (dx, dy) = match orientation {
                            2 => (7 - x, y),
                            3 => (7 - x, 3 - y),
                            4 => (x, 3 - y),
                            5 => (y, x),
                            6 => (3 - y, x),
                            7 => (3 - y, 7 - x),
                            8 => (y, 7 - x),
                            _ => unreachable!(),
                        };
                        let src = (y * 16 + eye * 8 + x) * 3;
                        let dst = (dy * eye_w as usize * 2 + eye * eye_w as usize + dx) * 3;
                        assert_eq!(&rgb.data()[dst..dst + 3], &unrotated.data()[src..src + 3], "{kind} orientation {orientation}, eye {eye}, ({x},{y})");
                    }
                }
            }
            assert_eq!(exif_orientation(got.meta.exif.as_deref().unwrap()), 1);
            let saved = stereo::encode_png(&got, &EncodeOptions::default()).unwrap();
            let reopened = read(&saved, false);
            assert_eq!(reopened.dimensions(), got.dimensions());
            assert_eq!(reopened.data(), got.data(), "{kind} does not rotate twice");
        }
    }
}

#[test]
fn mpo_views_with_different_icc_profiles_are_not_assigned_one_profile() {
    let mut left = filled(64, 48, RED);
    left.icc = Some(b"left profile".to_vec());
    let mut right = filled(64, 48, BLUE);
    right.icc = Some(b"different profile".to_vec());
    let bytes = mpo(&[(&jpeg(&left), REPRESENTATIVE | DISPARITY, Some(1)), (&jpeg(&right), DISPARITY, Some(2))]);
    let layout = stereo::layout(&bytes, false).unwrap();
    assert!(matches!(stereo::decode_pair(&bytes, &layout, &DecodeOptions::default()), Err(CodecError::Unsupported { .. })));
}

#[test]
fn damage_to_either_mpo_eye_is_reported() {
    let left = jpeg(&filled(64, 48, RED));
    let right = jpeg(&filled(64, 48, BLUE));
    for damage_left in [false, true] {
        let l = if damage_left { &left[..left.len() - 1] } else { &left };
        let r = if damage_left { &right } else { &right[..right.len() - 1] };
        let file = mpo(&[(l, REPRESENTATIVE | DISPARITY, Some(1)), (r, DISPARITY, Some(2))]);
        assert!(!read(&file, false).warnings.is_empty(), "damage to either view must reach the importer");
    }
}

#[test]
fn multi_view_mpo_is_not_silently_reduced_to_two_views() {
    let left = jpeg(&filled(64, 48, RED));
    let middle = jpeg(&filled(64, 48, GREEN));
    let right = jpeg(&filled(64, 48, BLUE));
    let file = mpo(&[(&left, REPRESENTATIVE | DISPARITY, Some(1)), (&middle, DISPARITY, Some(2)), (&right, DISPARITY, Some(3))]);
    let layout = stereo::layout(&file, false).unwrap();
    assert!(matches!(layout, StereoLayout::UnsupportedMpo { .. }));
    assert!(matches!(stereo::decode_pair(&file, &layout, &DecodeOptions::default()), Err(CodecError::Unsupported { .. })));
}
