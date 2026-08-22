//! Integration tests for the dependency-free PDF report writer.
//!
//! All tests are pure (no ffmpeg / no external PDF tooling): they validate the
//! *structure* of the produced bytes — header/trailer markers, object counts,
//! the cross-reference table pointing exactly at each object, string escaping,
//! WinAnsi mapping, and pagination math. `Report` is deterministic (no internal
//! clock), so byte-level assertions are stable.
//!
//! No `tempfile` dependency: the [`TestDir`] guard owns the output directory,
//! mirroring `tests/blackbox.rs` and `tests/anomaly.rs`.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use robs_outputs::report::write_pdf;
use robs_outputs::Report;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// RAII temp directory, removed on drop.
struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let mut p = std::env::temp_dir();
        p.push(format!("robs-report-test-{}-{nanos}", std::process::id()));
        fs::create_dir_all(&p).expect("create test dir");
        Self(p)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A small deterministic report.
fn sample_report(lines: usize) -> Report {
    let mut r = Report::new("ROBS Event Log Report", "Session event log export");
    r.push_meta("Generated", "2026-08-22 09:00:00");
    r.push_meta("Entries", lines.to_string());
    for i in 0..lines {
        r.push_line(format!("2026-08-22 09:00:{:02}  [Info]  event number {i}", i % 60));
    }
    r
}

/// Write `report` into a TestDir and read the bytes back.
fn render_to_bytes(report: &Report) -> Vec<u8> {
    let dir = TestDir::new();
    let out = dir.path().join("report.pdf");
    write_pdf(report, &out).expect("write pdf");
    assert!(out.exists(), "output file must exist");
    fs::read(&out).expect("read pdf back")
}

/// Count non-overlapping occurrences of `needle` in `haystack`.
fn count(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .filter(|w| *w == needle)
        .count()
}

// ===========================================================================
// Structure
// ===========================================================================

#[test]
fn pdf_has_valid_header_and_trailer_markers() {
    let bytes = render_to_bytes(&sample_report(3));
    assert!(bytes.starts_with(b"%PDF-1.7\n"), "must start with the PDF marker");
    let tail = &bytes[bytes.len() - 16..];
    assert!(tail.ends_with(b"%%EOF\n"), "must end with %%EOF, got tail {tail:?}");
    assert!(bytes.windows(14).any(|w| *w == *b"/Type /Catalog"));
    assert!(bytes.windows(7).any(|w| *w == *b"trailer"));
}

#[test]
fn single_page_report_has_one_page_object() {
    let bytes = render_to_bytes(&sample_report(5));
    assert_eq!(count(&bytes, b"/Type /Page "), 1, "one page dict expected");
    assert_eq!(count(&bytes, b"/Type /Pages"), 1, "one pages tree expected");
    assert_eq!(count(&bytes, b"/Count 1"), 1, "pages tree counts exactly 1");
    // Title and a body line survive into the content stream (unescaped ASCII).
    assert!(bytes.windows(b"ROBS Event Log Report".len()).any(|w| *w == *b"ROBS Event Log Report"));
    assert!(bytes.windows(b"event number 4".len()).any(|w| *w == *b"event number 4"));
}

#[test]
fn many_lines_produce_multiple_pages_with_footers() {
    // 200 body lines at ~57 first-page rows + ~60 per continuation page
    // must span >= 3 pages, each with its own footer.
    let bytes = render_to_bytes(&sample_report(200));
    let page_dicts = count(&bytes, b"/Type /Page ");
    assert!(page_dicts >= 3, "expected >= 3 pages, got {page_dicts}");
    // Every page carries exactly one footer: "Page i of N) Tj".
    let footer_tail = format!("of {page_dicts}) Tj");
    assert_eq!(
        count(&bytes, footer_tail.as_bytes()),
        page_dicts,
        "every page carries a footer"
    );
    // The pages-tree /Count must equal the number of page objects.
    let count_marker = format!("/Count {page_dicts}");
    assert!(bytes.windows(count_marker.len()).any(|w| *w == *count_marker.as_bytes()));
}

// ===========================================================================
// Cross-reference table correctness
// ===========================================================================

#[test]
fn xref_offsets_point_exactly_at_their_objects() {
    let bytes = render_to_bytes(&sample_report(7));

    // Locate startxref.
    let sx = bytes
        .windows(b"startxref".len())
        .position(|w| *w == *b"startxref")
        .expect("startxref marker");
    let after = &bytes[sx + b"startxref".len()..];
    // Skip the newline after the keyword (MSRV-safe; no trim_ascii_*).
    let digits = after.strip_prefix(b"\n").unwrap_or(after);
    let num_end = digits
        .iter()
        .position(|b| !b.is_ascii_digit())
        .expect("digits after startxref");
    let xref_at: usize = std::str::from_utf8(&digits[..num_end])
        .expect("ascii")
        .parse()
        .expect("startxref value");

    // startxref must point at the xref keyword...
    assert_eq!(&bytes[xref_at..xref_at + 4], b"xref", "startxref must point at 'xref'");

    // ...and every offset entry must point exactly at "N 0 obj".
    // Simple parse: first line after "xref" is "0 N"; then N+1 entries follow,
    // each exactly 20 bytes: "%010d %05d %c\n" style with padding.
    let mut pos = xref_at + b"xref\n".len();
    let nl = bytes[pos..].iter().position(|b| *b == b'\n').unwrap();
    let first_line = std::str::from_utf8(&bytes[pos..pos + nl]).unwrap().to_string();
    let total: usize = first_line.split_whitespace().nth(1).unwrap().parse().unwrap();
    pos += nl + 1;

    // Entry 0 is the free head.
    let free = &bytes[pos..pos + 20];
    assert!(free.starts_with(b"0000000000 65535 f"), "entry 0 must be the free object");
    pos += 20;

    for id in 1..total {
        let entry = &bytes[pos..pos + 20];
        assert!(entry.ends_with(b"00000 n \n") || entry.ends_with(b"00000 n\n"), "entry {id} shape");
        let off: usize = std::str::from_utf8(&entry[..10])
            .expect("ascii offset")
            .parse()
            .expect("numeric offset");
        let expected = format!("{id} 0 obj");
        assert!(
            &bytes[off..off + expected.len()] == expected.as_bytes(),
            "xref entry {id} ({off}) must point at '{expected}'"
        );
        pos += 20;
    }

    // Trailer immediately follows the last entry.
    assert_eq!(&bytes[pos..pos + 7], b"trailer");
}

// ===========================================================================
// Text encoding
// ===========================================================================

#[test]
fn pdf_special_characters_are_escaped() {
    let mut r = Report::new("Escaping", "specials");
    r.push_line("path C:\\recordings (take 2) closed)");
    let bytes = render_to_bytes(&r);
    // Backslash and parens must be backslash-escaped inside the string.
    assert!(bytes.windows(2).any(|w| *w == *b"\\\\"));
    assert!(bytes.windows(2).any(|w| *w == *b"\\("));
    assert!(bytes.windows(2).any(|w| *w == *b"\\)"));
    // The payload token survives fully escaped (and exactly once). Escaping
    // prefixes `\` rather than removing the paren bytes, so a bare-substring
    // "must not appear" check would be meaningless here.
    assert_eq!(count(&bytes, b"\\(take 2\\)"), 1);
}

#[test]
fn non_winansi_characters_are_replaced_not_corrupted() {
    let mut r = Report::new("Unicode", "mapping");
    r.push_line("emoji \u{1F4A9} and CJK \u{4E2D}\u{6587} drop to '?'");
    let bytes = render_to_bytes(&r);
    // The replacement marker made it through...
    assert!(bytes.windows(b"drop to '?'".len()).any(|w| *w == *b"drop to '?'"));
    // ...and no raw multi-byte UTF-8 leaked into the string streams.
    assert!(!bytes.windows(2).any(|w| w[0] == 0xF0 && (w[1] & 0xF0) == 0x90), "no UTF-8 emoji bytes");
}

#[test]
fn winansi_punctuation_maps_to_cp1252_bytes() {
    let mut r = Report::new("Punct", "cp1252");
    r.push_line("dash \u{2014} quote \u{2019}");
    let bytes = render_to_bytes(&r);
    // Em dash -> 0x97, right single quote -> 0x92 (single bytes, not UTF-8).
    let needle: &[u8] = &[b'd', b'a', b's', b'h', b' ', 0x97];
    assert!(bytes.windows(needle.len()).any(|w| *w == *needle), "em dash must be CP1252 0x97");
    let q: &[u8] = &[b' ', 0x92];
    assert!(bytes.windows(q.len()).any(|w| *w == *q), "curly quote must be CP1252 0x92");
}

// ===========================================================================
// Wrapping & pagination behavior
// ===========================================================================

#[test]
fn long_lines_wrap_with_two_space_continuation_indent() {
    let long = "word ".repeat(60); // 300 chars, spaces every 5
    let mut r = Report::new("Wrap", "test");
    r.push_line(long.trim_end());
    let bytes = render_to_bytes(&r);
    // The continuation indent must appear in a text op: "(  word" pattern.
    let cont: &[u8] = b"(  word";
    assert!(
        bytes.windows(cont.len()).any(|w| *w == *cont),
        "wrapped continuation must start with a two-space indent"
    );
}

#[test]
fn empty_report_still_renders_one_header_page() {
    let bytes = render_to_bytes(&Report::new("Only header", "no body"));
    assert_eq!(count(&bytes, b"/Type /Page "), 1);
    assert_eq!(count(&bytes, b"Page 1 of 1"), 1);
    assert!(bytes.windows(b"Only header".len()).any(|w| *w == *b"Only header"));
}

#[test]
fn write_pdf_reports_missing_directory_as_error() {
    // A path whose parent directories do not exist must surface an error, not
    // panic and not silently create a tree.
    let dir = TestDir::new();
    let bad = dir.path().join("no/such/subdir/report.pdf");
    let err = write_pdf(&sample_report(1), &bad);
    assert!(err.is_err(), "writing into a nonexistent directory must fail");
    assert!(!bad.exists());
}
