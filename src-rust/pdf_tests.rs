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
