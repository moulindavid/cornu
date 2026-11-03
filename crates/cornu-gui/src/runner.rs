use std::collections::HashSet;
use std::path::PathBuf;

use cornu_core::runner::data::{CsvData, DataError, ValueList, guess_header, rows_from_lists};
use cornu_core::runner::{Row, RunOptions, RunSummary};
use cornu_core::{AssertionResult, Captured, Method, Response};
use cornu_storage::{Folder, Item, RequestEntry};
use eframe::egui;

use crate::format;
use crate::response::{ResponseView, status_color};
use crate::tree::method_color;
use crate::widgets::{assertion_lines, captured_lines, show_in, split_rects, tests_label};
use crate::worker::{DocId, RunEvent, RunHandle};

const PREVIEW_ROWS: usize = 8;
const DELIMITERS: [(char, &str); 4] = [
    ('\t', "Tab"),
    (',', "Comma"),
    (';', "Semicolon"),
    ('|', "Pipe"),
];

/// What a runner tab runs; the folder or request path is the document path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunTarget {
    Folder,
    Request,
    /// An open request tab, unsaved edits included.
    Tab(DocId),
}

pub struct StepInfo {
    pub path: Option<PathBuf>,
    pub name: String,
    pub method: Method,
}

/// Requests of a folder and its subfolders, in tree order.
pub fn folder_requests(folder: &Folder) -> Vec<&RequestEntry> {
    let mut requests = Vec::new();
    for item in &folder.items {
        match item {
            Item::Request(entry) => requests.push(entry),
            Item::Folder(child) => requests.extend(folder_requests(child)),
        }
    }
    requests
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum DataKind {
    #[default]
    None,
    Lists,
    Csv,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Filter {
    #[default]
    All,
    Success,
    Failed,
    Errors,
}

impl Filter {
    const ALL: [Filter; 4] = [Filter::All, Filter::Success, Filter::Failed, Filter::Errors];

    fn label(self) -> &'static str {
        match self {
            Filter::All => "All",
            Filter::Success => "Passed",
            Filter::Failed => "Failed",
            Filter::Errors => "Errors",
        }
    }

    fn accepts(self, row: &RunRow) -> bool {
        match (self, &row.outcome) {
            (Filter::All, _) => true,
            (Filter::Success, Ok(_)) => row.passed,
            (Filter::Failed, Ok(_)) => !row.passed,
            (Filter::Errors, Err(_)) => true,
            _ => false,
        }
    }
}

#[derive(Debug)]
pub struct RunRow {
    pub iteration: usize,
    pub step: usize,
    pub name: String,
    pub method: Method,
    pub url: String,
    pub unresolved: Vec<String>,
    pub outcome: Result<Response, String>,
    pub captured: Vec<Captured>,
    pub assertions: Vec<AssertionResult>,
    /// 2xx, or every assertion passed when the request has some.
    pub passed: bool,
}

struct Running {
    handle: RunHandle,
    names: Vec<String>,
}

impl Drop for Running {
    fn drop(&mut self) {
        self.handle.cancel.cancel();
    }
}

pub struct RunnerTab {
    pub target: RunTarget,
    pub excluded: HashSet<PathBuf>,
    data: DataKind,
    lists: Vec<ValueList>,
    csv: CsvData,
    /// Once the user ticks the header box, we stop guessing it from the text.
    header_touched: bool,
    concurrency: usize,
    delay_ms: u64,
    split: f32,
    details_split: f32,
    running: Option<Running>,
    rows: Vec<RunRow>,
    total: usize,
    summary: Option<RunSummary>,
    filter: Filter,
    selected: Option<usize>,
    view: Option<ResponseView>,
    notice: Option<String>,
}

pub enum RunnerAction {
    Start,
}

impl RunnerTab {
    pub fn new(target: RunTarget) -> Self {
        Self {
            target,
            excluded: HashSet::new(),
            data: DataKind::None,
            lists: vec![ValueList::default()],
            csv: CsvData::default(),
            header_touched: false,
            concurrency: 1,
            delay_ms: 0,
            split: 0.45,
            details_split: 0.5,
            running: None,
            rows: Vec::new(),
            total: 0,
            summary: None,
            filter: Filter::All,
            selected: None,
            view: None,
            notice: None,
        }
    }

    pub fn is_running(&self) -> bool {
        self.running.is_some()
    }

    pub fn data_rows(&self) -> Result<Vec<Row>, DataError> {
        match self.data {
            DataKind::None => Ok(Vec::new()),
            DataKind::Lists => rows_from_lists(&self.lists),
            DataKind::Csv => self.csv.rows(),
        }
    }

    pub fn options(&self) -> RunOptions {
        RunOptions {
            concurrency: self.concurrency,
            delay: std::time::Duration::from_millis(self.delay_ms),
        }
    }

    pub fn begin(&mut self, handle: RunHandle, names: Vec<String>, total: usize) {
        self.running = Some(Running { handle, names });
        self.rows.clear();
        self.total = total;
        self.summary = None;
        self.selected = None;
        self.view = None;
        self.notice = None;
    }

    pub fn poll(&mut self) {
        let Some(running) = &self.running else {
            return;
        };
        let mut finished = None;
        for event in running.handle.events.try_iter() {
            match event {
                RunEvent::Result(result) => {
                    let passed = result.passed();
                    let row = RunRow {
                        iteration: result.iteration,
                        step: result.step,
                        name: running.names.get(result.step).cloned().unwrap_or_default(),
                        method: result.request.method.clone(),
                        url: result.request.url.clone(),
                        unresolved: result.unresolved,
                        outcome: result.outcome.map_err(|error| error.to_string()),
                        captured: result.captured,
                        assertions: result.assertions,
                        passed,
                    };
                    let key = (row.iteration, row.step);
                    let index = self
                        .rows
                        .partition_point(|other| (other.iteration, other.step) < key);
                    self.rows.insert(index, row);
                    if let Some(selected) = &mut self.selected
                        && *selected >= index
                    {
                        *selected += 1;
                    }
                }
                RunEvent::Finished(summary) => finished = Some(summary),
            }
        }
        if let Some(summary) = finished {
            self.summary = Some(summary);
            self.running = None;
        }
    }

    fn cancel(&mut self) {
        if let Some(running) = &self.running {
            running.handle.cancel.cancel();
        }
    }

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        id: egui::Id,
        steps: &[StepInfo],
        environment: Option<&str>,
    ) -> Option<RunnerAction> {
        let mut action = None;
        let enabled = steps
            .iter()
            .filter(|step| {
                !step
                    .path
                    .as_ref()
                    .is_some_and(|p| self.excluded.contains(p))
            })
            .count();
        let data = self.data_rows();
        ui.horizontal(|ui| {
            if self.is_running() {
                if ui.button("■ Cancel").clicked() {
                    self.cancel();
                }
                let done = self.rows.len();
                let progress = done as f32 / self.total.max(1) as f32;
                ui.add(
                    egui::ProgressBar::new(progress)
                        .text(format!("{done} / {}", self.total))
                        .desired_width(220.0),
                );
            } else {
                let ready = enabled > 0 && data.is_ok();
                let iterations = data.as_ref().map_or(0, |rows| rows.len().max(1));
                let run = ui.add_enabled(ready, egui::Button::new("▶ Run"));
                if run.clicked() {
                    action = Some(RunnerAction::Start);
                }
                if ready {
                    ui.weak(plural(enabled * iterations, "request"));
                }
            }
            ui.separator();
            ui.weak(match environment {
                Some(name) => format!("Environment: {name}"),
                None => "No environment".to_owned(),
            });
        });
        ui.separator();

        let [top, bottom] = split_rects(ui, id.with("split"), &mut self.split);
        show_in(ui, top, |ui| {
            egui::ScrollArea::vertical()
                .id_salt(id.with("config"))
                .show(ui, |ui| self.config(ui, id, steps, &data));
        });
        show_in(ui, bottom, |ui| self.results(ui, id));
        action
    }

    fn config(
        &mut self,
        ui: &mut egui::Ui,
        id: egui::Id,
        steps: &[StepInfo],
        data: &Result<Vec<Row>, DataError>,
    ) {
        ui.columns(2, |columns| {
            let ui = &mut columns[0];
            ui.strong("Requests");
            if steps.is_empty() {
                ui.weak("Nothing to run: the folder is empty or no longer exists.");
            }
            for step in steps {
                ui.horizontal(|ui| {
                    if let Some(path) = &step.path
                        && steps.len() > 1
                    {
                        let mut checked = !self.excluded.contains(path);
                        if ui.checkbox(&mut checked, "").changed() {
                            if checked {
                                self.excluded.remove(path);
                            } else {
                                self.excluded.insert(path.clone());
                            }
                        }
                    }
                    ui.colored_label(
                        method_color(&step.method, ui.visuals()),
                        egui::RichText::new(step.method.as_str())
                            .monospace()
                            .small(),
                    );
                    ui.label(&step.name);
                });
            }
            ui.add_space(8.0);
            ui.strong("Options");
            egui::Grid::new(id.with("options")).show(ui, |ui| {
                ui.label("Concurrent iterations");
                ui.add(egui::DragValue::new(&mut self.concurrency).range(1..=64));
                ui.end_row();
                ui.label("Delay after each request");
                ui.add(
                    egui::DragValue::new(&mut self.delay_ms)
                        .range(0..=60_000)
                        .suffix(" ms"),
                );
                ui.end_row();
            });

            let ui = &mut columns[1];
            ui.strong("Data");
            ui.horizontal(|ui| {
                ui.radio_value(&mut self.data, DataKind::None, "None (run once)");
                ui.radio_value(&mut self.data, DataKind::Lists, "Value lists");
                ui.radio_value(&mut self.data, DataKind::Csv, "CSV");
            });
            match self.data {
                DataKind::None => {
                    ui.weak("Each request is sent once with the current variables.");
                }
                DataKind::Lists => self.lists_editor(ui, id),
                DataKind::Csv => self.csv_editor(ui, id),
            }
            match data {
                Ok(rows) if self.data != DataKind::None => {
                    ui.weak(plural(rows.len(), "iteration"));
                }
                Ok(_) => {}
                Err(error) => {
                    ui.colored_label(ui.visuals().error_fg_color, error.to_string());
                }
            }
        });
    }

    fn lists_editor(&mut self, ui: &mut egui::Ui, id: egui::Id) {
        ui.weak("One value per line; several lists are read side by side.");
        let mut remove = None;
        for (index, list) in self.lists.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut list.variable)
                        .hint_text("variable")
                        .desired_width(120.0),
                );
                ui.add(
                    egui::TextEdit::multiline(&mut list.values)
                        .id_salt(id.with(("list", index)))
                        .hint_text("one value per line")
                        .code_editor()
                        .desired_rows(4)
                        .desired_width(ui.available_width() - 30.0),
                );
                if ui.small_button("🗑").on_hover_text("Remove").clicked() {
                    remove = Some(index);
                }
            });
        }
        if let Some(index) = remove {
            self.lists.remove(index);
        }
        if ui.button("+ Add list").clicked() {
            self.lists.push(ValueList::default());
        }
    }

    fn csv_editor(&mut self, ui: &mut egui::Ui, id: egui::Id) {
        ui.horizontal(|ui| {
            if ui.button("Load file…").clicked()
                && let Some(path) = rfd::FileDialog::new()
                    .add_filter("CSV", &["csv", "tsv", "txt"])
                    .pick_file()
            {
                match std::fs::read_to_string(&path) {
                    Ok(text) => self.set_csv_text(text),
                    Err(error) => self.notice = Some(format!("{}: {error}", path.display())),
                }
            }
            let mut header = self.csv.header;
            if ui.checkbox(&mut header, "First row is a header").changed() {
                self.csv.header = header;
                self.header_touched = true;
            }
            let detected = self.csv.delimiter();
            let selected = match self.csv.delimiter {
                None => format!("Auto ({})", delimiter_name(detected)),
                Some(delimiter) => delimiter_name(delimiter).to_owned(),
            };
            egui::ComboBox::from_id_salt(id.with("delimiter"))
                .selected_text(selected)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.csv.delimiter, None, "Auto");
                    for (delimiter, name) in DELIMITERS {
                        ui.selectable_value(&mut self.csv.delimiter, Some(delimiter), name);
                    }
                });
        });
        let mut text = self.csv.text.clone();
        let edit = ui.add(
            egui::TextEdit::multiline(&mut text)
                .id_salt(id.with("csv"))
                .hint_text("Paste CSV here, or cells copied from a spreadsheet")
                .code_editor()
                .desired_rows(5)
                .desired_width(f32::INFINITY),
        );
        if edit.changed() {
            self.set_csv_text(text);
        }
        if let Ok(table) = self.csv.table()
            && !table.columns.is_empty()
        {
            egui::ScrollArea::horizontal()
                .id_salt(id.with("preview"))
                .show(ui, |ui| {
                    egui::Grid::new(id.with("preview-grid"))
                        .striped(true)
                        .show(ui, |ui| {
                            for column in &table.columns {
                                ui.strong(format!("{{{{{column}}}}}"));
                            }
                            ui.end_row();
                            for record in table.records.iter().take(PREVIEW_ROWS) {
                                for value in record {
                                    ui.add(egui::Label::new(value).truncate());
                                }
                                ui.end_row();
                            }
                        });
                });
            if table.records.len() > PREVIEW_ROWS {
                ui.weak(format!("… and {} more", table.records.len() - PREVIEW_ROWS));
            }
        }
    }

    fn set_csv_text(&mut self, text: String) {
        self.csv.text = text;
        if !self.header_touched {
            self.csv.header = guess_header(&self.csv.text, self.csv.delimiter());
        }
    }

    fn results(&mut self, ui: &mut egui::Ui, id: egui::Id) {
        ui.add_space(4.0);
        if self.rows.is_empty() && self.summary.is_none() {
            ui.weak("Results appear here as the requests complete.");
            return;
        }
        ui.horizontal(|ui| {
            for filter in Filter::ALL {
                let count = self.rows.iter().filter(|row| filter.accepts(row)).count();
                let label = format!("{} ({count})", filter.label());
                if ui.selectable_label(self.filter == filter, label).clicked() {
                    self.filter = filter;
                }
            }
            ui.separator();
            if let Some(summary) = &self.summary {
                ui.weak(summary_text(summary));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_enabled_ui(!self.is_running(), |ui| {
                    if ui.button("Export JSON…").clicked() {
                        self.export(Format::Json);
                    }
                    if ui.button("Export CSV…").clicked() {
                        self.export(Format::Csv);
                    }
                });
            });
        });
        if let Some(notice) = &self.notice {
            ui.colored_label(ui.visuals().error_fg_color, notice);
        }

        if self.selected.is_some() {
            let [top, bottom] = split_rects(ui, id.with("details-split"), &mut self.details_split);
            show_in(ui, top, |ui| self.table(ui, id));
            show_in(ui, bottom, |ui| self.details(ui));
        } else {
            self.table(ui, id);
        }
    }

    fn table(&mut self, ui: &mut egui::Ui, id: egui::Id) {
        let visible: Vec<usize> = (0..self.rows.len())
            .filter(|&index| self.filter.accepts(&self.rows[index]))
            .collect();
        let height = ui.spacing().interact_size.y;
        let widths = [48.0, 180.0, 70.0, 64.0, 80.0, 80.0];
        ui.horizontal(|ui| {
            for (header, width) in ["#", "Request", "Status", "Tests", "Time", "Size"]
                .iter()
                .zip(widths)
            {
                cell(ui, width, height, egui::RichText::new(*header).strong());
            }
            ui.strong("URL");
        });
        let mut clicked = None;
        egui::ScrollArea::vertical()
            .id_salt(id.with("results"))
            .auto_shrink([false, false])
            .show_rows(ui, height, visible.len(), |ui, range| {
                for &index in &visible[range] {
                    let row = &self.rows[index];
                    let line = ui.horizontal(|ui| {
                        cell(ui, widths[0], height, (row.iteration + 1).to_string());
                        cell(ui, widths[1], height, row.name.as_str());
                        let (status, time, size) = match &row.outcome {
                            Ok(response) => (
                                egui::RichText::new(response.status.to_string())
                                    .color(status_color(response.status)),
                                format::duration(response.elapsed),
                                format::size(response.size()),
                            ),
                            Err(_) => (
                                egui::RichText::new("Error").color(ui.visuals().error_fg_color),
                                String::new(),
                                String::new(),
                            ),
                        };
                        cell(ui, widths[2], height, status);
                        cell(ui, widths[3], height, tests_label(ui, &row.assertions));
                        cell(ui, widths[4], height, time);
                        cell(ui, widths[5], height, size);
                        ui.add(egui::Label::new(format!("{} {}", row.method, row.url)).truncate());
                    });
                    let rect = line.response.rect.with_max_x(ui.max_rect().right());
                    let response = ui.interact(rect, id.with(("row", index)), egui::Sense::click());
                    if self.selected == Some(index) {
                        ui.painter().rect_filled(
                            rect,
                            2.0,
                            ui.visuals().selection.bg_fill.gamma_multiply(0.4),
                        );
                    } else if response.hovered() {
                        ui.painter().rect_filled(
                            rect,
                            2.0,
                            ui.visuals().widgets.hovered.bg_fill.gamma_multiply(0.3),
                        );
                    }
                    if response.clicked() {
                        clicked = Some(index);
                    }
                }
            });
        if let Some(index) = clicked {
            self.select(index);
        }
    }

    fn select(&mut self, index: usize) {
        if self.selected == Some(index) {
            self.selected = None;
            self.view = None;
            return;
        }
        self.selected = Some(index);
        self.view = match &self.rows[index].outcome {
            Ok(response) => Some(ResponseView::new(response.clone())),
            Err(_) => None,
        };
    }

    fn details(&mut self, ui: &mut egui::Ui) {
        let Some(row) = self.selected.and_then(|index| self.rows.get(index)) else {
            return;
        };
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.strong(format!("#{} {}", row.iteration + 1, row.name));
            ui.add(egui::Label::new(format!("{} {}", row.method, row.url)).truncate());
        });
        if !row.unresolved.is_empty() {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                format!(
                    "⚠ Sent with unresolved variables: {}",
                    row.unresolved.join(", ")
                ),
            );
        }
        captured_lines(ui, &row.captured);
        assertion_lines(ui, &row.assertions);
        match (&row.outcome, &mut self.view) {
            (Err(error), _) => {
                ui.colored_label(ui.visuals().error_fg_color, error);
            }
            (Ok(_), Some(view)) => view.show(ui),
            (Ok(_), None) => {}
        }
    }

    fn visible_rows(&self) -> Vec<&RunRow> {
        self.rows
            .iter()
            .filter(|row| self.filter.accepts(row))
            .collect()
    }

    fn export(&mut self, format: Format) {
        let (name, filter, extension) = match format {
            Format::Csv => ("run-results.csv", "CSV", "csv"),
            Format::Json => ("run-results.json", "JSON", "json"),
        };
        let Some(path) = rfd::FileDialog::new()
            .set_file_name(name)
            .add_filter(filter, &[extension])
            .save_file()
        else {
            return;
        };
        let rows = self.visible_rows();
        let text = match format {
            Format::Csv => export_csv(&rows),
            Format::Json => Ok(export_json(&rows)),
        };
        self.notice = text
            .and_then(|text| std::fs::write(&path, text).map_err(|error| error.to_string()))
            .err()
            .map(|error| format!("Export failed: {error}"));
    }
}

enum Format {
    Csv,
    Json,
}

fn cell(ui: &mut egui::Ui, width: f32, height: f32, text: impl Into<egui::WidgetText>) {
    ui.allocate_ui_with_layout(
        egui::vec2(width, height),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.set_width(width);
            ui.add(egui::Label::new(text).truncate());
        },
    );
}

fn delimiter_name(delimiter: char) -> &'static str {
    DELIMITERS
        .iter()
        .find(|(candidate, _)| *candidate == delimiter)
        .map_or("Other", |(_, name)| name)
}

fn plural(count: usize, word: &str) -> String {
    if count == 1 {
        format!("1 {word}")
    } else {
        format!("{count} {word}s")
    }
}

fn summary_text(summary: &RunSummary) -> String {
    let mut parts = vec![format!("{} passed", summary.succeeded)];
    if summary.failed > 0 {
        parts.push(format!("{} failed", summary.failed));
    }
    if summary.errors > 0 {
        parts.push(format!("{} errors", summary.errors));
    }
    if summary.skipped > 0 {
        parts.push(format!("{} skipped", summary.skipped));
    }
    let state = if summary.cancelled {
        "Cancelled"
    } else {
        "Done"
    };
    format!(
        "{state} in {}: {}",
        format::duration(summary.elapsed),
        parts.join(", ")
    )
}

fn row_fields(row: &RunRow) -> (Option<u16>, String, Option<u128>, Option<usize>) {
    match &row.outcome {
        Ok(response) => (
            Some(response.status),
            String::new(),
            Some(response.elapsed.as_millis()),
            Some(response.size()),
        ),
        Err(error) => (None, error.clone(), None, None),
    }
}

/// `passed/total`, empty without assertions.
fn tests_count(results: &[AssertionResult]) -> String {
    if results.is_empty() {
        return String::new();
    }
    let passed = results.iter().filter(|result| result.passed()).count();
    format!("{passed}/{}", results.len())
}

fn export_csv(rows: &[&RunRow]) -> Result<String, String> {
    let mut writer = csv::Writer::from_writer(Vec::new());
    let header = [
        "iteration",
        "request",
        "method",
        "url",
        "status",
        "tests",
        "error",
        "time_ms",
        "size",
    ];
    let mut write = |record: Vec<String>| writer.write_record(record).map_err(|e| e.to_string());
    write(header.map(str::to_owned).to_vec())?;
    for row in rows {
        let (status, error, time, size) = row_fields(row);
        write(vec![
            (row.iteration + 1).to_string(),
            row.name.clone(),
            row.method.to_string(),
            row.url.clone(),
            status.map(|s| s.to_string()).unwrap_or_default(),
            tests_count(&row.assertions),
            error,
            time.map(|t| t.to_string()).unwrap_or_default(),
            size.map(|s| s.to_string()).unwrap_or_default(),
        ])?;
    }
    let bytes = writer.into_inner().map_err(|error| error.to_string())?;
    String::from_utf8(bytes).map_err(|error| error.to_string())
}

/// Bodies are included as text, as the point of a data-driven run is often what came back.
fn export_json(rows: &[&RunRow]) -> String {
    let rows: Vec<serde_json::Value> = rows
        .iter()
        .map(|row| {
            let (status, error, time, size) = row_fields(row);
            let body = row
                .outcome
                .as_ref()
                .ok()
                .map(|response| response.text().into_owned());
            let captured: Vec<serde_json::Value> = row
                .captured
                .iter()
                .map(|captured| match &captured.value {
                    Ok(value) => serde_json::json!({"variable": captured.variable, "value": value}),
                    Err(error) => {
                        serde_json::json!({"variable": captured.variable, "error": error.to_string()})
                    }
                })
                .collect();
            serde_json::json!({
                "iteration": row.iteration + 1,
                "request": row.name,
                "method": row.method.as_str(),
                "url": row.url,
                "status": status,
                "error": (!error.is_empty()).then_some(error),
                "time_ms": time,
                "size": size,
                "unresolved": row.unresolved,
                "captured": captured,
                "passed": row.passed,
                "assertions": row.assertions.iter().map(|result| serde_json::json!({
                    "assertion": result.description,
                    "passed": result.passed(),
                    "actual": result.actual,
                    "error": result.error,
                })).collect::<Vec<_>>(),
                "body": body,
            })
        })
        .collect();
    serde_json::to_string_pretty(&rows).unwrap_or_default()
}

#[cfg(test)]
impl RunnerTab {
    pub fn use_lists(&mut self, lists: Vec<ValueList>) {
        self.data = DataKind::Lists;
        self.lists = lists;
    }

    pub fn rows(&self) -> &[RunRow] {
        &self.rows
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use cornu_core::runner::{CancelToken, StepResult};
    use cornu_core::{CaptureError, Request, SendError};
    use cornu_storage::Settings;

    use super::*;

    fn response(status: u16, body: &str) -> Response {
        Response {
            status,
            headers: Vec::new(),
            body: body.as_bytes().to_vec(),
            elapsed: Duration::from_millis(12),
        }
    }

    fn result(iteration: usize, step: usize, outcome: Result<Response, SendError>) -> RunEvent {
        RunEvent::Result(Box::new(StepResult {
            iteration,
            step,
            request: Request::new(Method::Get, format!("https://x.example/{iteration}/{step}")),
            unresolved: Vec::new(),
            captured: if (iteration, step) == (0, 0) {
                vec![
                    Captured {
                        variable: "token".into(),
                        value: Ok("abc".into()),
                    },
                    Captured {
                        variable: "id".into(),
                        value: Err(CaptureError::NotJson),
                    },
                ]
            } else {
                Vec::new()
            },
            assertions: match (&outcome, (iteration, step)) {
                (Ok(response), (1, 0)) => cornu_core::check(
                    &[
                        cornu_core::Assertion::status(404),
                        cornu_core::Assertion::body("", cornu_core::Operator::Contains, "gone"),
                    ],
                    response,
                ),
                _ => Vec::new(),
            },
            outcome,
        }))
    }

    fn finished_tab() -> RunnerTab {
        let mut tab = RunnerTab::new(RunTarget::Folder);
        let (tx, events) = std::sync::mpsc::channel();
        tab.begin(
            RunHandle {
                events,
                cancel: CancelToken::default(),
            },
            vec!["Get user".into(), "Audit".into()],
            4,
        );
        tx.send(result(1, 0, Ok(response(404, "missing")))).unwrap();
        tx.send(result(0, 1, Err(SendError::Timeout))).unwrap();
        tx.send(result(0, 0, Ok(response(200, "{\"id\":1}"))))
            .unwrap();
        tab.poll();
        assert!(tab.is_running());
        tx.send(result(1, 1, Ok(response(204, "")))).unwrap();
        tx.send(RunEvent::Finished(RunSummary {
            succeeded: 2,
            failed: 1,
            errors: 1,
            skipped: 0,
            cancelled: false,
            elapsed: Duration::from_millis(80),
        }))
        .unwrap();
        tab.poll();
        tab
    }

    fn render(tab: &mut RunnerTab, steps: &[StepInfo]) -> String {
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1200.0, 900.0),
            )),
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| {
            tab.show(ui, egui::Id::unique("runner"), steps, Some("dev"));
        });
        output.textures_delta.clear();
        format!("{:?}", output.shapes)
    }

    #[test]
    fn results_are_ordered_and_filtered() {
        let mut tab = finished_tab();
        assert!(!tab.is_running());
        let order: Vec<_> = tab.rows.iter().map(|r| (r.iteration, r.step)).collect();
        assert_eq!(order, [(0, 0), (0, 1), (1, 0), (1, 1)]);
        assert_eq!(tab.rows[2].name, "Get user");
        let count = |filter: Filter| tab.rows.iter().filter(|r| filter.accepts(r)).count();
        assert_eq!(
            Filter::ALL.map(count),
            [4, 2, 1, 1],
            "all, passed, failed, errors"
        );
        assert_eq!(tab.rows[2].assertions.len(), 2);
        assert!(!tab.rows[2].passed, "a failed test fails the 404");

        let shapes = render(&mut tab, &[]);
        for expected in [
            "Passed (2)",
            "Failed (1)",
            "Errors (1)",
            "✘ 1/2",
            "Done in 80 ms",
            "404",
            "Audit",
            "Environment: dev",
        ] {
            assert!(shapes.contains(expected), "{expected} in {shapes}");
        }
        tab.filter = Filter::Errors;
        assert_eq!(tab.visible_rows().len(), 1);
    }

    #[test]
    fn selecting_a_row_shows_its_response() {
        let mut tab = finished_tab();
        tab.select(0);
        let shapes = render(&mut tab, &[]);
        assert!(shapes.contains("Captured token = abc"), "{shapes}");
        assert!(shapes.contains("Could not capture id"), "{shapes}");
        tab.select(2);
        let shapes = render(&mut tab, &[]);
        assert!(shapes.contains("missing"), "{shapes}");
        for expected in [
            "Tests: 1/2 passed",
            "✔ status equals 404",
            "✘ body contains gone: got missing",
        ] {
            assert!(shapes.contains(expected), "{expected} in {shapes}");
        }
        tab.select(1);
        assert!(render(&mut tab, &[]).contains("request timed out"));
        tab.select(1);
        assert_eq!(tab.selected, None);
    }

    #[test]
    fn exports_contain_every_visible_row() {
        let tab = finished_tab();
        let rows = tab.visible_rows();
        let csv = export_csv(&rows).unwrap();
        let lines: Vec<_> = csv.lines().collect();
        assert_eq!(
            lines[0],
            "iteration,request,method,url,status,tests,error,time_ms,size"
        );
        assert_eq!(lines[1], "1,Get user,GET,https://x.example/0/0,200,,,12,8");
        assert_eq!(
            lines[2],
            "1,Audit,GET,https://x.example/0/1,,,request timed out,,"
        );
        assert_eq!(
            lines[3],
            "2,Get user,GET,https://x.example/1/0,404,1/2,,12,7"
        );
        assert_eq!(lines.len(), 5);

        let json: serde_json::Value = serde_json::from_str(&export_json(&rows)).unwrap();
        assert_eq!(json[0]["status"], 200);
        assert_eq!(json[0]["body"], "{\"id\":1}");
        assert_eq!(json[0]["captured"][0]["value"], "abc");
        assert_eq!(json[0]["captured"][1]["error"], "the body is not JSON");
        assert_eq!(json[1]["captured"], serde_json::json!([]));
        assert_eq!(json[0]["passed"], true);
        assert_eq!(json[2]["passed"], false);
        assert_eq!(json[2]["assertions"][1]["error"], "got missing");
        assert_eq!(json[2]["assertions"][0]["actual"], "404");
        assert_eq!(json[1]["error"], "request timed out");
        assert_eq!(json[1]["status"], serde_json::Value::Null);
        assert_eq!(json[2]["iteration"], 2);
    }

    #[test]
    fn data_sources_feed_the_rows() {
        let mut tab = RunnerTab::new(RunTarget::Request);
        assert_eq!(tab.data_rows().unwrap(), Vec::<Row>::new());
        tab.use_lists(vec![ValueList::new("id", "1\n2")]);
        assert_eq!(tab.data_rows().unwrap().len(), 2);
        tab.lists[0].variable.clear();
        assert!(tab.data_rows().is_err());

        tab.data = DataKind::Csv;
        tab.set_csv_text("id\tname\n1\tada\n".into());
        assert!(tab.csv.header, "header guessed");
        tab.header_touched = true;
        tab.set_csv_text("1\tada\n".into());
        assert!(tab.csv.header, "kept once chosen");
        tab.csv.header = false;
        assert_eq!(
            tab.data_rows().unwrap()[0][1],
            ("col2".into(), "ada".into())
        );
    }

    #[test]
    fn config_lists_the_requests_and_blocks_bad_data() {
        let steps = [
            StepInfo {
                path: Some("users/get.req.toml".into()),
                name: "Get user".into(),
                method: Method::Get,
            },
            StepInfo {
                path: Some("users/create.req.toml".into()),
                name: "Create user".into(),
                method: Method::Post,
            },
        ];
        let mut tab = RunnerTab::new(RunTarget::Folder);
        let shapes = render(&mut tab, &steps);
        assert!(
            shapes.contains("Create user") && shapes.contains("2 requests"),
            "{shapes}"
        );

        tab.excluded.insert("users/get.req.toml".into());
        tab.use_lists(vec![ValueList::new("id", "1\n2\n3")]);
        assert!(render(&mut tab, &steps).contains("3 requests"));

        tab.lists.push(ValueList::new("name", "a"));
        let shapes = render(&mut tab, &steps);
        assert!(
            shapes.contains("{{id}} has 3 values but {{name}} has 1"),
            "{shapes}"
        );
        assert!(!shapes.contains("3 requests"));
    }

    #[test]
    fn folder_requests_follow_the_tree() {
        let request = |path: &str| {
            Item::Request(RequestEntry {
                path: path.into(),
                request: Request::new(Method::Get, "").with_name(path),
            })
        };
        let folder = |path: &str, items| Folder {
            path: path.into(),
            settings: Settings::default(),
            items,
        };
        let root = folder(
            "",
            vec![
                request("a"),
                Item::Folder(folder("sub", vec![request("sub/b"), request("sub/c")])),
                request("d"),
            ],
        );
        let names: Vec<_> = folder_requests(&root)
            .iter()
            .map(|entry| entry.request.name.as_str())
            .collect();
        assert_eq!(names, ["a", "sub/b", "sub/c", "d"]);
    }
}
