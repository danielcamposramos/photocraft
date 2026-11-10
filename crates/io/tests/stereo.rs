//! Stereo pairs (MPO, JPS, PNG with `sTER`) through the document: a pair opens as one stereo
//! document holding both eyes side by side (left eye on the left), saves back as a pair with the
//! mark, and the document keeps the flag through a PhotoCraft file. The left eye is red and the
//! right eye blue, so a dropped or swapped eye shows.

use photocraft_codecs::{ChannelLayout, EncodeOptions, Image, stereo};
use photocraft_doc::Document;
use photocraft_io::*;

const EYE_W: u32 = 32;
const H: u32 = 16;

fn pair() -> Image {
    let data = (0..H).flat_map(|_| (0..2 * EYE_W).flat_map(|x| if x < EYE_W { [200, 30, 30] } else { [30, 30, 200] })).collect::<Vec<u8>>();
    Image::from_u8(2 * EYE_W, H, ChannelLayout::Rgb, data).unwrap()
}

/// Mean (red, blue) of the document's columns `x0..x1`, flattened.
fn red_blue(doc: &Document, x0: i32, x1: i32) -> (f32, f32) {
    let buf = photocraft_compose::flatten(doc);
    let (mut r, mut b, mut n) = (0.0, 0.0, 0.0);
    for y in 0..buf.rect.height() as usize {
        for x in x0 as usize..x1 as usize {
            let p = buf.px[y * buf.rect.width() as usize + x];
            r += p[0];
            b += p[2];
            n += 1.0;
        }
    }
    (r / n, b / n)
}

fn assert_pair(doc: &Document, what: &str) {
    assert!(doc.stereo, "{what}: a stereo document");
    assert_eq!((doc.size.width, doc.size.height), (2 * EYE_W, H), "{what}: both eyes side by side");
    let (lr, lb) = red_blue(doc, 0, EYE_W as i32);
    let (rr, rb) = red_blue(doc, EYE_W as i32, 2 * EYE_W as i32);
    assert!(lr > 0.6 && lb < 0.3, "{what}: the left half is the left eye (red): {lr} {lb}");
    assert!(rb > 0.6 && rr < 0.3, "{what}: the right half is the right eye (blue): {rr} {rb}");
}

fn files() -> Vec<(&'static str, Vec<u8>)> {
    let o = EncodeOptions::default();
    vec![
        ("pair.mpo", stereo::encode_mpo(&pair(), &o).unwrap()),
        ("pair.jps", stereo::encode_jps(&pair(), &o).unwrap()),
        ("pair.png", stereo::encode_png(&pair(), &o).unwrap()),
    ]
}

#[test]
fn a_stereo_file_opens_with_both_eyes() {
    for (name, bytes) in files() {
        let doc = import(name, &bytes).expect(name).document;
        assert_pair(&doc, name);
    }
}

#[test]
fn a_stereo_document_saves_as_each_pair_format() {
    let (name, bytes) = &files()[0];
    let doc = import(name, bytes).unwrap().document;
    for ext in ["mpo", "jps", "png"] {
        let out = export(&doc, &format!("out.{ext}"), &ExportOptions::default()).expect(ext);
        assert!(stereo::layout(&out.bytes, ext == "jps").is_some(), "{ext}: the file carries its stereo mark");
        assert_pair(&import(&format!("out.{ext}"), &out.bytes).unwrap().document, ext);
    }
}

#[test]
fn other_formats_get_the_pair_side_by_side() {
    let (name, bytes) = &files()[1];
    let doc = import(name, bytes).unwrap().document;
    let out = export(&doc, "out.jpg", &ExportOptions::default()).unwrap();
    assert!(stereo::layout(&out.bytes, false).is_none());
    assert!(out.warnings.iter().any(|w| w.contains("stereo metadata")));
    let back = import("out.jpg", &out.bytes).unwrap().document;
    assert!(!back.stereo);
    assert_eq!((back.size.width, back.size.height), (2 * EYE_W, H));
}

#[test]
fn a_flat_document_cannot_be_saved_as_a_pair() {
    let png = photocraft_codecs::encode(&pair(), photocraft_codecs::Format::Png, &EncodeOptions::default()).unwrap();
    let doc = import("flat.png", &png).unwrap().document;
    assert!(!doc.stereo, "a PNG without sTER is a flat picture");
    for ext in ["mpo", "jps"] {
        assert!(export(&doc, &format!("x.{ext}"), &ExportOptions::default()).is_err(), "{ext}");
    }
}

#[test]
fn saving_a_cross_fuse_png_keeps_the_normalized_eye_order() {
    use photocraft_codecs::{Format, PngStereo};
    let data = (0..H).flat_map(|_| (0..2 * EYE_W).flat_map(|x| if x < EYE_W { [30, 30, 200] } else { [200, 30, 30] })).collect();
    let cross = Image::from_u8(2 * EYE_W, H, ChannelLayout::Rgb, data).unwrap();
    let bytes = photocraft_codecs::encode(&cross, Format::Png, &EncodeOptions { png_stereo: Some(PngStereo::CrossFuse), ..Default::default() }).unwrap();
    let doc = import("cross.png", &bytes).unwrap().document;
    assert_pair(&doc, "cross-fuse import");
    let mut options = ExportOptions::default();
    apply_file_encoding(&doc, &mut options.encode);
    let saved = export(&doc, "saved.png", &options).unwrap();
    assert_eq!(photocraft_codecs::png_stereo(&saved.bytes), Some(PngStereo::DivergingFuse));
    assert_pair(&import("saved.png", &saved.bytes).unwrap().document, "File Save after normalization");
}

#[test]
fn stereo_export_preserves_cancellation() {
    let doc = import("pair.mpo", &files()[0].1).unwrap().document;
    let cancelled = || true;
    let progress = |_| {};
    let ctl = photocraft_raster::Interrupt::new(&cancelled, &progress);
    for name in ["pair.mpo", "pair.jps", "pair.png"] {
        assert!(matches!(export_with(&doc, name, &ExportOptions::default(), &ctl), Err(IoError::Cancelled)));
    }
}

#[test]
fn layered_exports_report_lost_stereo_identity() {
    let doc = import("pair.mpo", &files()[0].1).unwrap().document;
    for ext in ["psd", "psb", "ora", "tiff"] {
        let out = export(&doc, &format!("out.{ext}"), &ExportOptions::default()).unwrap();
        assert!(out.warnings.iter().any(|w| w.contains("stereo metadata")), "{ext}");
        assert!(!import(&format!("out.{ext}"), &out.bytes).unwrap().document.stereo);
    }
    let out = export(&doc, "out.pcraft", &ExportOptions::default()).unwrap();
    assert!(out.warnings.is_empty());
    assert_pair(&import("out.pcraft", &out.bytes).unwrap().document, "native stereo save");
}

#[test]
fn native_stereo_grids_and_pixel_shape_survive_every_pair_format() {
    for par in [(2, 1), (1, 2), (4, 3)] {
        let mut source = pair();
        source.meta.pixel_aspect = Some(par);
        let bytes = stereo::encode_png(&source, &EncodeOptions::default()).unwrap();
        let doc = import("native.png", &bytes).unwrap().document;
        assert_pair(&doc, "native grid");
        assert_eq!(doc.stereo_pixel_aspect, Some(par));
        for ext in ["pcraft", "png", "mpo", "jps"] {
            let out = export(&doc, &format!("out.{ext}"), &ExportOptions::default()).unwrap();
            let back = import(&format!("out.{ext}"), &out.bytes).unwrap().document;
            assert_pair(&back, ext);
            assert_eq!(back.stereo_pixel_aspect, Some(par), "{ext} pixel shape {par:?}");
        }
    }
}

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

fn ordinary_pixels(bytes: &[u8]) -> Document {
    let image = photocraft_codecs::decode(bytes).unwrap();
    let png = photocraft_codecs::encode(&image, photocraft_codecs::Format::Png, &EncodeOptions::default()).unwrap();
    import("ordinary.png", &png).unwrap().document
}

#[test]
fn readable_unsupported_pairs_open_as_ordinary_images_with_a_warning() {
    let jpeg = photocraft_codecs::encode(&pair(), photocraft_codecs::Format::Jpeg, &EncodeOptions::default()).unwrap();
    let odd = Image::from_u8(63, H, ChannelLayout::Rgb, vec![100; (63 * H * 3) as usize]).unwrap();
    let odd = photocraft_codecs::encode(&odd, photocraft_codecs::Format::Jpeg, &EncodeOptions::default()).unwrap();
    let mut descriptor = vec![0xFF, 0xE3, 0, 18];
    descriptor.extend_from_slice(b"_JPSJPS_");
    descriptor.extend_from_slice(&4u16.to_be_bytes());
    descriptor.extend_from_slice(&0x0004_0401u32.to_be_bytes());
    descriptor.extend_from_slice(&0u16.to_be_bytes());
    let anaglyph = after_soi(&jpeg, &descriptor);
    let multi = mpo(&[(&jpeg, 0x2002_0002, Some(1)), (&jpeg, 0x02_0002, Some(2)), (&jpeg, 0x02_0002, Some(3))]);
    for (name, bytes) in [("odd.jps", odd), ("anaglyph.jps", anaglyph), ("three-views.mpo", multi)] {
        let result = import(name, &bytes).expect(name);
        assert!(!result.document.stereo, "{name}: fallback must not claim a decoded pair");
        assert!(result.warnings.iter().any(|w| w.contains("stereo")), "{name}: explain fallback: {:?}", result.warnings);
        let plain = ordinary_pixels(&bytes);
        assert_eq!(result.document.size, plain.size, "{name}: keep the complete ordinary image");
        assert_eq!(photocraft_compose::flatten(&result.document).px, photocraft_compose::flatten(&plain).px, "{name}: ordinary import pixels");
    }
}

#[test]
fn stereo_exports_render_duotone_inks_like_ordinary_flat_export() {
    use photocraft_color::{Color, ColorMode, SampleType};
    use photocraft_doc::{Duotone, DuotoneInk, Size};
    let mut doc = Document::with_background("inks", Size::new(64, 16), ColorMode::Grayscale, SampleType::U8, Color::rgba(0.5, 0.5, 0.5, 1.0));
    doc.mode = ColorMode::Duotone;
    doc.duotone = Some(Duotone { inks: vec![DuotoneInk::new("Black", [0.0; 3]), DuotoneInk::new("Orange", [1.0, 0.5, 0.0])], psd_raw: None });
    let plain = export(&doc, "plain.png", &ExportOptions::default()).unwrap();
    let reference = photocraft_codecs::decode(&plain.bytes).unwrap().convert(ChannelLayout::Rgb, photocraft_codecs::SampleType::U8);
    doc.stereo = true;
    for ext in ["mpo", "jps", "png"] {
        let out = export(&doc, &format!("inks.{ext}"), &ExportOptions::default()).unwrap();
        let layout = stereo::layout(&out.bytes, ext == "jps").unwrap();
        let image = stereo::decode_pair(&out.bytes, &layout, &Default::default()).unwrap().convert(ChannelLayout::Rgb, photocraft_codecs::SampleType::U8);
        assert_eq!(image.dimensions(), reference.dimensions());
        let max = image.data().iter().zip(reference.data()).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
        assert!(max <= 5, "{ext}: rendered ink colors differ from flat export by {max}");
    }
}

#[test]
fn stereo_import_respects_the_configured_flat_decode_limit() {
    let options = ImportOptions { flat_max_pixel_bytes: 1, ..Default::default() };
    for (name, bytes) in files() {
        assert!(import_with_options(name, &bytes, &options).is_err(), "{name}: stereo must not bypass the configured allocation limit");
    }
}
