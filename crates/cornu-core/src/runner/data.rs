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
}

/// Several lists are read side by side: row `i` takes the `i`-th value of each.
pub fn rows_from_lists(lists: &[ValueList]) -> Result<Vec<Row>, DataError> {
    let mut columns: Vec<(&str, Vec<&str>)> = Vec::new();
    for list in lists {
        let name = list.name();
        if name.is_empty() {
            return Err(DataError::MissingVariable);
        }
        if name.contains(['{', '}', '\n']) {
            return Err(DataError::InvalidVariable(name.to_owned()));
        }
        if name.starts_with('$') {
            return Err(DataError::ReservedVariable(name.to_owned()));
        }
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
}
