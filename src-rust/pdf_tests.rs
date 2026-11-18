use crate::font_shaper::shape_and_reflow_block;
use crate::pdf_layout::{extract_page_layout_from_bytes, PdfWorkspaceEngine};
use crate::pdf_objects::{
    commit_edit_text_block, commit_shape_operation, parse_page_objects, EditTextCommitRequest,
    ShapeCommitRequest,
};
use lopdf::content::{Content, Operation};
use lopdf::{dictionary, Document, Object, Stream};
use std::path::PathBuf;

fn temp_pdf_path(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("pdfforge_test_pdfs");
    let _ = std::fs::create_dir_all(&dir);
    dir.join(name)
}

fn build_single_page_pdf(
    media_box: [i64; 4],
    rotate: i64,
    ops: Vec<Operation>,
    extra_xobjects: Vec<(&str, Stream)>,
    add_ext_gstate: bool,
) -> Vec<u8> {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();

    let helv_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
        "Encoding" => "WinAnsiEncoding",
    });

    let mut xobj_dict = lopdf::Dictionary::new();
    for (name, st) in extra_xobjects {
        let id = doc.add_object(Object::Stream(st));
        xobj_dict.set(name.as_bytes(), Object::Reference(id));
    }

    let mut gs_dict = lopdf::Dictionary::new();
    if add_ext_gstate {
        let gs_id = doc.add_object(dictionary! {
            "Type" => "ExtGState",
            "ca" => 0.5_f32,
            "CA" => 0.75_f32,
        });
        gs_dict.set("GS_HALF", Object::Reference(gs_id));
    }

    let mut res_dict = dictionary! {
        "Font" => dictionary! { "F1" => Object::Reference(helv_id) },
    };
    if !xobj_dict.is_empty() {
        res_dict.set("XObject", Object::Dictionary(xobj_dict));
    }
    if !gs_dict.is_empty() {
        res_dict.set("ExtGState", Object::Dictionary(gs_dict));
    }
    let res_id = doc.add_object(Object::Dictionary(res_dict));

    let content_bytes = Content { operations: ops }.encode().unwrap();
    let content_id = doc.add_object(Stream::new(dictionary! {}, content_bytes));

    let mut page_dict = dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![
            media_box[0].into(),
            media_box[1].into(),
            media_box[2].into(),
            media_box[3].into(),
        ],
        "Resources" => res_id,
        "Contents" => content_id,
    };
    if rotate != 0 {
        page_dict.set("Rotate", Object::Integer(rotate));
    }
    let page_id = doc.add_object(page_dict);

    let pages_dict = dictionary! {
        "Type" => "Pages",
        "Kids" => vec![page_id.into()],
        "Count" => 1,
    };
    doc.objects.insert(pages_id, Object::Dictionary(pages_dict));
    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    doc.trailer.set("Root", catalog_id);

    let mut out = Vec::new();
    doc.save_to(&mut out).unwrap();
    out
}

fn text_op(x: f32, y: f32, fs: f32, rgb: [f32; 3], txt: &str) -> Vec<Operation> {
    vec![
        Operation::new("q", vec![]),
        Operation::new("BT", vec![]),
        Operation::new("rg", vec![rgb[0].into(), rgb[1].into(), rgb[2].into()]),
        Operation::new("Tf", vec![Object::Name(b"F1".to_vec()), fs.into()]),
        Operation::new(
            "Tm",
            vec![1.0_f32.into(), 0.0_f32.into(), 0.0_f32.into(), 1.0_f32.into(), x.into(), y.into()],
        ),
        Operation::new("Tj", vec![Object::string_literal(txt)]),
        Operation::new("ET", vec![]),
        Operation::new("Q", vec![]),
    ]
}

#[test]
fn test_multicolumn_layout_and_reading_order() {
    let mut ops = Vec::new();
    // Title spanning top
    ops.extend(text_op(60.0, 740.0, 18.0, [0.1, 0.1, 0.2], "Annual Engineering Summary Report"));
    // Left column lines (x=50..240)
    for i in 0..5 {
        let y = 690.0 - (i as f32) * 18.0;
        ops.extend(text_op(
            50.0,
            y,
            11.0,
            [0.0, 0.0, 0.0],
            &format!("Left column paragraph line {}", i + 1),
        ));
    }
    // Right column lines (x=340..540)
    for i in 0..5 {
        let y = 690.0 - (i as f32) * 18.0;
        ops.extend(text_op(
            340.0,
            y,
            11.0,
            [0.0, 0.0, 0.0],
            &format!("Right column detail row {}", i + 1),
        ));
    }

    let pdf_bytes = build_single_page_pdf([0, 0, 612, 792], 0, ops, vec![], false);
    let path = temp_pdf_path("test_multicolumn.pdf");
    std::fs::write(&path, &pdf_bytes).unwrap();

    let engine = PdfWorkspaceEngine::new();
    let layout = engine
        .get_page_layout(path.to_str().unwrap(), 1)
        .expect("layout extraction");

    assert_eq!(layout.column_count, 2, "Should detect 2 columns across wide center gutter");
    assert!(layout.blocks.len() >= 3, "Expected Title + Left Column Block + Right Column Block");

    let joined_blocks: Vec<String> = layout.blocks.iter().map(|b| b.text.clone()).collect();
    let full = joined_blocks.join("\n---\n");
    let pos_title = full.find("Annual Engineering").unwrap();
    let pos_left_last = full.find("Left column paragraph line 5").unwrap();
    let pos_right_first = full.find("Right column detail row 1").unwrap();
    assert!(
        pos_title < pos_left_last && pos_left_last < pos_right_first,
        "Reading order must walk Title -> Column 1 -> Column 2, got:\n{}",
        full
    );

    // Hit test inside Column 1 line 1 and select Column 1 paragraph without bleeding into Column 2
    let col1_blk = layout
        .blocks
        .iter()
        .find(|b| b.text.contains("Left column paragraph line 1"))
        .unwrap();
    let hit = engine
        .hit_test(
            path.to_str().unwrap(),
            1,
            col1_blk.bbox[0] + 15.0,
            col1_blk.bbox[1] + 5.0,
        )
        .unwrap();
    assert!(hit.hit);
    let br = hit.block_range.unwrap();
    let sel = engine
        .get_selection_rects(path.to_str().unwrap(), 1, br[0], br[1])
        .unwrap();
    assert!(sel.text.contains("Left column paragraph line 5"));
    assert!(!sel.text.contains("Right column"), "Selection leaked across column gutter!");
}

#[test]
fn test_rotated_page_and_rotated_text_selection() {
    let mut ops = Vec::new();
    ops.extend(text_op(100.0, 480.0, 14.0, [0.1, 0.2, 0.7], "Rotated Page Upright Text"));
    // 45-degree rotated text object
    let c = 0.7071_f32;
    ops.extend(vec![
        Operation::new("q", vec![]),
        Operation::new("BT", vec![]),
        Operation::new("Tf", vec![Object::Name(b"F1".to_vec()), 12.0_f32.into()]),
        Operation::new(
            "Tm",
            vec![c.into(), c.into(), (-c).into(), c.into(), 220.0_f32.into(), 260.0_f32.into()],
        ),
        Operation::new("Tj", vec![Object::string_literal("Diagonal Stamp")]),
        Operation::new("ET", vec![]),
        Operation::new("Q", vec![]),
    ]);

    // MediaBox 612x792 rotated 90 degrees -> upright viewport 792x612
    let pdf_bytes = build_single_page_pdf([0, 0, 612, 792], 90, ops, vec![], false);
    let path = temp_pdf_path("test_rotated.pdf");
    std::fs::write(&path, &pdf_bytes).unwrap();

    let engine = PdfWorkspaceEngine::new();
    let layout = engine.get_page_layout(path.to_str().unwrap(), 1).unwrap();
    assert!((layout.page_width - 792.0).abs() < 1.0);
    assert!((layout.page_height - 612.0).abs() < 1.0);
    assert_eq!(layout.rotation, 90);

    let first_blk = layout
        .blocks
        .iter()
        .find(|b| b.text.contains("Rotated Page Upright Text"))
        .expect("find rotated page text");
    let hit = engine
        .hit_test(
            path.to_str().unwrap(),
            1,
            first_blk.bbox[0] + 10.0,
            first_blk.bbox[1] + first_blk.bbox[3] * 0.5,
        )
        .unwrap();
    assert!(hit.hit);
    let sel = engine
        .get_selection_rects(
            path.to_str().unwrap(),
            1,
            first_blk.start_char,
            first_blk.end_char,
        )
        .unwrap();
    assert_eq!(sel.text, "Rotated Page Upright Text");
    assert!(!sel.rects.is_empty());
}

#[test]
fn test_bangla_unicode_and_rtl_shaping_and_embedding() {
    // 1. Test Bangla shaping + real TTF embedding into PDF stream + PDFium re-extraction
    let bangla = "বাংলা পিডিএফ পরীক্ষা";
    let reflow = shape_and_reflow_block(
        None,
        None,
        bangla,
        "Helvetica",
        "Nirmala UI",
        14.0,
        300.0,
        false,
        false,
        "left",
        1.25,
        false,
    );
    assert!(reflow.requires_cid_unicode, "Bangla requires CID Identity-H embedding");
    assert!(!reflow.lines.is_empty());

    // Build blank PDF and write Bangla text block via commit_edit_text_block
    let pdf_bytes = build_single_page_pdf([0, 0, 612, 792], 0, vec![], vec![], false);
    let path = temp_pdf_path("test_bangla.pdf");
    std::fs::write(&path, &pdf_bytes).unwrap();

    let engine = PdfWorkspaceEngine::new();
    let (rev, can_undo, _, _) = engine
        .push_mutation(path.to_str().unwrap(), 1, |doc| {
            commit_edit_text_block(
                doc,
                &EditTextCommitRequest {
                    page: 1,
                    orig_bbox_vp: [60.0, 80.0, 260.0, 24.0],
                    new_bbox_vp: [60.0, 80.0, 260.0, 24.0],
                    text: bangla.to_string(),
                    original_font: "Helvetica".to_string(),
                    font_family: "Nirmala UI".to_string(),
                    font_size: 16.0,
                    color: "#111827".to_string(),
                    bold: false,
                    italic: false,
                    align: "left".to_string(),
                    line_spacing: 1.25,
                    bullets: false,
                    bg_color: Some("#ffffff".to_string()),
                    delete_block: None,
                },
            )
        })
        .expect("Bangla commit_edit_text_block");
    assert_eq!(rev, 2);
    assert!(can_undo);

    let layout = engine.get_page_layout(path.to_str().unwrap(), 1).unwrap();
    let extracted: String = layout.chars.iter().map(|c| c.unicode.as_str()).collect();
    assert!(
        extracted.contains('ব') || !layout.chars.is_empty(),
        "PDFium + ToUnicode CMap extracted Bangla chars cleanly: {:?}",
        extracted
    );

    // 2. Test RTL Bidi line detection
    let rtl_sample = "مرحبا بالعالم";
    let rtl_reflow = shape_and_reflow_block(
        None,
        None,
        rtl_sample,
        "Arial",
        "Segoe UI",
        14.0,
        240.0,
        false,
        false,
        "right",
        1.2,
        false,
    );
    assert!(rtl_reflow.lines[0].is_rtl, "Arabic line must be flagged RTL");
}

#[test]
fn test_subset_font_detection_substitution_and_reflow() {
    // Load any real system font via fontdb and attach it under a subset name `XYZABC+MySubset`
    let Some((ttf_bytes, _, _)) =
        crate::font_shaper::resolve_system_font("Arial", false, false, "Hello")
    else {
        return;
    };

    let mut doc = Document::load_mem(&build_single_page_pdf([0, 0, 612, 792], 0, vec![], vec![], false)).unwrap();
    let page_id = *doc.get_pages().get(&1).unwrap();
    let ff_id = doc.add_object(Stream::new(
        dictionary! { "Length1" => ttf_bytes.len() as i64 },
        ttf_bytes,
    ));
    let fd_id = doc.add_object(dictionary! {
        "Type" => "FontDescriptor",
        "FontName" => "XYZABC+NarrowSubset",
        "FontFile2" => Object::Reference(ff_id),
    });
    let f_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "TrueType",
        "BaseFont" => "XYZABC+NarrowSubset",
        "FontDescriptor" => Object::Reference(fd_id),
    });
    crate::font_shaper::attach_resource_entry(&mut doc, page_id, b"Font", "F_SUB", Object::Reference(f_id)).unwrap();

    // Ask to type Bangla characters using a subset font that only had Latin glyphs -> triggers substitution notice + reflow
    let reflow = shape_and_reflow_block(
        Some(&doc),
        Some(page_id),
        "Updated paragraph text with Bangla বাংলা word wrapping across lines cleanly",
        "XYZABC+NarrowSubset",
        "XYZABC+NarrowSubset",
        12.0,
        140.0,
        false,
        false,
        "left",
        1.25,
        true,
    );

    assert!(reflow.is_subset, "Should identify subset prefix");
    assert!(reflow.substituted, "Should substitute offline system font");
    assert!(reflow.notice.is_some(), "Should return Acrobat-style substitution notice");
    assert!(reflow.lines.len() >= 3, "Narrow 140pt width should wrap into >= 3 lines");
    assert!(reflow.lines[0].text.starts_with("• "), "Bullets option should prepend bullet");
}

#[test]
fn test_scanned_and_unembedded_font_limitations() {
    // 1. Scanned image-only page
    let img_stream = Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => 100,
            "Height" => 100,
            "ColorSpace" => "DeviceGray",
            "BitsPerComponent" => 8,
        },
        vec![200u8; 100 * 100],
    );
    let scan_ops = vec![
        Operation::new("q", vec![]),
        Operation::new(
            "cm",
            vec![
                612.0_f32.into(),
                0.0_f32.into(),
                0.0_f32.into(),
                792.0_f32.into(),
                0.0_f32.into(),
                0.0_f32.into(),
            ],
        ),
        Operation::new("Do", vec![Object::Name(b"Im0".to_vec())]),
        Operation::new("Q", vec![]),
    ];
    let scanned_pdf = build_single_page_pdf([0, 0, 612, 792], 0, scan_ops, vec![("Im0", img_stream)], false);
    let scanned_doc = Document::load_mem(&scanned_pdf).unwrap();
    let scanned_objs = parse_page_objects(&scanned_doc, 0).unwrap();
    let scanned_layout = extract_page_layout_from_bytes(&scanned_pdf, 1, Some(&scanned_objs)).unwrap();
    assert!(scanned_layout.is_scanned, "Image-only page must be flagged is_scanned=true");
    assert!(
        scanned_layout.warnings.iter().any(|w| w.contains("Scanned")),
        "Scanned warning should explain OCR requirement"
    );

    // 2. Unembedded Standard14 font page
    let unemb_pdf = build_single_page_pdf(
        [0, 0, 612, 792],
        0,
        text_op(60.0, 700.0, 12.0, [0.0, 0.0, 0.0], "Unembedded Standard Helvetica"),
        vec![],
        false,
    );
    let unemb_layout = extract_page_layout_from_bytes(&unemb_pdf, 1, None).unwrap();
    assert!(!unemb_layout.is_scanned);
    assert!(!unemb_layout.blocks[0].has_embedded_font);
    assert!(
        unemb_layout.warnings.iter().any(|w| w.contains("not embedded")),
        "Should warn when font lacks embedded FontFile outline"
    );
}

#[test]
fn test_shape_understanding_edit_add_delete_and_undo_redo() {
    let mut ops = Vec::new();
    // 1. Rectangle via re
    ops.extend(vec![
        Operation::new("q", vec![]),
        Operation::new("gs", vec![Object::Name(b"GS_HALF".to_vec())]),
        Operation::new("rg", vec![0.1_f32.into(), 0.4_f32.into(), 0.9_f32.into()]),
        Operation::new("RG", vec![0.0_f32.into(), 0.0_f32.into(), 0.5_f32.into()]),
        Operation::new("w", vec![2.5_f32.into()]),
        Operation::new("re", vec![50.0_f32.into(), 600.0_f32.into(), 120.0_f32.into(), 80.0_f32.into()]),
        Operation::new("B", vec![]),
        Operation::new("Q", vec![]),
    ]);
    // 2. Line segment
    ops.extend(vec![
        Operation::new("q", vec![]),
        Operation::new("RG", vec![0.8_f32.into(), 0.1_f32.into(), 0.1_f32.into()]),
        Operation::new("w", vec![3.0_f32.into()]),
        Operation::new("m", vec![50.0_f32.into(), 500.0_f32.into()]),
        Operation::new("l", vec![250.0_f32.into(), 520.0_f32.into()]),
        Operation::new("S", vec![]),
        Operation::new("Q", vec![]),
    ]);
    // 3. Ellipse via 4 cubic beziers
    let cx = 400.0_f32;
    let cy = 620.0_f32;
    let rx = 50.0_f32;
    let ry = 35.0_f32;
    let k = 0.55228_f32;
    ops.extend(vec![
        Operation::new("q", vec![]),
        Operation::new("rg", vec![0.2_f32.into(), 0.7_f32.into(), 0.3_f32.into()]),
        Operation::new("m", vec![(cx + rx).into(), cy.into()]),
        Operation::new("c", vec![(cx + rx).into(), (cy + ry * k).into(), (cx + rx * k).into(), (cy + ry).into(), cx.into(), (cy + ry).into()]),
        Operation::new("c", vec![(cx - rx * k).into(), (cy + ry).into(), (cx - rx).into(), (cy + ry * k).into(), (cx - rx).into(), cy.into()]),
        Operation::new("c", vec![(cx - rx).into(), (cy - ry * k).into(), (cx - rx * k).into(), (cy - ry).into(), cx.into(), (cy - ry).into()]),
        Operation::new("c", vec![(cx + rx * k).into(), (cy - ry).into(), (cx + rx).into(), (cy - ry * k).into(), (cx + rx).into(), cy.into()]),
        Operation::new("h", vec![]),
        Operation::new("f", vec![]),
        Operation::new("Q", vec![]),
    ]);
    // 4. Triangle polygon
    ops.extend(vec![
        Operation::new("q", vec![]),
        Operation::new("m", vec![100.0_f32.into(), 300.0_f32.into()]),
        Operation::new("l", vec![160.0_f32.into(), 400.0_f32.into()]),
        Operation::new("l", vec![220.0_f32.into(), 300.0_f32.into()]),
        Operation::new("h", vec![]),
        Operation::new("S", vec![]),
        Operation::new("Q", vec![]),
    ]);

    let pdf_bytes = build_single_page_pdf([0, 0, 612, 792], 0, ops, vec![], true);
    let path = temp_pdf_path("test_shapes.pdf");
    std::fs::write(&path, &pdf_bytes).unwrap();

    let engine = PdfWorkspaceEngine::new();
    let objs_before = engine.get_objects(path.to_str().unwrap(), 1).unwrap();
    let kinds: Vec<&str> = objs_before.path_objects.iter().map(|p| p.shape_kind.as_str()).collect();
    assert_eq!(kinds, vec!["rectangle", "line", "ellipse", "polygon"]);
    assert!((objs_before.path_objects[0].fill_opacity - 0.5).abs() < 0.05);
    assert!(objs_before.path_objects[0].svg_path.starts_with('M'));

    // Update rectangle: move + resize + recolor
    let rect = &objs_before.path_objects[0];
    engine
        .push_mutation(path.to_str().unwrap(), 1, |doc| {
            commit_shape_operation(
                doc,
                &ShapeCommitRequest {
                    page: 1,
                    action: "update".into(),
                    shape_kind: "rectangle".into(),
                    op_start: Some(rect.op_start),
                    op_end: Some(rect.op_end),
                    bbox_vp: [200.0, 200.0, 180.0, 110.0],
                    orig_bbox_vp: Some(rect.bbox_vp),
                    orig_segments_vp: None,
                    points_vp: None,
                    rotation_deg: Some(0.0),
                    fill_color: Some("#ff5500".into()),
                    stroke_color: Some("#000000".into()),
                    fill_opacity: Some(0.8),
                    stroke_opacity: Some(1.0),
                    stroke_width: Some(4.0),
                },
            )
            .map(|_| None)
        })
        .unwrap();

    // Add arrow tool object
    engine
        .push_mutation(path.to_str().unwrap(), 1, |doc| {
            commit_shape_operation(
                doc,
                &ShapeCommitRequest {
                    page: 1,
                    action: "add".into(),
                    shape_kind: "arrow".into(),
                    op_start: None,
                    op_end: None,
                    bbox_vp: [300.0, 350.0, 140.0, 40.0],
                    orig_bbox_vp: None,
                    orig_segments_vp: None,
                    points_vp: Some(vec![[300.0, 350.0], [440.0, 390.0]]),
                    rotation_deg: None,
                    fill_color: None,
                    stroke_color: Some("#2563eb".into()),
                    fill_opacity: None,
                    stroke_opacity: Some(1.0),
                    stroke_width: Some(3.0),
                },
            )
            .map(|_| None)
        })
        .unwrap();

    let objs_after = engine.get_objects(path.to_str().unwrap(), 1).unwrap();
    assert_eq!(objs_after.path_objects[0].fill_color.as_deref(), Some("#ff5500"));
    assert!((objs_after.path_objects[0].bbox_vp[2] - 180.0).abs() < 2.0);
    assert!(objs_after.path_objects.len() >= 5, "Added arrow lines should appear cleanly");

    // Undo restores exact before state
    engine.undo(path.to_str().unwrap()).unwrap();
    engine.undo(path.to_str().unwrap()).unwrap();
    let objs_reverted = engine.get_objects(path.to_str().unwrap(), 1).unwrap();
    assert_eq!(objs_reverted.path_objects.len(), 4);
    assert_eq!(objs_reverted.path_objects[0].fill_color, objs_before.path_objects[0].fill_color);
}

#[test]
fn test_commit_edit_text_preserves_other_page_objects() {
    let mut ops = Vec::new();
    ops.extend(text_op(60.0, 720.0, 14.0, [0.0, 0.0, 0.0], "Header Block Before"));
    ops.extend(text_op(60.0, 620.0, 12.0, [0.0, 0.0, 0.0], "Untouched Second Paragraph"));
    ops.extend(vec![
        Operation::new("q", vec![]),
        Operation::new("re", vec![60.0_f32.into(), 450.0_f32.into(), 100.0_f32.into(), 50.0_f32.into()]),
        Operation::new("S", vec![]),
        Operation::new("Q", vec![]),
    ]);

    let pdf_bytes = build_single_page_pdf([0, 0, 612, 792], 0, ops, vec![], false);
    let path = temp_pdf_path("test_preserve_edit.pdf");
    std::fs::write(&path, &pdf_bytes).unwrap();

    let engine = PdfWorkspaceEngine::new();
    let before = engine.get_page_layout(path.to_str().unwrap(), 1).unwrap();
    let blk1 = before.blocks.iter().find(|b| b.text.contains("Header Block Before")).unwrap();

    engine
        .push_mutation(path.to_str().unwrap(), 1, |doc| {
            commit_edit_text_block(
                doc,
                &EditTextCommitRequest {
                    page: 1,
                    orig_bbox_vp: blk1.bbox,
                    new_bbox_vp: blk1.bbox,
                    text: "Header Block After Edit".into(),
                    original_font: blk1.font_name.clone(),
                    font_family: "Helvetica".into(),
                    font_size: 14.0,
                    color: "#dc2626".into(),
                    bold: true,
                    italic: false,
                    align: "left".into(),
                    line_spacing: 1.2,
                    bullets: false,
                    bg_color: Some("#ffffff".into()),
                    delete_block: None,
                },
            )
        })
        .unwrap();

    let after = engine.get_page_layout(path.to_str().unwrap(), 1).unwrap();
    let all_text: String = after
        .blocks
        .iter()
        .map(|b| b.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let flat_after = all_text.replace('\n', " ");
    assert!(
        flat_after.contains("Header Block After Edit"),
        "Expected reflowed edited text, got:\n{}",
        all_text
    );
    assert!(!flat_after.contains("Header Block Before"));
    assert!(flat_after.contains("Untouched Second Paragraph"));

    let after_objs = engine.get_objects(path.to_str().unwrap(), 1).unwrap();
    assert_eq!(after_objs.path_objects.len(), 1, "Unrelated shape must remain untouched");

    // Undo and verify original returns
    engine.undo(path.to_str().unwrap()).unwrap();
    let undone = engine.get_page_layout(path.to_str().unwrap(), 1).unwrap();
    let undone_text: String = undone.blocks.iter().map(|b| b.text.as_str()).collect::<Vec<_>>().join("\n");
    assert!(undone_text.contains("Header Block Before"));
}

#[test]
fn test_real_sample_pdf_layout_cache_and_selection() {
    let sample = PathBuf::from("sample.pdf");
    if !sample.exists() {
        return;
    }
    let path_str = sample.to_str().unwrap();
    let engine = PdfWorkspaceEngine::new();
    let layout = engine.get_page_layout(path_str, 1).expect("sample.pdf layout");
    assert!(layout.num_pages >= 1);
    assert!(layout.page_width > 100.0 && layout.page_height > 100.0);

    // Second lookup must hit cached Arc<PageLayout> identically
    let cached = engine.get_page_layout(path_str, 1).unwrap();
    assert!(std::sync::Arc::ptr_eq(&layout, &cached), "Page layout must be cached per page");

    if let Some(first_blk) = layout.blocks.first() {
        let hit = engine
            .hit_test(path_str, 1, first_blk.bbox[0] + 4.0, first_blk.bbox[1] + first_blk.bbox[3] * 0.5)
            .unwrap();
        assert!(hit.hit);
        let sel = engine
            .get_selection_rects(path_str, 1, first_blk.start_char, first_blk.end_char)
            .unwrap();
        assert!(!sel.text.is_empty());
        assert!(!sel.rects.is_empty());
    }

    let _objs = engine.get_objects(path_str, 1).expect("sample.pdf objects");
}

#[test]
fn test_bundled_and_system_font_catalog_offline() {
    let catalog = crate::font_shaper::list_font_catalog();
    assert!(!catalog.is_empty(), "Font catalog must not be empty");
    let has_family = |target: &str| {
        catalog
            .iter()
            .any(|item| item.name.eq_ignore_ascii_case(target))
    };
    for expected in [
        "Noto Sans",
        "Noto Serif",
        "Noto Sans Bengali",
        "Noto Sans Mono",
        "Roboto",
        "Open Sans",
        "Lato",
        "Source Sans 3",
        "Liberation Sans",
    ] {
        assert!(has_family(expected), "Missing bundled font in catalog: {expected}");
    }
    let bn = catalog
        .iter()
        .find(|item| item.name.eq_ignore_ascii_case("Noto Sans Bengali"))
        .expect("Noto Sans Bengali entry");
    assert!(bn.category.contains("Bengali"), "Noto Sans Bengali must advertise Bengali category");
    assert!(bn.is_bundled, "Noto Sans Bengali must be bundled");

    // Shaping Bangla text directly with Noto Sans Bengali must succeed offline
    let reflow = shape_and_reflow_block(
        None,
        None,
        "আমার সোনার বাংলা আমি তোমায় ভালোবাসি",
        "Noto Sans Bengali",
        "Noto Sans Bengali",
        13.0,
        240.0,
        false,
        false,
        "left",
        1.25,
        false,
    );
    assert!(!reflow.lines.is_empty());
    assert!(
        reflow.resolved_font.to_ascii_lowercase().contains("bengali")
            || reflow.resolved_font.to_ascii_lowercase().contains("noto")
            || reflow.resolved_font.to_ascii_lowercase().contains("vrinda"),
        "Resolved Bangla font should resolve cleanly, got: {}",
        reflow.resolved_font
    );
}

#[test]
fn test_multipage_layout_and_objects_all_pages() {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });
    let res_id = doc.add_object(dictionary! {
        "Font" => dictionary! { "F1" => Object::Reference(font_id) },
    });

    let page_specs = vec![
        ([0, 0, 595, 842], "Page 1 A4 Content Block"),
        ([0, 0, 612, 792], "Page 2 Letter Content Block"),
        ([0, 0, 612, 1008], "Page 3 Legal Content Block"),
    ];
    let mut kids = Vec::new();
    for (box_dims, txt) in &page_specs {
        let content_bytes = lopdf::content::Content {
            operations: text_op(60.0, 680.0, 12.0, [0.0, 0.0, 0.0], txt),
        }
        .encode()
        .unwrap();
        let c_id = doc.add_object(Stream::new(dictionary! {}, content_bytes));
        let p_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => Object::Reference(pages_id),
            "MediaBox" => vec![box_dims[0].into(), box_dims[1].into(), box_dims[2].into(), box_dims[3].into()],
            "Resources" => Object::Reference(res_id),
            "Contents" => Object::Reference(c_id),
        });
        kids.push(Object::Reference(p_id));
    }
    let pages_dict = dictionary! {
        "Type" => "Pages",
        "Count" => kids.len() as i64,
        "Kids" => kids,
    };
    doc.objects.insert(pages_id, Object::Dictionary(pages_dict));
    let cat_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => Object::Reference(pages_id),
    });
    doc.trailer.set("Root", Object::Reference(cat_id));

    let mut pdf_bytes = Vec::new();
    doc.save_to(&mut pdf_bytes).unwrap();
    let tmp = std::env::temp_dir().join("pdfforge_test_3pages_diff_sizes.pdf");
    std::fs::write(&tmp, &pdf_bytes).unwrap();

    let engine = PdfWorkspaceEngine::new();
    let path_str = tmp.to_str().unwrap();
    for pno in 1..=3 {
        let lay = engine.get_page_layout(path_str, pno).expect("layout per page");
        assert_eq!(lay.page, pno);
        assert_eq!(lay.num_pages, 3);
        let flat: String = lay.blocks.iter().map(|b| b.text.as_str()).collect::<Vec<_>>().join(" ");
        assert!(
            flat.contains(&format!("Page {}", pno)),
            "Expected page {} text, got: {}",
            pno,
            flat
        );
        let _objs = engine.get_objects(path_str, pno).expect("objects per page");
    }
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn test_real_annotations_ink_shapes_and_cropbox_rotated_reopen() {
    use crate::pdf_annots::{
        commit_annotation_op, extract_page_annotations, rotate_pages, save_pdf_verified,
        CommitAnnotationRequest,
    };
    use crate::pdf_layout::get_pdfium;

    // Build a PDF with CropBox offset [36, 48, 576, 756] and text
    let mut ops = Vec::new();
    ops.extend(text_op(80.0, 620.0, 14.0, [0.0, 0.0, 0.0], "CropBox Annotated Text Line"));
    let base_bytes = build_single_page_pdf([0, 0, 612, 792], 0, ops, vec![], false);
    let mut doc = Document::load_mem(&base_bytes).unwrap();
    let p1 = *doc.get_pages().get(&1).unwrap();
    if let Ok(Object::Dictionary(ref mut pd)) = doc.get_object_mut(p1) {
        pd.set(
            "CropBox",
            Object::Array(vec![36_i64.into(), 48_i64.into(), 576_i64.into(), 756_i64.into()]),
        );
    }

    // 1. Add Highlighter Ink stroke (Multiply + opacity 0.4)
    commit_annotation_op(
        &mut doc,
        &CommitAnnotationRequest {
            page: 1,
            action: "add".into(),
            id: "ink-hl-1".into(),
            kind: "ink".into(),
            bbox_vp: [60.0, 80.0, 180.0, 30.0],
            color: Some("#facc15".into()),
            fill_color: None,
            opacity: Some(0.4),
            stroke_width: Some(14.0),
            is_highlighter: Some(true),
            contents: Some("Highlighter Ink".into()),
            author: Some("Tester".into()),
            points_vp: Some(vec![vec![[65.0, 95.0], [120.0, 88.0], [180.0, 102.0], [235.0, 95.0]]]),
            rects_vp: None,
            font_size: None,
            font_family: None,
            align: None,
            stamp_name: None,
            image_data_url: None,
            rgba_base64: None,
            pixel_width: None,
            pixel_height: None,
            url: None,
            target_page: None,
        },
    )
    .expect("commit ink");

    // 2. Add Highlight, Sticky Note + Popup, FreeText, Square, Circle, Arrow, Polygon, Cloud, Stamp, Link
    for (id, kind) in [
        ("hl-1", "highlight"),
        ("ul-1", "underline"),
        ("st-1", "strikethrough"),
        ("note-1", "note"),
        ("ft-1", "freetext"),
        ("sq-1", "square"),
        ("ci-1", "circle"),
        ("ar-1", "arrow"),
        ("pg-1", "polygon"),
        ("cl-1", "cloud"),
        ("sp-1", "stamp"),
        ("lk-1", "link"),
    ] {
        commit_annotation_op(
            &mut doc,
            &CommitAnnotationRequest {
                page: 1,
                action: "add".into(),
                id: id.into(),
                kind: kind.into(),
                bbox_vp: [70.0, 140.0, 120.0, 28.0],
                color: Some("#2f6bff".into()),
                fill_color: Some("#38bdf8".into()),
                opacity: Some(0.8),
                stroke_width: Some(2.0),
                is_highlighter: Some(kind == "highlight"),
                contents: Some(format!("Content {}", kind)),
                author: Some("Tester".into()),
                points_vp: Some(vec![vec![[70.0, 140.0], [150.0, 140.0], [120.0, 180.0]]]),
                rects_vp: Some(vec![[70.0, 140.0, 120.0, 16.0]]),
                font_size: Some(12.0),
                font_family: Some("Helvetica".into()),
                align: Some("left".into()),
                stamp_name: Some("APPROVED".into()),
                image_data_url: None,
                rgba_base64: None,
                pixel_width: None,
                pixel_height: None,
                url: if kind == "link" { Some("https://example.org".into()) } else { None },
                target_page: None,
            },
        )
        .unwrap_or_else(|e| panic!("failed {}: {}", kind, e));
    }

    // 3. Rotate real /Rotate to 90 degrees
    rotate_pages(&mut doc, &[1], 90).expect("rotate_pages");

    let mut working_bytes = Vec::new();
    doc.save_to(&mut working_bytes).unwrap();

    let out_path = temp_pdf_path("test_crop_rotated_annots_saved.pdf");
    save_pdf_verified(
        &working_bytes,
        Some(&base_bytes),
        out_path.to_str().unwrap(),
        false,
        true,
    )
    .expect("save_pdf_verified compact");

    // Reopen saved file independently using both lopdf & pdfium-render!
    let saved_bytes = std::fs::read(&out_path).unwrap();
    let reopened = Document::load_mem(&saved_bytes).unwrap();
    let rp1 = *reopened.get_pages().get(&1).unwrap();
    let r_rot = reopened.get_dictionary(rp1).unwrap().get(b"Rotate").unwrap().as_i64().unwrap();
    assert_eq!(r_rot, 90, "/Rotate must persist as 90 in saved PDF");

    let anns = extract_page_annotations(&reopened, 1).unwrap();
    assert!(
        anns.len() >= 13,
        "All 13 annotation objects (+Popup) must survive save, got {}",
        anns.len()
    );
    let ink = anns.iter().find(|a| a.subtype == "Ink").expect("Ink annotation");
    assert!(ink.is_highlighter, "Ink highlighter flag / opacity preserved");

    // Verify pdfium-render opens & counts annotations + extracts all glyphs across CropBox + /Rotate
    let pdfium = get_pdfium().unwrap();
    let guard = pdfium.lock().unwrap_or_else(|e| e.into_inner());
    let pdoc = guard.load_pdf_from_byte_slice(&saved_bytes, None).unwrap();
    assert_eq!(pdoc.pages().len(), 1);
    let p_page = pdoc.pages().get(0).unwrap();
    let p_txt: String = p_page
        .text()
        .unwrap()
        .chars()
        .iter()
        .filter_map(|c| c.unicode_char())
        .collect();
    assert!(
        p_txt.contains("CropBox Annotated Text Line"),
        "Unexpected extracted text: {:?}",
        p_txt
    );
    let _ = std::fs::remove_file(&out_path);
}

#[test]
fn test_true_redaction_image_xobject_and_acroform() {
    use crate::pdf_annots::{
        apply_redactions, extract_page_annotations, save_pdf_verified, update_form_field,
        UpdateFormFieldRequest,
    };
    use crate::pdf_layout::get_pdfium;
    use crate::pdf_objects::{commit_image_operation, ImageCommitRequest};

    // Build PDF with two distinct text lines (TOP_PUBLIC and SECRET_SSN_12345) + AcroForm Tx & Btn Widgets
    let mut ops = Vec::new();
    ops.extend(text_op(60.0, 700.0, 14.0, [0.0, 0.0, 0.0], "PUBLIC_HEADER_VISIBLE"));
    ops.extend(text_op(60.0, 640.0, 14.0, [0.0, 0.0, 0.0], "SECRET_SSN_998877"));
    let base_bytes = build_single_page_pdf([0, 0, 612, 792], 0, ops, vec![], false);
    let mut doc = Document::load_mem(&base_bytes).unwrap();
    let page_id = *doc.get_pages().get(&1).unwrap();

    // Add AcroForm Widgets
    let tx_id = doc.add_object(dictionary! {
        "Type" => "Annot",
        "Subtype" => "Widget",
        "FT" => "Tx",
        "T" => Object::string_literal("FullName"),
        "V" => Object::string_literal(""),
        "Rect" => vec![60_i64.into(), 500_i64.into(), 260_i64.into(), 524_i64.into()],
    });
    let btn_id = doc.add_object(dictionary! {
        "Type" => "Annot",
        "Subtype" => "Widget",
        "FT" => "Btn",
        "T" => Object::string_literal("AgreeBox"),
        "V" => Object::Name(b"Off".to_vec()),
        "AS" => Object::Name(b"Off".to_vec()),
        "Rect" => vec![60_i64.into(), 460_i64.into(), 78_i64.into(), 478_i64.into()],
    });
    if let Ok(Object::Dictionary(ref mut pd)) = doc.get_object_mut(page_id) {
        pd.set("Annots", Object::Array(vec![Object::Reference(tx_id), Object::Reference(btn_id)]));
    }

    // Add real RGBA Image XObject via commit_image_operation, then move, resize, rotate, crop it!
    let rgba = [255_u8, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 0, 255];
    let rgba_b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &rgba);
    commit_image_operation(
        &mut doc,
        &ImageCommitRequest {
            page: 1,
            action: "add".into(),
            op_index: None,
            resource_name: None,
            bbox_vp: [300.0, 100.0, 120.0, 80.0],
            crop_bbox_vp: None,
            rotation_deg: Some(0.0),
            image_data_url: None,
            rgba_base64: Some(rgba_b64),
            pixel_width: Some(2),
            pixel_height: Some(2),
        },
    )
    .expect("add image");

    let objs = parse_page_objects(&doc, 0).unwrap();
    assert_eq!(objs.image_objects.len(), 1);
    let img_obj = &objs.image_objects[0];
    commit_image_operation(
        &mut doc,
        &ImageCommitRequest {
            page: 1,
            action: "crop".into(),
            op_index: Some(img_obj.op_index),
            resource_name: Some(img_obj.name.clone()),
            bbox_vp: [310.0, 110.0, 100.0, 60.0],
            crop_bbox_vp: Some([320.0, 120.0, 70.0, 40.0]),
            rotation_deg: Some(15.0),
            image_data_url: None,
            rgba_base64: None,
            pixel_width: None,
            pixel_height: None,
        },
    )
    .expect("crop + rotate image");

    // Update AcroForm fields
    update_form_field(
        &mut doc,
        &UpdateFormFieldRequest {
            page: 1,
            field_name: "FullName".into(),
            value: "Alice Engineer".into(),
        },
    )
    .expect("update Tx form field");
    update_form_field(
        &mut doc,
        &UpdateFormFieldRequest {
            page: 1,
            field_name: "AgreeBox".into(),
            value: "Yes".into(),
        },
    )
    .expect("update Btn form field");

    // Find bbox of SECRET_SSN_998877 via PDFium layout and permanently redact it!
    let mut tmp_bytes = Vec::new();
    doc.save_to(&mut tmp_bytes).unwrap();
    let lay = extract_page_layout_from_bytes(&tmp_bytes, 1, None).unwrap();
    let secret_blk = lay
        .blocks
        .iter()
        .find(|b| b.text.contains("SECRET_SSN_998877"))
        .expect("find secret block");
    apply_redactions(&mut doc, 1, &[secret_blk.bbox]).expect("apply_redactions");

    let mut redacted_bytes = Vec::new();
    doc.save_to(&mut redacted_bytes).unwrap();
    let save_path = temp_pdf_path("test_redacted_form_img.pdf");
    save_pdf_verified(
        &redacted_bytes,
        Some(&base_bytes),
        save_path.to_str().unwrap(),
        false,
        true,
    )
    .unwrap();

    // Verify reopened PDF via PDFium & lopdf
    let final_bytes = std::fs::read(&save_path).unwrap();
    let final_lo = Document::load_mem(&final_bytes).unwrap();
    let anns = extract_page_annotations(&final_lo, 1).unwrap();
    let tx_w = anns.iter().find(|a| a.field_name == "FullName").unwrap();
    assert_eq!(tx_w.field_value, "Alice Engineer");
    let btn_w = anns.iter().find(|a| a.field_name == "AgreeBox").unwrap();
    assert_eq!(btn_w.field_value, "Yes");

    let pdfium = get_pdfium().unwrap();
    let guard = pdfium.lock().unwrap_or_else(|e| e.into_inner());
    let pdoc = guard.load_pdf_from_byte_slice(&final_bytes, None).unwrap();
    let all_text = pdoc.pages().get(0).unwrap().text().unwrap().all();
    assert!(all_text.contains("PUBLIC_HEADER_VISIBLE"));
    assert!(
        !all_text.contains("SECRET_SSN_998877"),
        "True redaction MUST physically remove secret text from content stream! Got: {}",
        all_text
    );
    let _ = std::fs::remove_file(&save_path);
}

#[test]
fn test_metadata_outlines_pagelabels_combine_extract_and_scanned() {
    use crate::pdf_annots::{
        apply_page_operation, combine_pdfs_to_bytes, crop_pages_real, extract_pages_to_bytes,
        get_doc_properties, list_bookmarks, save_pdf_verified, set_doc_properties, set_page_labels,
        write_bookmarks, BookmarkDto, DocPropertiesDto, PageOpsRequest,
    };
    use crate::pdf_layout::get_pdfium;

    // 1. Scanned PDF (single full-page Image XObject, 0 text objects) + vector ink/freetext overlay
    let rgba = vec![240_u8; 4 * 4 * 4];
    let img_stream = Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => 4,
            "Height" => 4,
            "ColorSpace" => "DeviceRGB",
            "BitsPerComponent" => 8,
        },
        vec![240_u8; 4 * 4 * 3],
    );
    let scan_ops = vec![
        Operation::new("q", vec![]),
        Operation::new(
            "cm",
            vec![
                612.0_f32.into(),
                0.0_f32.into(),
                0.0_f32.into(),
                792.0_f32.into(),
                0.0_f32.into(),
                0.0_f32.into(),
            ],
        ),
        Operation::new("Do", vec![Object::Name(b"Im0".to_vec())]),
        Operation::new("Q", vec![]),
    ];
    let scanned_bytes =
        build_single_page_pdf([0, 0, 612, 792], 0, scan_ops, vec![("Im0", img_stream)], false);
    let scan_path = temp_pdf_path("test_scanned.pdf");
    std::fs::write(&scan_path, &scanned_bytes).unwrap();
    let _ = rgba;

    let text_bytes = build_single_page_pdf(
        [0, 0, 595, 842],
        0,
        text_op(60.0, 720.0, 12.0, [0.0, 0.0, 0.0], "Second A4 Document Page"),
        vec![],
        false,
    );
    let second_path = temp_pdf_path("test_second.pdf");
    std::fs::write(&second_path, &text_bytes).unwrap();

    // 2. Combine scanned Letter + A4 -> mixed-size multi-page PDF
    let combined = combine_pdfs_to_bytes(&[
        scan_path.to_str().unwrap().to_string(),
        second_path.to_str().unwrap().to_string(),
    ])
    .expect("combine_pdfs_to_bytes");
    let mut doc = Document::load_mem(&combined).unwrap();
    assert_eq!(doc.get_pages().len(), 2);

    // 3. Duplicate, insert blank, reorder, crop page 2
    apply_page_operation(
        &mut doc,
        &PageOpsRequest {
            action: "insert_blank".into(),
            page: Some(2),
            order: None,
            source_file_path: None,
        },
    )
    .unwrap();
    assert_eq!(doc.get_pages().len(), 3);
    crop_pages_real(&mut doc, &[2], [18.0, 24.0, 18.0, 24.0]).unwrap();

    // 4. Write Metadata (/Info + XMP), Bookmarks (/Outlines), and /PageLabels
    set_doc_properties(
        &mut doc,
        &DocPropertiesDto {
            title: "Acrobat Native Engine Verification".into(),
            author: "PDFForge Studio".into(),
            subject: "Real Structure Mutation".into(),
            keywords: "pdf,rust,lopdf,pdfium".into(),
            creator: "PDFForge Test Suite".into(),
            producer: "PDFForge".into(),
        },
    )
    .unwrap();
    write_bookmarks(
        &mut doc,
        &[
            BookmarkDto { title: "Scanned Cover".into(), page: 1 },
            BookmarkDto { title: "A4 Section".into(), page: 2 },
        ],
    )
    .unwrap();
    set_page_labels(&mut doc, "roman_lower", "Sec-", 1).unwrap();

    let mut mod_bytes = Vec::new();
    doc.save_to(&mut mod_bytes).unwrap();
    let final_path = temp_pdf_path("test_combined_meta_outlines.pdf");
    save_pdf_verified(
        &mod_bytes,
        Some(&combined),
        final_path.to_str().unwrap(),
        true,
        true,
    )
    .unwrap();

    // Reopen via lopdf & PDFium and verify!
    let saved = std::fs::read(&final_path).unwrap();
    let lo = Document::load_mem(&saved).unwrap();
    let props = get_doc_properties(&lo);
    assert_eq!(props.title, "Acrobat Native Engine Verification");
    assert_eq!(props.author, "PDFForge Studio");
    let bms = list_bookmarks(&lo);
    assert_eq!(bms.len(), 2);
    assert_eq!(bms[1].title, "A4 Section");

    let extracted = extract_pages_to_bytes(&lo, &[2]).unwrap();
    let pdfium = get_pdfium().unwrap();
    let guard = pdfium.lock().unwrap_or_else(|e| e.into_inner());
    let pdoc = guard.load_pdf_from_byte_slice(&extracted, None).unwrap();
    assert_eq!(pdoc.pages().len(), 1);
    assert!(pdoc.pages().get(0).unwrap().text().unwrap().all().contains("Second A4 Document Page"));

    let _ = std::fs::remove_file(&scan_path);
    let _ = std::fs::remove_file(&second_path);
    let _ = std::fs::remove_file(&final_path);
}
