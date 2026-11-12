use std::fmt::Write as _;
use std::time::Duration;

use anstyle::{AnsiColor, Style};
use cornu_core::runner::{RunSummary, StepResult};
use cornu_core::{Environment, Response};
use cornu_storage::Workspace;
use serde_json::{Value, json};

use crate::workspace::{HIDDEN, Masker, display_path, folder_requests, kind};

const GREEN: Style = AnsiColor::Green.on_default();
const RED: Style = AnsiColor::Red.on_default();
const YELLOW: Style = AnsiColor::Yellow.on_default();
const DIM: Style = Style::new().dimmed();
const BOLD: Style = Style::new().bold();

pub fn millis(elapsed: Duration) -> u64 {
    elapsed.as_millis() as u64
}

fn seconds(elapsed: Duration) -> String {
    format!("{:.3}", elapsed.as_secs_f64())
}

fn name(names: &[String], result: &StepResult, iterations: usize) -> String {
    let name = names.get(result.step).cloned().unwrap_or_default();
    if iterations > 1 {
        format!("{name} #{}", result.iteration + 1)
    } else {
        name
    }
}

/// One line per request, then what went wrong with it.
pub fn print_result(result: &StepResult, names: &[String], iterations: usize) {
    let method = result.request.method.as_str();
    let name = name(names, result, iterations);
    match &result.outcome {
        Ok(response) => {
            let (mark, style) = if result.passed() {
                ("✔", GREEN)
            } else {
                ("✘", RED)
            };
            anstream::println!(
                "{style}{mark} {}{style:#}  {method:<7} {name}  {DIM}{} ms{DIM:#}",
                response.status,
                millis(response.elapsed)
            );
        }
        Err(error) => {
            anstream::println!("{RED}✘ ERR{RED:#}  {method:<7} {name}  {RED}{error}{RED:#}");
        }
    }
    for assertion in result
        .assertions
        .iter()
        .filter(|assertion| !assertion.passed())
    {
        anstream::println!(
            "    {RED}✘{RED:#} {}: {}",
            assertion.description,
            assertion.error.as_deref().unwrap_or_default()
        );
    }
    for captured in &result.captured {
        if let Err(error) = &captured.value {
            anstream::println!(
                "    {YELLOW}⚠{YELLOW:#} could not capture {}: {error}",
                captured.variable
            );
        }
    }
    for line in &result.logs {
        anstream::println!("    {DIM}›{DIM:#} {line}");
    }
    if !result.unresolved.is_empty() {
        anstream::println!(
            "    {YELLOW}⚠{YELLOW:#} unresolved variables: {}",
            result.unresolved.join(", ")
        );
    }
}

pub fn print_summary(summary: &RunSummary) {
    let total = summary.succeeded + summary.failed + summary.errors + summary.skipped;
    let mut line = format!(
        "{total} request{} · {GREEN}{} passed{GREEN:#}",
        if total == 1 { "" } else { "s" },
        summary.succeeded
    );
    let style = |count: usize, style: Style| if count > 0 { style } else { DIM };
    let failed = style(summary.failed, RED);
    let errors = style(summary.errors, RED);
    let _ = write!(line, " · {failed}{} failed{failed:#}", summary.failed);
    let _ = write!(line, " · {errors}{} errors{errors:#}", summary.errors);
    if summary.skipped > 0 {
        let _ = write!(line, " · {YELLOW}{} skipped{YELLOW:#}", summary.skipped);
    }
    let _ = write!(line, " · {:.2} s", summary.elapsed.as_secs_f64());
    anstream::println!("{BOLD}{line}{BOLD:#}");
}

/// The full response of a single request.
pub fn print_response(response: &Response) {
    for (name, value) in &response.headers {
        anstream::println!("{DIM}{name}:{DIM:#} {value}");
    }
    println!();
    let text = response.text();
    match serde_json::from_str::<Value>(&text) {
        Ok(value) if value.is_object() || value.is_array() => {
            println!(
                "{}",
                serde_json::to_string_pretty(&value).unwrap_or_default()
            );
        }
        _ => println!("{text}"),
    }
}

pub fn result_json(result: &StepResult, names: &[String], masker: &Masker) -> Value {
    let request = &result.request;
    let headers: Vec<Value> = cornu_core::key_value::enabled(&request.headers)
        .map(|(key, value)| json!([key, masker.mask(value)]))
        .collect();
    let mut value = json!({
        "name": names.get(result.step).cloned().unwrap_or_default(),
        "iteration": result.iteration + 1,
        "passed": result.passed(),
        "request": {
            "method": request.method.as_str(),
            "url": masker.mask(&request.url),
            "headers": headers,
        },
        "unresolved": result.unresolved,
        "logs": result.logs,
        "captures": result.captured.iter().map(|captured| match &captured.value {
            Ok(value) => json!({"variable": captured.variable, "value": value}),
            Err(error) => json!({"variable": captured.variable, "error": error.to_string()}),
        }).collect::<Vec<_>>(),
        "assertions": result.assertions.iter().map(|assertion| json!({
            "description": assertion.description,
            "passed": assertion.passed(),
            "actual": assertion.actual,
            "error": assertion.error,
        })).collect::<Vec<_>>(),
    });
    match &result.outcome {
        Ok(response) => {
            value["response"] = json!({
                "status": response.status,
                "elapsed_ms": millis(response.elapsed),
                "size": response.size(),
                "headers": response.headers.iter().map(|(name, value)| json!([name, value])).collect::<Vec<_>>(),
                "body": response.text(),
            });
        }
        Err(error) => value["error"] = json!(error.to_string()),
    }
    value
}

pub fn summary_json(summary: &RunSummary) -> Value {
    json!({
        "passed": summary.succeeded,
        "failed": summary.failed,
        "errors": summary.errors,
        "skipped": summary.skipped,
        "cancelled": summary.cancelled,
        "elapsed_ms": millis(summary.elapsed),
    })
}

pub fn junit(
    suite: &str,
    results: &[StepResult],
    names: &[String],
    iterations: usize,
    summary: &RunSummary,
) -> String {
    let mut cases = String::new();
    for result in results {
        let name = escape(&name(names, result, iterations));
        let time = result
            .outcome
            .as_ref()
            .map_or(String::from("0"), |response| seconds(response.elapsed));
        let _ = write!(
            cases,
            "    <testcase name=\"{name}\" classname=\"cornu\" time=\"{time}\""
        );
        let failures: Vec<String> = match &result.outcome {
            Err(_) => Vec::new(),
            Ok(_) if result.passed() => Vec::new(),
            Ok(response) => {
                let failed: Vec<String> = result
                    .assertions
                    .iter()
                    .filter(|assertion| !assertion.passed())
                    .map(|assertion| {
                        format!(
                            "{}: {}",
                            assertion.description,
                            assertion.error.as_deref().unwrap_or_default()
                        )
                    })
                    .collect();
                if failed.is_empty() {
                    vec![format!("status {}", response.status)]
                } else {
                    failed
                }
            }
        };
        match &result.outcome {
            Err(error) => {
                let message = escape(&error.to_string());
                let _ = writeln!(
                    cases,
                    ">\n      <error message=\"{message}\"/>\n    </testcase>"
                );
            }
            Ok(_) if failures.is_empty() => cases.push_str("/>\n"),
            Ok(_) => {
                let message = escape(&failures[0]);
                let details = escape(&failures.join("\n"));
                let _ = writeln!(
                    cases,
                    ">\n      <failure message=\"{message}\">{details}</failure>\n    </testcase>"
                );
            }
        }
    }
    let tests = results.len();
    let (failures, errors) = (summary.failed, summary.errors);
    let suite = escape(suite);
    let time = seconds(summary.elapsed);
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <testsuites name=\"cornu\" tests=\"{tests}\" failures=\"{failures}\" errors=\"{errors}\" time=\"{time}\">\n  \
         <testsuite name=\"{suite}\" tests=\"{tests}\" failures=\"{failures}\" errors=\"{errors}\" skipped=\"{}\" time=\"{time}\">\n\
         {cases}  </testsuite>\n</testsuites>\n",
        summary.skipped
    )
}

fn escape(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .fold(String::with_capacity(text.len()), |mut out, c| {
            match c {
                '&' => out.push_str("&amp;"),
                '<' => out.push_str("&lt;"),
                '>' => out.push_str("&gt;"),
                '"' => out.push_str("&quot;"),
                '\'' => out.push_str("&apos;"),
                c => out.push(c),
            }
            out
        })
}

pub fn list(folder: &cornu_storage::Folder, json: bool) {
    let requests = folder_requests(folder);
    if json {
        let items: Vec<Value> = requests
            .iter()
            .map(|entry| {
                json!({
                    "path": display_path(&entry.path),
                    "name": entry.request.name,
                    "method": entry.request.method.as_str(),
                    "url": entry.request.url,
                    "kind": kind(&entry.request),
                })
            })
            .collect();
        println!("{}", Value::Array(items));
        return;
    }
    if requests.is_empty() {
        anstream::println!("{DIM}No requests.{DIM:#}");
    }
    let width = requests
        .iter()
        .map(|entry| display_path(&entry.path).chars().count())
        .max()
        .unwrap_or(0);
    for entry in requests {
        let label = match kind(&entry.request) {
            "websocket" => "WS",
            "grpc" => "gRPC",
            _ => entry.request.method.as_str(),
        };
        let path = display_path(&entry.path);
        anstream::println!(
            "{BOLD}{label:<7}{BOLD:#} {path:<width$}  {DIM}{}{DIM:#}",
            entry.request.url
        );
    }
}

pub fn environments(workspace: &Workspace, show_secrets: bool, json: bool) {
    let environments: Vec<&Environment> = workspace
        .environments
        .iter()
        .map(|entry| &entry.environment)
        .collect();
    let value = |variable: &cornu_core::KeyValue| {
        if variable.secret && !show_secrets {
            None
        } else {
            Some(variable.value.clone())
        }
    };
    if json {
        let items: Vec<Value> = environments
            .iter()
            .map(|environment| {
                json!({
                    "name": environment.name,
                    "variables": environment.variables.iter().map(|variable| json!({
                        "name": variable.key,
                        "value": value(variable),
                        "secret": variable.secret,
                        "enabled": variable.enabled,
                    })).collect::<Vec<_>>(),
                })
            })
            .collect();
        println!("{}", Value::Array(items));
        return;
    }
    if environments.is_empty() {
        anstream::println!("{DIM}No environments.{DIM:#}");
    }
    for environment in environments {
        anstream::println!("{BOLD}{}{BOLD:#}", environment.name);
        for variable in &environment.variables {
            let shown = value(variable).unwrap_or_else(|| HIDDEN.to_owned());
            let disabled = if variable.enabled { "" } else { "  (disabled)" };
            anstream::println!("  {} = {shown}{DIM}{disabled}{DIM:#}", variable.key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xml_is_escaped() {
        assert_eq!(escape("a<b & \"c\"\u{1}"), "a&lt;b &amp; &quot;c&quot;");
    }
}
