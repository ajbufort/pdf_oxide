//! `extract_spans_filtered_with_reading_order` combines reading order with
//! optional-content / ink filtering.
//!
//! The motivating real-world shape (found in a web-crawl corpus): a PDF whose
//! `/OCProperties/D` turns a layer OFF, where that hidden layer holds a COPY of
//! the visible content. `render_page` honours `/D` and shows one copy; span
//! extraction ignores `/D` unless told otherwise and yields TWO - so every word
//! on the page is emitted twice. Nothing in the existing API could fix that: the
//! reading-order call cannot filter, and the filtered call returns assembled
//! text rather than positioned spans.

use std::collections::HashSet;

use pdf_oxide::document::{PdfDocument, ReadingOrder};
use pdf_oxide::optional_content;

/// A page that draws "REPORT" plainly, and draws "REPORT" a SECOND time inside
/// a `BDC /OC /MC0` scope whose OCG ("Hidden") is listed in `/D/OFF`. The two
/// copies sit at different y so neither can be dismissed as a dedup artifact.
fn pdf_with_default_off_duplicate_layer() -> Vec<u8> {
    let mut pdf = Vec::new();
    let mut offsets: Vec<usize> = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.5\n");

    // Catalog: the OCG is declared AND explicitly turned off in the default config.
    offsets.push(pdf.len());
    pdf.extend_from_slice(
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R\n\
           /OCProperties << /OCGs [6 0 R] /D << /OFF [6 0 R] >> >> >>\nendobj\n\n",
    );

    offsets.push(pdf.len());
    pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n\n");

    offsets.push(pdf.len());
    pdf.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792]\n\
           /Contents 4 0 R\n\
           /Resources << /Font << /F1 5 0 R >> /Properties << /MC0 6 0 R >> >> >>\nendobj\n\n",
    );

    // Visible copy at y=700; hidden copy at y=600 inside the OFF layer.
    let content = b"BT /F1 24 Tf 72 700 Td (REPORT) Tj ET\n\
                    /OC /MC0 BDC BT /F1 24 Tf 72 600 Td (REPORT) Tj ET EMC\n";
    offsets.push(pdf.len());
    let hdr = format!("4 0 obj\n<< /Length {} >>\nstream\n", content.len());
    pdf.extend_from_slice(hdr.as_bytes());
    pdf.extend_from_slice(content);
    pdf.extend_from_slice(b"\nendstream\nendobj\n\n");

    offsets.push(pdf.len());
    pdf.extend_from_slice(
        b"5 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica\n\
           /Encoding /WinAnsiEncoding >>\nendobj\n\n",
    );

    offsets.push(pdf.len());
    pdf.extend_from_slice(b"6 0 obj\n<< /Type /OCG /Name /Hidden >>\nendobj\n\n");

    let xref_offset = pdf.len();
    let n_obj = offsets.len() + 1;
    let mut xref = format!("xref\n0 {n_obj}\n0000000000 65535 f \n");
    for off in &offsets {
        xref.push_str(&format!("{off:010} 00000 n \n"));
    }
    pdf.extend_from_slice(xref.as_bytes());
    pdf.extend_from_slice(
        format!("trailer\n<< /Size {n_obj} /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n")
            .as_bytes(),
    );
    pdf
}

fn count_report(spans: &[pdf_oxide::layout::TextSpan]) -> usize {
    spans.iter().filter(|s| s.text.contains("REPORT")).count()
}

/// The whole point: excluding the document's own default-off layer drops the
/// duplicate copy, leaving exactly the one the page displays.
#[test]
fn excluding_default_off_layer_drops_the_duplicate_copy() {
    let doc = PdfDocument::from_bytes(pdf_with_default_off_duplicate_layer()).expect("parse");

    let unfiltered = doc
        .extract_spans_with_reading_order(0, ReadingOrder::Structure)
        .expect("unfiltered");
    assert_eq!(
        count_report(&unfiltered),
        2,
        "without a filter BOTH copies are extracted - this is the bug being fixed"
    );

    let hidden = optional_content::compute_default_off_ocgs(&doc);
    assert!(hidden.contains("Hidden"), "/D/OFF must mark the OCG off; got {hidden:?}");

    let filtered = doc
        .extract_spans_filtered_with_reading_order(
            0,
            ReadingOrder::Structure,
            hidden,
            HashSet::new(),
        )
        .expect("filtered");
    assert_eq!(
        count_report(&filtered),
        1,
        "the default-off layer's copy must be gone, matching what render_page shows"
    );
}

/// Empty filter sets must be EXACTLY the unfiltered call - the new method is a
/// superset of the old one, so existing callers cannot shift.
#[test]
fn empty_filters_are_identical_to_the_unfiltered_call() {
    let doc = PdfDocument::from_bytes(pdf_with_default_off_duplicate_layer()).expect("parse");
    for order in [
        ReadingOrder::TopToBottom,
        ReadingOrder::ColumnAware,
        ReadingOrder::Structure,
    ] {
        let plain: Vec<String> = doc
            .extract_spans_with_reading_order(0, order)
            .expect("plain")
            .into_iter()
            .map(|s| s.text)
            .collect();
        let empty: Vec<String> = doc
            .extract_spans_filtered_with_reading_order(0, order, HashSet::new(), HashSet::new())
            .expect("empty-filtered")
            .into_iter()
            .map(|s| s.text)
            .collect();
        assert_eq!(plain, empty, "empty filters must not change {order:?} output");
    }
}

/// Filtering must not disturb the requested ordering: the surviving span is the
/// visible one whatever strategy is asked for.
#[test]
fn filtering_preserves_the_requested_reading_order() {
    let doc = PdfDocument::from_bytes(pdf_with_default_off_duplicate_layer()).expect("parse");
    let hidden = optional_content::compute_default_off_ocgs(&doc);
    for order in [
        ReadingOrder::TopToBottom,
        ReadingOrder::ColumnAware,
        ReadingOrder::Structure,
    ] {
        let spans = doc
            .extract_spans_filtered_with_reading_order(0, order, hidden.clone(), HashSet::new())
            .expect("filtered");
        assert_eq!(count_report(&spans), 1, "{order:?}: duplicate must be gone");
    }
}

/// A one-page PDF (MediaBox 0 0 200 200) that draws "VISIBLE" on the page and
/// "OFFPAGE" far above it (y=5000, way outside the MediaBox). Docs that reuse one
/// big Form XObject across pages rely on a `W n` clip to hide such off-page text;
/// the raw extractor does not honour `W n`, so the reading-order path must drop
/// spans that lie entirely outside the MediaBox - as `extract_spans` already does.
fn pdf_with_offpage_text() -> Vec<u8> {
    let mut pdf = Vec::new();
    let mut off: Vec<usize> = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.5\n");
    off.push(pdf.len());
    pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n\n");
    off.push(pdf.len());
    pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n\n");
    off.push(pdf.len());
    pdf.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200]\n\
           /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>\nendobj\n\n",
    );
    let content = b"BT /F1 12 Tf 50 100 Td (VISIBLE) Tj ET\n\
                    BT /F1 12 Tf 50 5000 Td (OFFPAGE) Tj ET\n";
    off.push(pdf.len());
    pdf.extend_from_slice(format!("4 0 obj\n<< /Length {} >>\nstream\n", content.len()).as_bytes());
    pdf.extend_from_slice(content);
    pdf.extend_from_slice(b"\nendstream\nendobj\n\n");
    off.push(pdf.len());
    pdf.extend_from_slice(
        b"5 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
           /Encoding /WinAnsiEncoding >>\nendobj\n\n",
    );
    let xref = pdf.len();
    let mut x = String::from("xref\n0 6\n0000000000 65535 f \n");
    for o in &off {
        x.push_str(&format!("{o:010} 00000 n \n"));
    }
    pdf.extend_from_slice(x.as_bytes());
    pdf.extend_from_slice(
        format!("trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
    );
    pdf
}

/// The reading-order path drops text lying entirely outside the MediaBox, matching
/// `extract_spans`. Before this, a doc reusing one big Form XObject emitted every
/// page's worth of off-page spans (measured: ~5x a chart's visible labels).
#[test]
fn reading_order_drops_offpage_spans() {
    let doc = PdfDocument::from_bytes(pdf_with_offpage_text()).expect("parse");
    let texts: Vec<String> = doc
        .extract_spans_filtered_with_reading_order(
            0,
            ReadingOrder::TopToBottom,
            HashSet::new(),
            HashSet::new(),
        )
        .expect("spans")
        .into_iter()
        .map(|s| s.text.trim().to_string())
        .filter(|t| !t.is_empty())
        .collect();
    assert!(
        texts.iter().any(|t| t.contains("VISIBLE")),
        "on-page text must survive: {texts:?}"
    );
    assert!(
        !texts.iter().any(|t| t.contains("OFFPAGE")),
        "off-MediaBox text must be dropped: {texts:?}"
    );
}
