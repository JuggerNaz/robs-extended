//! Minimal, dependency-free PDF report writer.
//!
//! Produces a paginated A4 document from plain structured text: a title, a
//! subtitle, key/value metadata lines, and monospace body lines (the event-log
//! export is the first consumer). No PDF crate is pulled in — a text-only PDF
//! with the base-14 fonts is a small, well-defined format, and hand-rolling it
//! keeps the workspace dependency tree unchanged (same philosophy as the
//! `TestDir` guards in the test suites).
//!
//! # Layout
//! - Page 1 renders a header block: bold title, grey subtitle, metadata pairs,
//!   a horizontal rule, then body lines.
//! - Continuation pages repeat a small header and continue the body.
//! - Body lines are set in `Courier` (fixed advance: `0.6 * size` per glyph),
//!   so wrapping is exact character math: break at the last space inside the
//!   column window, or hard-break when a token exceeds it.
//! - Every page gets a `Page N of M` footer; `M` is known only after layout,
//!   so footers are appended after all body pages are built.
//!
//! # Encoding
//! Strings are written with the standard 14 fonts under `WinAnsiEncoding`
//! (CP1252). [`push_pdf_string`] escapes `\ ( )` and maps characters onto
//! WinAnsi bytes, replacing anything unrepresentable with `?`.
//!
//! The output is a valid PDF 1.7 file: a linear object table, one page + one
//! content-stream object per page, three shared font objects, and a correct
//! cross-reference table (verified byte-for-byte by the xref-offset test).

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};

// ---------------------------------------------------------------------------
// Report model
// ---------------------------------------------------------------------------

/// A structured text report to be rendered as a PDF.
///
/// Build with [`Report::new`], then [`push_meta`](Report::push_meta) for the
/// header block and [`push_line`](Report::push_line) for body lines. The
/// document is fully deterministic for a given content (no internal clock), so
/// callers pass any "generated at" timestamp they want as metadata.
#[derive(Debug, Clone, Default)]
pub struct Report {
    title: String,
    subtitle: String,
    meta: Vec<(String, String)>,
    body_lines: Vec<String>,
}

impl Report {
    /// An empty report with the given header texts.
    pub fn new(title: impl Into<String>, subtitle: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            subtitle: subtitle.into(),
            meta: Vec::new(),
            body_lines: Vec::new(),
        }
    }

    /// Append a `key: value` line to the page-1 header block.
    pub fn push_meta(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.meta.push((key.into(), value.into()));
    }

    /// Append one body line. Long lines are wrapped at render time.
    pub fn push_line(&mut self, line: impl Into<String>) {
        self.body_lines.push(line.into());
    }
}

// ---------------------------------------------------------------------------
// Page geometry
// ---------------------------------------------------------------------------

/// A4 portrait, in PDF points.
const PAGE_W: f32 = 595.0;
const PAGE_H: f32 = 842.0;
/// Symmetric page margin.
const MARGIN: f32 = 48.0;
/// Body text size (Courier).
const BODY_SIZE: f32 = 9.0;
/// Baseline-to-baseline distance for body lines.
const BODY_LEADING: f32 = 12.0;
/// Courier glyph advance is 0.6 em, so columns are exact.
const COURIER_ADVANCE: f32 = 0.6;
/// Body column count = floor(content_width / (size * advance)).
const BODY_COLS: usize = ((PAGE_W - 2.0 * MARGIN) / (BODY_SIZE * COURIER_ADVANCE)) as usize;
/// Body may not descend below this baseline (keeps clear of the footer).
const BODY_BOTTOM: f32 = 64.0;
/// Footer baseline.
const FOOTER_Y: f32 = 30.0;

// ---------------------------------------------------------------------------
// PDF string encoding (WinAnsi)
// ---------------------------------------------------------------------------

/// Append `s` to `out` as a PDF literal string, escaped and mapped onto
/// WinAnsi bytes. Anything CP1252 cannot represent becomes `?`.
fn push_pdf_string(out: &mut Vec<u8>, s: &str) {
    for c in s.chars() {
        // Printable ASCII passes through, with PDF's three escapes.
        match c {
            '\\' => out.extend_from_slice(b"\\\\"),
            '(' => out.extend_from_slice(b"\\("),
            ')' => out.extend_from_slice(b"\\)"),
            '\t' => out.extend_from_slice(b"    "),
            '\r' | '\n' => out.push(b' '),
            _ => {
                let code = c as u32;
                if (0x20..=0x7E).contains(&code) {
                    out.push(code as u8);
                } else {
                    // Punctuation that lives in CP1252's 0x80-0x9F window.
                    let mapped: Option<u8> = match c {
                        '\u{2013}' => Some(0x96), // en dash
                        '\u{2014}' => Some(0x97), // em dash
                        '\u{2018}' => Some(0x91), // left single quote
                        '\u{2019}' => Some(0x92), // right single quote
                        '\u{201C}' => Some(0x93), // left double quote
                        '\u{201D}' => Some(0x94), // right double quote
                        '\u{2022}' => Some(0x95), // bullet
                        '\u{20AC}' => Some(0x80), // euro
                        // Latin-1 supplement maps 1:1 onto CP1252 bytes.
                        '\u{00A0}'..='\u{00FF}' => Some(code as u8),
                        _ => None,
                    };
                    out.push(mapped.unwrap_or(b'?'));
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Layout
// ---------------------------------------------------------------------------

/// A single laid-out page: raw content-stream operators.
struct PageContent(Vec<u8>);

/// Text op: `BT /F<size> Tf r g b rg x y Tm (s) Tj ET`.
fn text_op(out: &mut Vec<u8>, font: &str, size: f32, grey: f32, x: f32, y: f32, s: &str) {
    out.extend_from_slice(b"BT /");
    out.extend_from_slice(font.as_bytes());
    out.extend_from_slice(format!(" {size} Tf {grey} {grey} {grey} rg {x:.2} {y:.2} Tm (").as_bytes());
    push_pdf_string(out, s);
    out.extend_from_slice(b") Tj ET\n");
}

/// Horizontal rule: `x0 y m x1 y l S`.
fn rule_op(out: &mut Vec<u8>, x0: f32, x1: f32, y: f32, grey: f32) {
    out.extend_from_slice(
        format!("{grey} {grey} {grey} RG 1 w {x0:.2} {y:.2} m {x1:.2} {y:.2} l S\n").as_bytes(),
    );
}

/// Wrap one body line to [`BODY_COLS`] columns, breaking at the last space
/// inside the window when possible, hard-breaking tokens wider than the
/// window, and prefixing continuation lines with a two-space indent.
/// Blank input lines are preserved as blank output lines.
fn wrap_line(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut indent = 0usize;
    let mut rest: Vec<char> = line.chars().collect();
    loop {
        if rest.is_empty() {
            // First chunk empty = the caller pushed a blank line; keep it.
            if indent == 0 {
                out.push(String::new());
            }
            break;
        }
        let width = BODY_COLS - indent;
        if rest.len() <= width {
            out.push(indented(indent, &rest));
            break;
        }
        let window: String = rest[..width].iter().collect();
        // The window is always a prefix of `rest`, so a space found at window
        // index `brk` maps 1:1 onto `rest[brk]` — no offset bookkeeping.
        match window.rfind(' ') {
            // Break after the last space; `brk > 0` keeps >=1 char on the line.
            Some(brk) if brk > 0 => {
                out.push(indented(indent, &rest[..brk]));
                rest = rest[brk + 1..].to_vec();
            }
            // No space in the window: hard break.
            _ => {
                out.push(indented(indent, &rest[..width]));
                rest = rest[width..].to_vec();
            }
        }
        indent = 2;
    }
    out
}

/// `indent` spaces followed by the chars of `chunk`.
fn indented(indent: usize, chunk: &[char]) -> String {
    let mut s = String::with_capacity(indent + chunk.len());
    for _ in 0..indent {
        s.push(' ');
    }
    s.extend(chunk.iter().copied());
    s
}

/// Lay out the whole report into per-page content streams.
fn layout(report: &Report) -> Vec<PageContent> {
    let right = PAGE_W - MARGIN;
    let mut pages: Vec<PageContent> = Vec::new();
    let mut cur = PageContent(Vec::new());
    let mut first_page = true;
    let mut y;

    // Header block (page 1 only).
    let start_page = |cur: &mut PageContent, first: bool| -> f32 {
        let mut yy = PAGE_H - MARGIN;
        if first {
            yy -= BODY_SIZE + 5.0; // title baseline (16pt text)
            text_op(&mut cur.0, "/F1", 16.0, 0.10, MARGIN, yy, &report.title);
            yy -= 16.0;
            text_op(&mut cur.0, "/F2", 10.0, 0.45, MARGIN, yy, &report.subtitle);
            yy -= 16.0;
            for (k, v) in &report.meta {
                text_op(&mut cur.0, "/F2", 9.0, 0.25, MARGIN, yy, &format!("{k}: {v}"));
                yy -= 11.0;
            }
            yy -= 8.0;
            rule_op(&mut cur.0, MARGIN, right, yy, 0.6);
            yy -= 20.0;
        } else {
            // Continuation header: small grey title + rule.
            text_op(&mut cur.0, "/F2", 8.0, 0.5, MARGIN, yy - 8.0, &report.title);
            rule_op(&mut cur.0, MARGIN, right, yy - 16.0, 0.8);
            yy -= 32.0;
        }
        yy
    };

    y = start_page(&mut cur, first_page);

    for line in &report.body_lines {
        for piece in wrap_line(line) {
            if y < BODY_BOTTOM {
                pages.push(cur);
                cur = PageContent(Vec::new());
                first_page = false;
                y = start_page(&mut cur, first_page);
            }
            text_op(&mut cur.0, "/F3", BODY_SIZE, 0.15, MARGIN, y, &piece);
            y -= BODY_LEADING;
        }
    }

    // Push the final (or only) page; a header-only report still yields one.
    pages.push(cur);
    pages
}

// ---------------------------------------------------------------------------
// Serialization
// ---------------------------------------------------------------------------

/// Render the report and write it to `path` as a PDF 1.7 file.
pub fn write_pdf(report: &Report, path: &Path) -> Result<()> {
    let bytes = render(report);
    fs::write(path, bytes)
        .with_context(|| format!("writing PDF report to {}", path.display()))
}

/// Render the report to PDF bytes (split out for testability).
fn render(report: &Report) -> Vec<u8> {
    let pages = layout(report);

    // Object numbering:
    //   1 catalog, 2 pages, 3-5 fonts, then (page, content) pairs from 6 on.
    let page_objs: Vec<usize> = (0..pages.len()).map(|i| 6 + 2 * i).collect();

    let mut objects: Vec<Vec<u8>> = Vec::with_capacity(5 + 2 * pages.len());

    objects.push(obj_bytes(1, b"<< /Type /Catalog /Pages 2 0 R >>"));

    let kids: Vec<String> = page_objs.iter().map(|id| format!("{id} 0 R")).collect();
    let pages_dict = format!(
        "<< /Type /Pages /Kids [{}] /Count {} >>",
        kids.join(" "),
        pages.len()
    );
    objects.push(obj_bytes(2, pages_dict.as_bytes()));

    // Font object ids are fixed (3, 4, 5); every page's /Resources dict
    // references them by resource name /F1 /F2 /F3.
    for (id, base) in [(3usize, "Helvetica-Bold"), (4, "Helvetica"), (5, "Courier")] {
        let dict =
            format!("<< /Type /Font /Subtype /Type1 /BaseFont /{base} /Encoding /WinAnsiEncoding >>");
        objects.push(obj_bytes(id, dict.as_bytes()));
    }

    for (i, page) in pages.iter().enumerate() {
        let page_id = page_objs[i];
        let content_id = page_id + 1;
        let page_dict = format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {PAGE_W} {PAGE_H}] /Resources \
             << /Font << /F1 3 0 R /F2 4 0 R /F3 5 0 R >> >> /Contents {content_id} 0 R >>"
        );
        objects.push(obj_bytes(page_id, page_dict.as_bytes()));

        // Footers need the total page count, which is only known here.
        let mut ops = page.0.clone();
        text_op(
            &mut ops,
            "/F2",
            8.0,
            0.5,
            MARGIN,
            FOOTER_Y,
            &format!("Page {} of {}", i + 1, pages.len()),
        );
        let stream = format!(
            "<< /Length {} >>\nstream\n",
            ops.len()
        );
        let mut content = stream.into_bytes();
        content.extend_from_slice(&ops);
        content.extend_from_slice(b"endstream");
        objects.push(obj_bytes(content_id, &content));
    }

    // File assembly: header, objects (recording offsets), xref, trailer.
    let mut out = Vec::with_capacity(4096);
    out.extend_from_slice(b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n");
    let mut offsets = Vec::with_capacity(objects.len());
    for obj in &objects {
        offsets.push(out.len() as u64);
        out.extend_from_slice(obj);
    }

    let xref_at = out.len() as u64;
    out.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for off in &offsets {
        out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer << /Size {} /Root 1 0 R >>\nstartxref\n{xref_at}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    out
}

/// Wrap `body` in `N 0 obj ... endobj`.
fn obj_bytes(id: usize, body: &[u8]) -> Vec<u8> {
    let mut v = format!("{id} 0 obj\n").into_bytes();
    v.extend_from_slice(body);
    v.extend_from_slice(b"\nendobj\n");
    v
}
