use anyhow::Result;
use serde::Serialize;
use serde_json::Value as Json;

#[derive(Debug, Serialize)]
pub struct Hit {
    pub agent: String,
    pub session_id: String,
    pub path: String,
    pub line_no: u64,
    pub ts: Option<String>,
    pub role: String,
    pub model: Option<String>,
    #[serde(flatten)]
    pub body: Body,
    #[serde(rename = "ref")]
    pub reference: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub cut: Vec<Cut>,
    #[serde(skip)]
    pub byte_off: u64,
    #[serde(skip)]
    pub byte_len: u64,
    #[serde(skip)]
    pub event_id: i64,
    #[serde(skip)]
    pub position: Position,
}

/// Where a grep or show page stopped: the file ID, line, and ordinal of the
/// last event it examined, or `i64::MAX` line and ordinal for a file read to
/// its end. Files are read in ID order and each file in line order, so a later
/// page resumes after this position.
pub type Position = (i64, i64, i64);

/// What a hit carries of its event: the indexed text for ranked lookup, the
/// whole source record, parsed and redacted, for grep and show, or part of
/// that record's JSON text when it does not fit a page alone.
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum Body {
    Text { text: String, truncated: bool },
    Record { record: Json },
    Part { part: String },
    Unavailable { unavailable: String },
}

impl Hit {
    /// Replaces a record that does not fit a page alone with the part of its
    /// JSON text from `offset` that does, and returns where the next part
    /// starts while any is left.
    pub(super) fn fit(&mut self, kind: &str, offset: usize) -> Result<Option<usize>> {
        let Body::Record { record } = &self.body else {
            return Ok(None);
        };
        let text = record.to_string();
        anyhow::ensure!(text.is_char_boundary(offset), CURSOR_MISMATCH);
        self.body = Body::Part { part: text };
        let line = line_len(self, kind);
        if let Body::Part { part } = &mut self.body {
            self.cut = fit(line, vec![("/part".to_owned(), part, offset)]);
        }
        Ok(self
            .cut
            .iter()
            .find(|c| c.offset + c.kept < c.bytes)
            .map(|c| c.offset + c.kept))
    }
}

#[derive(Debug, Default, Serialize)]
pub struct Coverage {
    pub incomplete: bool,
    pub continuation: Option<Position>,
    /// Where `show` resumes in the JSON text of the record at `continuation`,
    /// which the next page shows again from there.
    pub within: Option<usize>,
    pub unavailable_ranges: usize,
    pub records: usize,
    pub bytes: u64,
}

/// Data lines per page, in bytes. Rule: docs/formats.md `display_page`.
pub(super) const PAGE_BYTES: usize = 24 * 1024;

/// A string shortened to fit a page: `field` is its JSON Pointer in the data
/// record, `bytes` the length of the whole string, and the page holds `kept`
/// bytes of it starting at `offset`.
#[derive(Debug, Serialize)]
pub struct Cut {
    pub field: String,
    pub bytes: usize,
    pub offset: usize,
    pub kept: usize,
}

/// Serializing fails only for a map with non-string keys, which no output has.
fn json_len(value: &impl Serialize) -> usize {
    serde_json::to_vec(value)
        .expect("output records serialize")
        .len()
}

pub(super) fn line_len(record: &impl Serialize, kind: &str) -> usize {
    json_len(record) + r#","type":"""#.len() + kind.len() + 1
}

pub const CURSOR_MISMATCH: &str =
    "cursor belongs to another query or index version; restart without --cursor";

/// Shortens strings, each from its offset, to the largest common length that
/// fits a data line of `line` bytes into [`PAGE_BYTES`], so short ones stay
/// whole, and lists each string that was shortened or starts past its
/// beginning.
pub(super) fn fit(line: usize, slots: Vec<(String, &mut String, usize)>) -> Vec<Cut> {
    let whole: usize = slots.iter().map(|(_, text, _)| json_len(text)).sum();
    let layout = |limit: usize| {
        let mut size = line - whole;
        let mut kept = Vec::new();
        let mut cuts = Vec::new();
        for (field, text, offset) in &slots {
            let rest = &text[*offset..];
            let k = rest.floor_char_boundary(limit);
            size += json_len(&&rest[..k]);
            if k < rest.len() || *offset > 0 {
                cuts.push(Cut {
                    field: field.clone(),
                    bytes: text.len(),
                    offset: *offset,
                    kept: k,
                });
            }
            kept.push(k);
        }
        if !cuts.is_empty() {
            size += r#","cut":"#.len() + json_len(&cuts);
        }
        (size, kept, cuts)
    };
    // Four bytes hold any character, so a continuing record always advances.
    let limits: Vec<usize> = (4..=PAGE_BYTES).collect();
    let fitting = limits.partition_point(|&limit| layout(limit).0 <= PAGE_BYTES);
    let (_, kept, cuts) = layout(limits[fitting.saturating_sub(1)]);
    for ((_, text, offset), k) in slots.into_iter().zip(kept) {
        *text = text[offset..offset + k].to_owned();
    }
    cuts
}

/// A hit's columns, from `event_rows e` and `sessions s`.
pub(super) const COLUMNS: &str = "s.agent,s.session_id,s.path,e.line_no,e.ts,e.role,e.model,e.byte_off,e.byte_len,e.id,e.file_id,e.ordinal";

pub(super) fn hit(row: &rusqlite::Row<'_>) -> rusqlite::Result<Hit> {
    let agent: String = row.get(0)?;
    let session_id: String = row.get(1)?;
    let path: String = row.get(2)?;
    let line_no: u64 = row.get(3)?;
    let reference = format!("{path}:{line_no}");
    Ok(Hit {
        agent,
        session_id,
        path,
        line_no,
        ts: row.get(4)?,
        role: row.get(5)?,
        model: row.get(6)?,
        body: Body::Text {
            text: String::new(),
            truncated: false,
        },
        reference,
        cut: Vec::new(),
        byte_off: row.get(7)?,
        byte_len: row.get(8)?,
        event_id: row.get(9)?,
        position: (row.get(10)?, row.get(3)?, row.get(11)?),
    })
}
