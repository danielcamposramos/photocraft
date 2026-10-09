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
