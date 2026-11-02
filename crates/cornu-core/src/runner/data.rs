//! Where the rows of a run come from.

use serde::{Deserialize, Serialize};

use super::Row;

/// A variable bound to pasted values, one per line: each value is one iteration.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValueList {
    pub variable: String,
    /// Kept as pasted, so the text the user edits round-trips untouched.
    pub values: String,
}

impl ValueList {
    pub fn new(variable: impl Into<String>, values: impl Into<String>) -> Self {
        Self {
            variable: variable.into(),
            values: values.into(),
        }
    }

    /// Lines are trimmed and blank ones skipped: lists pasted from a spreadsheet or a
    /// terminal often carry trailing spaces and empty lines.
    pub fn values(&self) -> Vec<&str> {
        self.values
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect()
    }

    /// `{{ id }}` and ` id ` both bind `id`.
    pub fn name(&self) -> &str {
        let name = self.variable.trim();
        name.strip_prefix("{{")
            .and_then(|inner| inner.strip_suffix("}}"))
            .map_or(name, str::trim)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DataError {
    #[error("a list has no variable name")]
    MissingVariable,
    #[error("{0:?} cannot be used as a variable name")]
    InvalidVariable(String),
    #[error("{{{{{0}}}}} is reserved for generated values")]
    ReservedVariable(String),
    #[error("{{{{{0}}}}} is bound to more than one list")]
    DuplicateVariable(String),
    #[error("the list for {{{{{0}}}}} has no values")]
    EmptyList(String),
    #[error("{{{{{first}}}}} has {expected} values but {{{{{variable}}}}} has {found}")]
    LengthMismatch {
        first: String,
        expected: usize,
        variable: String,
        found: usize,
    },
    #[error("invalid CSV: {0}")]
    Csv(String),
    #[error("the CSV has no data rows")]
    NoRows,
    #[error("column {{{{{0}}}}} appears more than once in the header")]
    DuplicateColumn(String),
    #[error("line {line} has {found} values but the header names {expected} columns")]
    TooManyValues {
        line: u64,
        found: usize,
        expected: usize,
    },
}

fn check_name(name: &str) -> Result<(), DataError> {
    if name.contains(['{', '}', '\n']) {
        return Err(DataError::InvalidVariable(name.to_owned()));
    }
    if name.starts_with('$') {
        return Err(DataError::ReservedVariable(name.to_owned()));
    }
    Ok(())
}

/// Several lists are read side by side: row `i` takes the `i`-th value of each.
pub fn rows_from_lists(lists: &[ValueList]) -> Result<Vec<Row>, DataError> {
    let mut columns: Vec<(&str, Vec<&str>)> = Vec::new();
    for list in lists {
        let name = list.name();
        if name.is_empty() {
            return Err(DataError::MissingVariable);
        }
        check_name(name)?;
        if columns.iter().any(|(existing, _)| *existing == name) {
            return Err(DataError::DuplicateVariable(name.to_owned()));
        }
        let values = list.values();
        if values.is_empty() {
            return Err(DataError::EmptyList(name.to_owned()));
        }
        if let Some((first, expected)) = columns.first().map(|(n, v)| (*n, v.len()))
            && values.len() != expected
        {
            return Err(DataError::LengthMismatch {
                first: first.to_owned(),
                expected,
                variable: name.to_owned(),
                found: values.len(),
            });
        }
        columns.push((name, values));
    }

    let count = columns.first().map_or(0, |(_, values)| values.len());
    Ok((0..count)
        .map(|index| {
            columns
                .iter()
                .map(|(name, values)| (name.to_string(), values[index].to_owned()))
                .collect()
        })
        .collect())
}

/// Spreadsheets copy cells as tab-separated text, so tabs win ties.
const DELIMITERS: [char; 4] = ['\t', ',', ';', '|'];
const SNIFFED_RECORDS: usize = 20;

/// CSV text, pasted or read from a file: one iteration per data row, one variable per column.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CsvData {
    pub text: String,
    /// Without a header the columns are `{{col1}}`, `{{col2}}`…
    #[serde(default)]
    pub header: bool,
    /// Detected from the text when not set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delimiter: Option<char>,
}

/// Columns and records of a CSV, as the runner and its preview see them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Table {
    pub columns: Vec<String>,
    /// Padded with empty values to the number of columns.
    pub records: Vec<Vec<String>>,
}

impl CsvData {
    pub fn new(text: impl Into<String>, header: bool) -> Self {
        Self {
            text: text.into(),
            header,
            delimiter: None,
        }
    }

    fn content(&self) -> &str {
        self.text.trim_start_matches('\u{feff}')
    }

    pub fn delimiter(&self) -> char {
        self.delimiter
            .unwrap_or_else(|| detect_delimiter(self.content()))
    }

    pub fn table(&self) -> Result<Table, DataError> {
        let mut records = read_records(self.content(), self.delimiter(), usize::MAX)?;
        let columns = if self.header && !records.is_empty() {
            let header = records.remove(0).1;
            let mut columns: Vec<String> = Vec::new();
            for (index, name) in header.iter().enumerate() {
                let name = match name.trim() {
                    "" => format!("col{}", index + 1),
                    name => name.to_owned(),
                };
                check_name(&name)?;
                if columns.contains(&name) {
                    return Err(DataError::DuplicateColumn(name));
                }
                columns.push(name);
            }
            if let Some((line, record)) = records.iter().find(|(_, r)| r.len() > columns.len()) {
                return Err(DataError::TooManyValues {
                    line: *line,
                    found: record.len(),
                    expected: columns.len(),
                });
            }
            columns
        } else {
            let width = records.iter().map(|(_, r)| r.len()).max().unwrap_or(0);
            (1..=width).map(|index| format!("col{index}")).collect()
        };
        let records = records
            .into_iter()
            .map(|(_, mut record)| {
                record.resize(columns.len(), String::new());
                record
            })
            .collect();
        Ok(Table { columns, records })
    }

    pub fn rows(&self) -> Result<Vec<Row>, DataError> {
        let table = self.table()?;
        if table.records.is_empty() {
            return Err(DataError::NoRows);
        }
        Ok(table
            .records
            .into_iter()
            .map(|record| table.columns.iter().cloned().zip(record).collect())
            .collect())
    }
}

/// `(line, values)` for each non-blank record.
fn read_records(
    text: &str,
    delimiter: char,
    limit: usize,
) -> Result<Vec<(u64, Vec<String>)>, DataError> {
    let mut delimiter_byte = [0; 4];
    delimiter.encode_utf8(&mut delimiter_byte);
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .delimiter(delimiter_byte[0])
        .from_reader(text.as_bytes());
    let mut records = Vec::new();
    for record in reader.records().take(limit) {
        let record = record.map_err(|error| DataError::Csv(error.to_string()))?;
        if record.iter().all(|value| value.trim().is_empty()) {
            continue;
        }
        let line = record.position().map_or(0, |position| position.line());
        records.push((line, record.iter().map(str::to_owned).collect()));
    }
    Ok(records)
}

/// The delimiter splitting the first records into the same number of columns, the most
/// columns winning; a comma when nothing splits.
pub fn detect_delimiter(text: &str) -> char {
    let mut best = (',', 1, false);
    for delimiter in DELIMITERS {
        let Ok(records) = read_records(text, delimiter, SNIFFED_RECORDS) else {
            continue;
        };
        let Some(width) = records.first().map(|(_, r)| r.len()) else {
            continue;
        };
        let consistent = records.iter().all(|(_, r)| r.len() == width);
        let better = (consistent, width) > (best.2, best.1);
        if width > 1 && better {
            best = (delimiter, width, consistent);
        }
    }
    best.0
}

/// A first row of distinct, non-numeric names reads as a header; a good default to offer.
pub fn guess_header(text: &str, delimiter: char) -> bool {
    let Ok(records) = read_records(text.trim_start_matches('\u{feff}'), delimiter, 2) else {
        return false;
    };
    let Some((_, first)) = records.first() else {
        return false;
    };
    let names: Vec<&str> = first.iter().map(|name| name.trim()).collect();
    records.len() > 1
        && names.iter().enumerate().all(|(index, name)| {
            !name.is_empty()
                && name.parse::<f64>().is_err()
                && check_name(name).is_ok()
                && !names[..index].contains(name)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(rows: &[Row]) -> Vec<Vec<(&str, &str)>> {
        rows.iter()
            .map(|row| row.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect())
            .collect()
    }

    #[test]
    fn one_iteration_per_non_blank_line() {
        let list = ValueList::new(" {{ id }} ", "42\r\n  7  \n\n\t\nabc def\n");
        assert_eq!(list.name(), "id");
        assert_eq!(list.values(), ["42", "7", "abc def"]);
        let rows = rows_from_lists(&[list]).unwrap();
        assert_eq!(
            pairs(&rows),
            [[("id", "42")], [("id", "7")], [("id", "abc def")]]
        );
    }

    #[test]
    fn lists_are_zipped() {
        let rows = rows_from_lists(&[
            ValueList::new("id", "1\n2"),
            ValueList::new("name", "ada\ngrace\n"),
        ])
        .unwrap();
        assert_eq!(
            pairs(&rows),
            [
                [("id", "1"), ("name", "ada")],
                [("id", "2"), ("name", "grace")]
            ]
        );
        assert_eq!(rows_from_lists(&[]).unwrap(), Vec::<Row>::new());
    }

    #[test]
    fn bad_bindings_are_explained() {
        let cases = [
            (
                vec![ValueList::new("  ", "1")],
                "a list has no variable name",
            ),
            (vec![ValueList::new("a}b", "1")], "\"a}b\" cannot be used"),
            (vec![ValueList::new("$uuid", "1")], "{{$uuid}} is reserved"),
            (
                vec![ValueList::new("id", "1"), ValueList::new("{{id}}", "2")],
                "{{id}} is bound to more than one list",
            ),
            (vec![ValueList::new("id", "\n \n")], "{{id}} has no values"),
            (
                vec![ValueList::new("id", "1\n2\n3"), ValueList::new("name", "a")],
                "{{id}} has 3 values but {{name}} has 1",
            ),
        ];
        for (lists, message) in cases {
            let error = rows_from_lists(&lists).unwrap_err().to_string();
            assert!(error.contains(message), "{error}");
        }
    }

    #[test]
    fn lists_round_trip_through_toml() {
        let list = ValueList::new("id", "1\n2\n");
        let text = toml::to_string(&list).unwrap();
        assert_eq!(toml::from_str::<ValueList>(&text).unwrap(), list);
    }

    fn table(csv: &CsvData) -> (Vec<String>, Vec<Vec<String>>) {
        let table = csv.table().unwrap();
        (table.columns, table.records)
    }

    #[test]
    fn csv_with_header_names_the_variables() {
        let csv = CsvData::new(
            "\u{feff}id, name ,email\n1,\"Ada, Countess\",ada@x\n\n2,grace,\n",
            true,
        );
        assert_eq!(csv.delimiter(), ',');
        let (columns, records) = table(&csv);
        assert_eq!(columns, ["id", "name", "email"]);
        assert_eq!(
            records,
            [["1", "Ada, Countess", "ada@x"], ["2", "grace", ""]]
        );
        assert_eq!(
            pairs(&csv.rows().unwrap())[0],
            [("id", "1"), ("name", "Ada, Countess"), ("email", "ada@x")]
        );
    }

    #[test]
    fn csv_without_header_numbers_the_columns() {
        let csv = CsvData::new("a;1\nb;2;extra\nc\n", false);
        assert_eq!(csv.delimiter(), ';');
        let (columns, records) = table(&csv);
        assert_eq!(columns, ["col1", "col2", "col3"]);
        assert_eq!(records[0], ["a", "1", ""]);
        assert_eq!(records[2], ["c", "", ""]);
        assert_eq!(pairs(&csv.rows().unwrap())[1][2], ("col3", "extra"));
    }

    #[test]
    fn delimiters_are_detected() {
        assert_eq!(detect_delimiter("id\tname\n1\tAda, Countess\n"), '\t');
        assert_eq!(detect_delimiter("a,b;c\nd,e;f\n"), ',');
        assert_eq!(detect_delimiter("a;b;c\n1;2,5;3\n"), ';');
        assert_eq!(detect_delimiter("a|b\n1|2\n"), '|');
        assert_eq!(detect_delimiter("single\ncolumn\n"), ',');
        assert_eq!(detect_delimiter(""), ',');
        let mut csv = CsvData::new("a;b\n1;2", false);
        csv.delimiter = Some(',');
        assert_eq!(table(&csv).0, ["col1"]);
    }

    #[test]
    fn headers_are_guessed() {
        assert!(guess_header("id,name\n1,ada\n", ','));
        assert!(guess_header("\u{feff}id\tname\n1\tada\n", '\t'));
        assert!(!guess_header("1,ada\n2,grace\n", ','));
        assert!(!guess_header("id,id\n1,2\n", ','));
        assert!(!guess_header("id,\n1,2\n", ','));
        assert!(!guess_header("id,name\n", ','));
    }

    #[test]
    fn csv_problems_are_explained() {
        let cases = [
            (CsvData::new("", false), "no data rows"),
            (CsvData::new("id,name\n", true), "no data rows"),
            (
                CsvData::new("id,id\n1,2\n", true),
                "{{id}} appears more than once",
            ),
            (
                CsvData::new("id\n1\n2,3\n", true),
                "line 3 has 2 values but the header names 1",
            ),
            (CsvData::new("$uuid\n1\n", true), "reserved"),
        ];
        for (csv, message) in cases {
            let error = csv.rows().unwrap_err().to_string();
            assert!(error.contains(message), "{error}");
        }
        let (columns, _) = table(&CsvData::new("id,,name\n1,2,3\n", true));
        assert_eq!(columns, ["id", "col2", "name"]);
    }

    #[test]
    fn csv_round_trips_through_toml() {
        let mut csv = CsvData::new("a\tb\n1\t2\n", true);
        assert_eq!(
            toml::from_str::<CsvData>(&toml::to_string(&csv).unwrap()).unwrap(),
            csv
        );
        csv.delimiter = Some('\t');
        assert_eq!(
            toml::from_str::<CsvData>(&toml::to_string(&csv).unwrap()).unwrap(),
            csv
        );
    }
}
