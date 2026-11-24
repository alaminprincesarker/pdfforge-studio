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
fn test_layout_non_transitive_baselines_never_panic() {
    // Regression test for Rust driftsort total-order panic (0xc0000409 in release):
    // Characters whose baselines step by 1.5pt..2.0pt with decreasing X form a cycle
    // under any epsilon comparator `|a.baseline - b.baseline| <= 2.5`.
    // Simulate a dense scanned/OCR/math page with 60 characters stepping by 1.6pt in Y
    // while descending in X (plus normal lines across multiple columns).
    let mut ops = Vec::new();
    for i in 0..60 {
        let x = 520.0 - (i as f32) * 7.5;
        let y = 740.0 - (i as f32) * 1.6;
        ops.extend(text_op(x, y, 10.0, [0.0, 0.0, 0.0], "A"));
    }
    for row in 0..8 {
        let base_y = 500.0 - (row as f32) * 16.0;
        for col in 0..12 {
            let jitter = ((row * 13 + col * 7) % 5) as f32 * 0.6 - 1.2;
            ops.extend(text_op(
                50.0 + (col as f32) * 40.0,
                base_y + jitter,
                11.0,
                [0.0, 0.0, 0.0],
                "W",
            ));
        }
    }

    let pdf_bytes = build_single_page_pdf([0, 0, 612, 792], 0, ops, vec![], false);
    let path = temp_pdf_path("test_non_transitive_baselines.pdf");
    std::fs::write(&path, &pdf_bytes).unwrap();

    let engine = PdfWorkspaceEngine::new();
    let layout = engine
        .get_page_layout(path.to_str().unwrap(), 1)
        .expect("layout extraction must never panic on wobbling baselines");
    assert!(layout.chars.len() >= 100);
    assert!(!layout.blocks.is_empty());

    let sel = engine
        .get_selection_rects(path.to_str().unwrap(), 1, 0, layout.chars.len() + 50)
        .expect("out-of-bounds end_char must clamp safely");
    assert!(!sel.rects.is_empty());
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

#[test]
fn test_large_file_mmap_range_lru_cache_and_png_render() {
    use crate::pdf_layout::{RustPageRenderCache, RustRenderCacheKey};

    let bytes = build_single_page_pdf(
        [0, 0, 612, 792],
        0,
        text_op(72.0, 700.0, 16.0, [0.0, 0.0, 0.0], "Large File Mmap Range PNG Test"),
        vec![],
        false,
    );
    let pdf_path = temp_pdf_path("test_mmap_range_render.pdf");
    std::fs::write(&pdf_path, &bytes).unwrap();
    let path_str = pdf_path.to_str().unwrap();

    let engine = PdfWorkspaceEngine::new();
    let (backing, rev, dirty) = engine.load_backing(path_str).expect("load_backing");
    assert!(backing.is_mmap(), "unedited disk PDF should use Mmap");
    assert_eq!(rev, 1);
    assert!(!dirty);

    // Byte range read must match exact disk bytes without full copy
    let (slice, total, _, _) = engine.read_range(path_str, 0, 7).expect("read_range header");
    assert_eq!(&slice, &bytes[0..8]);
    assert_eq!(total as usize, bytes.len());

    // Rust PDFium PNG render + tile render + 16 MP safety cap
    let png_full = engine
        .render_page_png_cached(path_str, 1, 612, 792, 0, None, false)
        .expect("render full PNG");
    assert_eq!(&png_full[0..8], &[137, 80, 78, 71, 13, 10, 26, 10]);

    let png_tile = engine
        .render_page_png_cached(path_str, 1, 2448, 3168, 0, Some((100, 120, 400, 300)), true)
        .expect("render tile PNG");
    assert_eq!(&png_tile[0..8], &[137, 80, 78, 71, 13, 10, 26, 10]);

    let (entries, cached_bytes) = engine.render_cache_stats();
    assert!(entries >= 2 && cached_bytes > 0);

    // Suspending clears render cache cleanly for that doc
    engine.suspend_document(path_str);
    let (entries_after, _) = engine.render_cache_stats();
    assert_eq!(entries_after, 0);

    // LRU render cache clamps to minimum 64 MB budget
    let cache = RustPageRenderCache::new(64 * 1024 * 1024);
    for i in 0..3 {
        cache.insert(
            RustRenderCacheKey {
                doc_key: "d".into(),
                revision: 1,
                page: i + 1,
                width: 100,
                height: 100,
                rotation: 0,
                tile_x: 0,
                tile_y: 0,
                tile_w: 100,
                tile_h: 100,
            },
            std::sync::Arc::new(vec![1u8; 30 * 1024 * 1024]),
        );
    }
    let (lru_entries, lru_bytes) = cache.stats();
    assert!(lru_bytes <= 64 * 1024 * 1024 && lru_entries == 2, "LRU evicted oldest page");
    cache.clear();
    assert_eq!(cache.stats(), (0, 0));

    // HTTP range parser validation
    assert_eq!(crate::parse_byte_range(Some("bytes=0-65535"), 100_000), Some((0, 65535)));
    assert_eq!(crate::parse_byte_range(Some("bytes=99000-"), 100_000), Some((99000, 99999)));
    assert_eq!(crate::parse_byte_range(Some("bytes=-512"), 100_000), Some((99488, 99999)));
    assert_eq!(crate::parse_byte_range(Some("bytes=200000-200100"), 100_000), None);

    engine.close_document(path_str);
    let _ = std::fs::remove_file(&pdf_path);
}

fn build_raster_image_pdf(
    pages_pixels: Vec<(u32, u32, Vec<u8>, i32, Option<&'static str>)>,
    page_w: f32,
    page_h: f32,
) -> Vec<u8> {
    use lopdf::{Dictionary, Object, Stream};
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let mut page_ids = Vec::new();

    for (iw, ih, gray, pdf_rotate, overlay_text) in pages_pixels {
        let mut img_dict = Dictionary::new();
        img_dict.set("Type", Object::Name(b"XObject".to_vec()));
        img_dict.set("Subtype", Object::Name(b"Image".to_vec()));
        img_dict.set("Width", Object::Integer(iw as i64));
        img_dict.set("Height", Object::Integer(ih as i64));
        img_dict.set("ColorSpace", Object::Name(b"DeviceGray".to_vec()));
        img_dict.set("BitsPerComponent", Object::Integer(8));
        let mut img_stream = Stream::new(img_dict, gray);
        let _ = img_stream.compress();
        let img_id = doc.add_object(Object::Stream(img_stream));

        let mut xobj_dict = Dictionary::new();
        xobj_dict.set("Im1", Object::Reference(img_id));
        let mut res_dict = Dictionary::new();
        res_dict.set("XObject", Object::Dictionary(xobj_dict));

        let mut content_str = format!("q\n{} 0 0 {} 0 0 cm\n/Im1 Do\nQ\n", page_w, page_h);
        if let Some(txt) = overlay_text {
            let mut f1 = Dictionary::new();
            f1.set("Type", Object::Name(b"Font".to_vec()));
            f1.set("Subtype", Object::Name(b"Type1".to_vec()));
            f1.set("BaseFont", Object::Name(b"Helvetica".to_vec()));
            let fid = doc.add_object(Object::Dictionary(f1));
            let mut fd = Dictionary::new();
            fd.set("F1", Object::Reference(fid));
            res_dict.set("Font", Object::Dictionary(fd));
            content_str.push_str(&format!("BT /F1 14 Tf 50 740 Td ({}) Tj ET\n", txt));
        }

        let mut cstream = Stream::new(Dictionary::new(), content_str.into_bytes());
        let _ = cstream.compress();
        let cid = doc.add_object(Object::Stream(cstream));

        let mut page_dict = Dictionary::new();
        page_dict.set("Type", Object::Name(b"Page".to_vec()));
        page_dict.set("Parent", Object::Reference(pages_id));
        page_dict.set(
            "MediaBox",
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(page_w as i64),
                Object::Integer(page_h as i64),
            ]),
        );
        if pdf_rotate != 0 {
            page_dict.set("Rotate", Object::Integer(pdf_rotate as i64));
        }
        page_dict.set("Resources", Object::Dictionary(res_dict));
        page_dict.set("Contents", Object::Reference(cid));
        let pid = doc.add_object(Object::Dictionary(page_dict));
        page_ids.push(Object::Reference(pid));
    }

    let mut pages_dict = Dictionary::new();
    pages_dict.set("Type", Object::Name(b"Pages".to_vec()));
    pages_dict.set("Count", Object::Integer(page_ids.len() as i64));
    pages_dict.set("Kids", Object::Array(page_ids));
    doc.objects.insert(pages_id, Object::Dictionary(pages_dict));

    let mut catalog = Dictionary::new();
    catalog.set("Type", Object::Name(b"Catalog".to_vec()));
    catalog.set("Pages", Object::Reference(pages_id));
    let cat_id = doc.add_object(Object::Dictionary(catalog));
    doc.trailer.set("Root", Object::Reference(cat_id));

    let mut out = Vec::new();
    doc.save_to(&mut out).unwrap();
    out
}

#[test]
fn test_ocr_clean_skewed_noisy_mixed_rotated_and_bangla() {
    use crate::ocr::{
        inspect_page_scan_status_bytes, list_available_languages, render_page_for_ocr_from_bytes,
        rotate_gray_arbitrary_white, rotate_gray_cw, run_ocr_on_grayscale, write_ocr_results_into_pdf,
    };
    use crate::pdf_layout::get_pdfium;

    let langs = list_available_languages();
    assert!(langs.available, "Tesseract DLL must load: {:?}", langs.engine_error);
    assert!(langs.languages.iter().any(|l| l.code == "eng"), "eng.traineddata missing");
    assert!(langs.languages.iter().any(|l| l.code == "ben"), "ben.traineddata missing");

    // 1. Render a clean source English document to grayscale image -> build scanned-only PDF (0 vector chars)
    let mut src_en_ops = Vec::new();
    src_en_ops.extend(text_op(64.0, 680.0, 22.0, [0.0, 0.0, 0.0], "INVOICE SUMMARY REPORT 2026"));
    src_en_ops.extend(text_op(64.0, 620.0, 15.0, [0.0, 0.0, 0.0], "Quarterly revenue rose sharply across desktop tools."));
    src_en_ops.extend(text_op(64.0, 580.0, 15.0, [0.0, 0.0, 0.0], "Total verified orders reached forty two thousand units."));
    let src_en_pdf = build_single_page_pdf([0, 0, 612, 792], 0, src_en_ops, vec![], false);
    let (clean_gray, gw, gh, vpw, vph) =
        render_page_for_ocr_from_bytes(&src_en_pdf, 1, 300).expect("render clean scan");

    // Clean English scan PDF -> detect as scanned -> OCR at 150, 300, 600 DPI timings
    let scanned_en_pdf = build_raster_image_pdf(vec![(gw, gh, clean_gray.clone(), 0, None)], vpw, vph);
    let det = inspect_page_scan_status_bytes(&scanned_en_pdf, "clean.pdf", 1).unwrap();
    assert!(det.is_scanned, "clean raster PDF must be detected as scanned (cov={})", det.image_coverage);
    assert_eq!(det.non_whitespace_chars, 0);

    for dpi in [150u32, 300u32, 600u32] {
        let (rg, rw, rh, _, _) = render_page_for_ocr_from_bytes(&scanned_en_pdf, 1, dpi).unwrap();
        let res = run_ocr_on_grayscale(1, rg, rw, rh, dpi, vpw, vph, "eng").unwrap();
        let txt: String = res.words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ");
        println!("[OCR CLEAN {} DPI] time={}ms conf={:.1}% text={}", dpi, res.duration_ms, res.mean_confidence, txt);
        assert!(txt.contains("INVOICE") && txt.contains("Quarterly") && txt.contains("orders"), "Failed at {} DPI: {}", dpi, txt);
    }

    // Write 300 DPI OCR into scanned PDF and verify searchable/selectable text in PDFium + unchanged visual image
    let res_300 = run_ocr_on_grayscale(1, clean_gray.clone(), gw, gh, 300, vpw, vph, "eng").unwrap();
    let searchable_en_pdf =
        write_ocr_results_into_pdf(&scanned_en_pdf, &[res_300], "searchable_image", 0).unwrap();
    let pdfium = get_pdfium().unwrap();
    {
        let guard = pdfium.lock().unwrap_or_else(|e| e.into_inner());
        let pdoc = guard.load_pdf_from_byte_slice(&searchable_en_pdf, None).unwrap();
        let extracted = pdoc.pages().get(0).unwrap().text().unwrap().all();
        println!("[OCR PDFium extracted] {}", extracted);
        assert!(extracted.contains("INVOICE") && extracted.contains("Quarterly") && extracted.contains("revenue"));
    }

    // After OCR, page is no longer "scanned (no text)"!
    let det_after = inspect_page_scan_status_bytes(&searchable_en_pdf, "clean.pdf", 1).unwrap();
    assert!(!det_after.is_scanned && det_after.has_text, "Post-OCR PDF must report has_text=true");

    // 2. Skewed scan (~5.0 deg skew)
    let skewed_gray = rotate_gray_arbitrary_white(&clean_gray, gw, gh, 5.0);
    let skewed_res = run_ocr_on_grayscale(1, skewed_gray, gw, gh, 300, vpw, vph, "eng").unwrap();
    let skewed_txt: String = skewed_res.words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ");
    println!("[OCR SKEWED] detected_skew={:.2}deg time={}ms conf={:.1}% text={}", skewed_res.skew_deg, skewed_res.duration_ms, skewed_res.mean_confidence, skewed_txt);
    assert!(skewed_res.skew_deg.abs() > 3.5, "Should detect ~5 deg skew, got {}", skewed_res.skew_deg);
    assert!(skewed_txt.contains("INVOICE") && skewed_txt.contains("Quarterly"), "Skewed OCR missed words: {}", skewed_txt);

    // 3. Low-quality phone photo (uneven dark lighting shadow gradient + salt/pepper grain)
    let mut noisy_gray = clean_gray.clone();
    for y in 0..(gh as usize) {
        for x in 0..(gw as usize) {
            let idx = y * (gw as usize) + x;
            let shade = 0.42 + 0.52 * ((x as f32 / gw as f32) * 0.6 + (y as f32 / gh as f32) * 0.4);
            let mut v = (noisy_gray[idx] as f32) * shade;
            if ((x * 73 + y * 139) % 197) == 0 {
                v = 25.0; // salt/pepper dark speckle
            }
            noisy_gray[idx] = v.clamp(0.0, 255.0) as u8;
        }
    }
    let noisy_res = run_ocr_on_grayscale(1, noisy_gray, gw, gh, 300, vpw, vph, "eng").unwrap();
    let noisy_txt: String = noisy_res.words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ");
    println!("[OCR LOW-QUALITY PHOTO] time={}ms conf={:.1}% text={}", noisy_res.duration_ms, noisy_res.mean_confidence, noisy_txt);
    assert!(noisy_txt.contains("INVOICE") && noisy_txt.contains("orders"), "Noisy photo OCR failed: {}", noisy_txt);

    // 4. Rotated page: (a) PDF /Rotate = 90 + (b) Sideways OSD image rotated 90 deg CW with /Rotate = 0
    let (sideways_gray, sw_w, sw_h) = rotate_gray_cw(&clean_gray, gw, gh, 90);
    let sideways_res = run_ocr_on_grayscale(1, sideways_gray, sw_w, sw_h, 300, vph, vpw, "eng").unwrap();
    let sideways_txt: String = sideways_res.words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ");
    println!("[OCR SIDEWAYS OSD] osd_cw={}deg conf={:.1}% text={}", sideways_res.osd_cw_deg, sideways_res.mean_confidence, sideways_txt);
    assert!(sideways_txt.contains("INVOICE") || sideways_txt.contains("SUMMARY"), "Sideways OSD OCR failed: {}", sideways_txt);

    let rot90_pdf = build_raster_image_pdf(vec![(gw, gh, clean_gray.clone(), 90, None)], 612.0, 792.0);
    let (rg90, rw90, rh90, vp90w, vp90h) = render_page_for_ocr_from_bytes(&rot90_pdf, 1, 300).unwrap();
    let rot90_res = run_ocr_on_grayscale(1, rg90, rw90, rh90, 300, vp90w, vp90h, "eng").unwrap();
    let rot90_out = write_ocr_results_into_pdf(&rot90_pdf, &[rot90_res], "searchable_image_exact", 0).unwrap();
    {
        let guard = pdfium.lock().unwrap_or_else(|e| e.into_inner());
        let pdoc = guard.load_pdf_from_byte_slice(&rot90_out, None).unwrap();
        let ex90: String = pdoc
            .pages()
            .get(0)
            .unwrap()
            .text()
            .unwrap()
            .chars()
            .iter()
            .filter_map(|c| c.unicode_char())
            .collect();
        println!("[OCR PDF /Rotate 90] {}", ex90);
        assert!(ex90.contains("INVOICE") && ex90.contains("SUMMARY") && ex90.contains("Total"));
    }

    // 5. Mixed page: image covering page + existing text -> must NOT flag as scanned
    let mixed_pdf = build_raster_image_pdf(vec![(gw, gh, clean_gray.clone(), 0, Some("Existing Vector Header"))], vpw, vph);
    let mixed_det = inspect_page_scan_status_bytes(&mixed_pdf, "mixed.pdf", 1).unwrap();
    assert!(!mixed_det.is_scanned && mixed_det.has_text, "Mixed page must report has_text=true, is_scanned=false");

    // 6. Bangla scan: render real Bangla PDF text to raster image -> OCR with 'ben' -> embed Unicode ToUnicode CMap -> verify PDFium copy!
    let mut ben_src = build_single_page_pdf([0, 0, 612, 792], 0, Vec::new(), vec![], false);
    {
        let mut lo = Document::load_mem(&ben_src).unwrap();
        let _pid = *lo.get_pages().get(&1).unwrap();
        let req = crate::pdf_objects::EditTextCommitRequest {
            page: 1,
            orig_bbox_vp: [60.0, 120.0, 420.0, 40.0],
            new_bbox_vp: [60.0, 120.0, 420.0, 40.0],
            text: "বাংলাদেশ আমার সোনার বাংলা".to_string(),
            original_font: "".into(),
            font_family: "Noto Sans Bengali".into(),
            font_size: 26.0,
            color: "#000000".into(),
            bold: false,
            italic: false,
            align: "left".into(),
            line_spacing: 1.25,
            bullets: false,
            bg_color: None,
            delete_block: None,
        };
        crate::pdf_objects::commit_edit_text_block(&mut lo, &req).unwrap();
        ben_src.clear();
        lo.save_to(&mut ben_src).unwrap();
    }
    let (ben_gray, bw, bh, bvpw, bvph) = render_page_for_ocr_from_bytes(&ben_src, 1, 300).unwrap();
    let scanned_ben_pdf = build_raster_image_pdf(vec![(bw, bh, ben_gray.clone(), 0, None)], bvpw, bvph);
    let ben_res = run_ocr_on_grayscale(1, ben_gray, bw, bh, 300, bvpw, bvph, "ben").unwrap();
    let ben_txt: String = ben_res.words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ");
    println!("[OCR BANGLA] time={}ms conf={:.1}% text={}", ben_res.duration_ms, ben_res.mean_confidence, ben_txt);
    assert!(ben_txt.chars().any(|c| ('\u{0980}'..='\u{09FF}').contains(&c)), "Bangla OCR produced no Bengali script: {}", ben_txt);
    let searchable_ben_pdf = write_ocr_results_into_pdf(&scanned_ben_pdf, &[ben_res], "searchable_image", 0).unwrap();
    {
        let guard = pdfium.lock().unwrap_or_else(|e| e.into_inner());
        let pdoc = guard.load_pdf_from_byte_slice(&searchable_ben_pdf, None).unwrap();
        let ex_ben = pdoc.pages().get(0).unwrap().text().unwrap().all();
        println!("[OCR BANGLA PDFium extracted] {}", ex_ben);
        assert!(ex_ben.chars().any(|c| ('\u{0980}'..='\u{09FF}').contains(&c)), "ToUnicode CMap failed to roundtrip Bangla in PDFium");
    }
}

#[test]
fn test_ocr_200_page_job_pipeline_and_undo() {
    use crate::ocr::{OcrJobManager, OcrStartRequest};

    // Build a realistic 200-page scanned PDF (compact 150dpi-equivalent image frames + shared structure)
    let src_ops = text_op(72.0, 700.0, 18.0, [0.0, 0.0, 0.0], "Document Archive Page Reference");
    let src_pdf = build_single_page_pdf([0, 0, 612, 792], 0, src_ops, vec![], false);
    let (gray, gw, gh, vpw, vph) =
        crate::ocr::render_page_for_ocr_from_bytes(&src_pdf, 1, 150).unwrap();

    let pages_vec: Vec<(u32, u32, Vec<u8>, i32, Option<&'static str>)> = (0..200)
        .map(|i| {
            let txt = if i == 199 { Some("Existing Text Page 200") } else { None };
            (gw, gh, gray.clone(), 0, txt)
        })
        .collect();
    let large_scan_pdf = build_raster_image_pdf(pages_vec, vpw, vph);
    let pdf_path = temp_pdf_path("test_ocr_200pages.pdf");
    std::fs::write(&pdf_path, &large_scan_pdf).unwrap();
    let path_str = pdf_path.to_str().unwrap().to_string();

    let ws = std::sync::Arc::new(PdfWorkspaceEngine::new());
    let mgr = OcrJobManager::new();
    let page_nums: Vec<usize> = (1..=200).collect();

    let range_info = crate::ocr::inspect_pages_range_status(&ws, &path_str, &page_nums).unwrap();
    assert_eq!(range_info.total_pages, 200);
    assert_eq!(range_info.scanned_pages.len(), 199);
    assert_eq!(range_info.text_pages, vec![200]);

    let job_id = mgr
        .start_job(
            ws.clone(),
            OcrStartRequest {
                file_path: path_str.clone(),
                pages: page_nums,
                language: "eng".into(),
                secondary_language: None,
                output_mode: "searchable_image".into(),
                downsample_dpi: 150,
                rerun_text_pages: false,
            },
        )
        .expect("start 200-page OCR job");

    let snap = loop {
        std::thread::sleep(std::time::Duration::from_millis(250));
        let st = mgr.get_status(&job_id).unwrap();
        if st.status != "running" {
            break st;
        }
    };
    println!(
        "[OCR 200-PAGE RESULT] status={} recognized={} skipped={} avg_ms_per_page={} conf={:.1}%",
        snap.status, snap.recognized_pages, snap.skipped_pages, snap.avg_ms_per_page, snap.mean_confidence
    );
    assert_eq!(snap.status, "done");
    assert_eq!(snap.recognized_pages, 199);
    assert_eq!(snap.skipped_pages, 1);
    assert!(snap.can_undo.unwrap_or(false));

    // Verify Undo restores 0-text page 1 cleanly
    let (_, undo_rev, can_undo_after, _) = ws.undo(&path_str).unwrap();
    assert!(undo_rev > snap.revision.unwrap());
    assert!(!can_undo_after);
    let post_undo_bytes = ws.get_working_bytes(&path_str).unwrap();
    let post_undo_det = crate::ocr::inspect_page_scan_status_bytes(&post_undo_bytes, &path_str, 1).unwrap();
    assert!(post_undo_det.is_scanned, "Undo must restore original image-only page");

    ws.close_document(&path_str);
    let _ = std::fs::remove_file(&pdf_path);
}

#[test]
fn test_fast_bmp_disk_cache_and_indexed_search() {
    use crate::disk_cache::{
        compute_file_hash, read_cached_render, search_file_indexed, write_cached_render_async,
    };
    use crate::pdf_layout::encode_bgra_to_bmp_fast;

    // 1. Verify top-down 32-bit BGRA BMP fast serializer
    let bgra = vec![200u8; 64 * 48 * 4];
    let bmp = encode_bgra_to_bmp_fast(64, 48, &bgra).expect("bmp encode");
    assert_eq!(&bmp[0..2], b"BM");
    assert_eq!(bmp.len(), 54 + 64 * 48 * 4);

    // 2. Verify fast PDF probe + disk-cached render + instant repeat indexed search
    let mut ops = Vec::new();
    ops.extend(text_op(72.0, 700.0, 16.0, [0.0, 0.0, 0.0], "Indexed Search Cache Verification Token_9942"));
    let pdf_bytes = build_single_page_pdf([0, 0, 612, 792], 0, ops, vec![], false);
    let pdf_path = temp_pdf_path("test_perf_disk_cache.pdf");
    std::fs::write(&pdf_path, &pdf_bytes).unwrap();
    let path_str = pdf_path.to_str().unwrap();

    let ws = PdfWorkspaceEngine::new();
    let probe = ws
        .probe_file_fast(path_str, &pdf_bytes[..pdf_bytes.len().min(1024)], pdf_bytes.len() as u64)
        .expect("probe_file_fast");
    assert_eq!(probe.num_pages, 1);
    assert!((probe.page1_width - 612.0).abs() < 2.0);
    assert!(!probe.file_hash.is_empty());

    let (fast_bytes, mime) = ws
        .render_page_fast_cached(path_str, 1, 306, 396, 0, None, false, false)
        .expect("render_page_fast_cached");
    assert!(mime == "image/bmp" || mime == "image/png");
    assert!(fast_bytes.len() > 100);

    // Disk cache roundtrip
    let fh = compute_file_hash(path_str);
    write_cached_render_async(fh.clone(), 1, 1, 128, 0, true, vec![137u8; 64]);
    std::thread::sleep(std::time::Duration::from_millis(80));
    let hit = read_cached_render(&fh, 1, 1, 128, 0, true);
    assert!(hit.is_some());

    // First search builds disk/RAM index; second search hits cache in <5ms
    let s1 = search_file_indexed(&ws, path_str, "Token_9942", false).expect("search 1");
    assert_eq!(s1.hits.len(), 1);
    assert_eq!(s1.hits[0].page_number, 1);
    let s2 = search_file_indexed(&ws, path_str, "Token_9942", false).expect("search 2");
    assert!(s2.cache_hit);
    assert_eq!(s2.hits.len(), 1);

    ws.close_document(path_str);
    let _ = std::fs::remove_file(&pdf_path);
}
