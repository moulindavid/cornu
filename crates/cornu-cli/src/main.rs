mod report;
mod workspace;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use clap::{Args, Parser, Subcommand};
use cornu_core::runner::data::{CsvData, detect_delimiter, guess_header};
use cornu_core::runner::{CancelToken, RunOptions, RunPlan, Step, StepResult};
use cornu_core::{CookieJar, Environment, HttpClient, TokenStore};
use cornu_storage::Workspace;
use serde_json::json;
use workspace::{Masker, Target, display_path};

/// Runs the requests of a cornu workspace from a terminal, a script, an agent or CI.
///
/// Exit codes: 0 when every request passed, 1 when one failed or could not be sent, 2 for a
/// usage or workspace error.
#[derive(Parser)]
#[command(name = "cornu-cli", version)]
struct Cli {
    /// The workspace folder; by default the nearest one holding a cornu.toml, from the current
    /// folder up.
    #[arg(short, long, global = true, value_name = "DIR")]
    workspace: Option<PathBuf>,
    /// Print JSON instead of text, for scripts and agents.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run a request, a folder or the whole workspace, once or once per data row.
    Run(RunArgs),
    /// Send one request and print its full response.
    Send(SendArgs),
    /// List the requests of the workspace, or of a folder.
    List {
        /// A folder, relative to the workspace or to the current folder.
        path: Option<String>,
    },
    /// List the environments and their variables, with secrets hidden.
    Envs {
        #[arg(long)]
        show_secrets: bool,
    },
}

#[derive(Args)]
struct VariableArgs {
    /// The environment to use, by name.
    #[arg(short, long, value_name = "NAME")]
    env: Option<String>,
    /// Set a variable, over the workspace and environment values. Repeatable.
    #[arg(long = "var", value_name = "NAME=VALUE", value_parser = parse_variable)]
    variables: Vec<(String, String)>,
    /// Print secret values instead of hiding them.
    #[arg(long)]
    show_secrets: bool,
}

#[derive(Args)]
struct RunArgs {
    /// A request or a folder, relative to the workspace or to the current folder; the whole
    /// workspace by default.
    path: Option<String>,
    #[command(flatten)]
    variables: VariableArgs,
    /// A CSV file: one iteration per row, with its columns as variables (col1, col2… without a
    /// header row).
    #[arg(long, value_name = "FILE")]
    data: Option<PathBuf>,
    /// The first row of the CSV names its columns; guessed by default.
    #[arg(long, requires = "data", conflicts_with = "no_header")]
    header: bool,
    /// The first row of the CSV is data.
    #[arg(long, requires = "data")]
    no_header: bool,
    /// How many iterations run at once.
    #[arg(long, default_value_t = 1, value_name = "N")]
    concurrency: usize,
    /// Pause after each request, in milliseconds.
    #[arg(long, default_value_t = 0, value_name = "MS")]
    delay: u64,
    /// Stop at the first failure.
    #[arg(long)]
    bail: bool,
    /// Also write a JUnit XML report, for CI test reports.
    #[arg(long, value_name = "FILE")]
    junit: Option<PathBuf>,
}

#[derive(Args)]
struct SendArgs {
    /// The request, relative to the workspace or to the current folder.
    path: String,
    #[command(flatten)]
    variables: VariableArgs,
}

fn parse_variable(text: &str) -> Result<(String, String), String> {
    match text.split_once('=') {
        Some((name, value)) if !name.trim().is_empty() => {
            Ok((name.trim().to_owned(), value.to_owned()))
        }
        _ => Err(format!("expected NAME=VALUE, got {text:?}")),
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let json = cli.json;
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => return fail(json, &error.to_string()),
    };
    match runtime.block_on(execute(cli)) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(error) => fail(json, &error),
    }
}

fn fail(json: bool, error: &str) -> ExitCode {
    if json {
        println!("{}", json!({ "error": error }));
    } else {
        anstream::eprintln!(
            "{}error:{} {error}",
            anstyle::AnsiColor::Red.on_default().bold().render(),
            anstyle::Reset.render()
        );
    }
    ExitCode::from(2)
}

/// Whether everything passed.
async fn execute(cli: Cli) -> Result<bool, String> {
    let root = workspace::find_root(cli.workspace.as_deref())?;
    let workspace = Workspace::load(&root).map_err(|error| error.to_string())?;
    for problem in &workspace.problems {
        anstream::eprintln!("warning: {problem}");
    }
    match cli.command {
        Command::List { path } => {
            let folder = match workspace::target(&workspace, path.as_deref())? {
                Target::Folder(folder) => folder,
                Target::Request(_) => return Err("list takes a folder, not a request".into()),
            };
            report::list(folder, cli.json);
            Ok(true)
        }
        Command::Envs { show_secrets } => {
            report::environments(&workspace, show_secrets, cli.json);
            Ok(true)
        }
        Command::Send(args) => {
            let Target::Request(entry) = workspace::target(&workspace, Some(&args.path))? else {
                return Err(format!(
                    "{} is a folder: use `cornu-cli run` for folders",
                    args.path
                ));
            };
            let run = Run {
                workspace: &workspace,
                variables: &args.variables,
                json: cli.json,
            };
            let plan = run.plan(&[entry], Vec::new(), RunOptions::default())?;
            run.send(plan).await
        }
        Command::Run(args) => {
            let target = workspace::target(&workspace, args.path.as_deref())?;
            let entries = target.requests();
            if entries.is_empty() {
                return Err(format!(
                    "nothing to run in {}",
                    args.path.as_deref().unwrap_or("the workspace")
                ));
            }
            let rows = match &args.data {
                Some(path) => rows(path, args.header, args.no_header)?,
                None => Vec::new(),
            };
            let options = RunOptions {
                concurrency: args.concurrency,
                delay: Duration::from_millis(args.delay),
                bail: args.bail,
            };
            let run = Run {
                workspace: &workspace,
                variables: &args.variables,
                json: cli.json,
            };
            let plan = run.plan(&entries, rows, options)?;
            run.run(plan, args.junit.as_deref()).await
        }
    }
}

fn rows(
    path: &Path,
    header: bool,
    no_header: bool,
) -> Result<Vec<cornu_core::runner::Row>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("could not read {}: {error}", path.display()))?;
    let header = header || (!no_header && guess_header(&text, detect_delimiter(&text)));
    CsvData::new(text, header)
        .rows()
        .map_err(|error| format!("{}: {error}", path.display()))
}

struct Run<'a> {
    workspace: &'a Workspace,
    variables: &'a VariableArgs,
    json: bool,
}

impl Run<'_> {
    fn environment(&self) -> Result<Option<&Environment>, String> {
        workspace::environment(self.workspace, self.variables.env.as_deref())
    }

    fn plan(
        &self,
        entries: &[&cornu_storage::RequestEntry],
        rows: Vec<cornu_core::runner::Row>,
        options: RunOptions,
    ) -> Result<(RunPlan, Vec<String>), String> {
        let environment = self.environment()?;
        let steps = entries
            .iter()
            .map(|entry| {
                let mut request = entry.request.clone();
                request.auth = self.workspace.effective_auth(&entry.path, &request.auth);
                let variables = self
                    .workspace
                    .variables(&entry.path, environment)
                    .with_layer(self.variables.variables.iter().cloned());
                Step { request, variables }
            })
            .collect();
        let names = entries
            .iter()
            .map(|entry| display_path(&entry.path))
            .collect();
        Ok((
            RunPlan {
                steps,
                rows,
                options,
            },
            names,
        ))
    }

    fn masker(&self) -> Result<Masker, String> {
        Ok(if self.variables.show_secrets {
            Masker::default()
        } else {
            Masker::new(self.workspace, self.environment()?)
        })
    }

    /// With the workspace's client settings, cookies and OAuth2 tokens, as in the app.
    fn client(&self) -> Result<(HttpClient, CookieJar, TokenStore), String> {
        let root = &self.workspace.root;
        let options = self.workspace.tree.settings.client.relative_to(root);
        let (cookies, tokens) = (CookieJar::default(), TokenStore::default());
        if let Err(error) = cornu_storage::load_cookies(root, &cookies) {
            anstream::eprintln!("warning: {error}");
        }
        if let Err(error) = cornu_storage::load_tokens(root, &tokens) {
            anstream::eprintln!("warning: {error}");
        }
        let client = HttpClient::with_cookies(&options, cookies.clone())
            .map_err(|error| error.to_string())?
            .with_tokens(tokens.clone());
        Ok((client, cookies, tokens))
    }

    fn save_state(&self, cookies: &CookieJar, tokens: &TokenStore) {
        let root = &self.workspace.root;
        for result in [
            cornu_storage::save_cookies(root, cookies),
            cornu_storage::save_tokens(root, tokens),
        ] {
            if let Err(error) = result {
                anstream::eprintln!("warning: {error}");
            }
        }
    }

    async fn execute(
        &self,
        plan: RunPlan,
        mut each: impl FnMut(&StepResult),
    ) -> Result<(Vec<StepResult>, cornu_core::runner::RunSummary), String> {
        let (client, cookies, tokens) = self.client()?;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let run = tokio::spawn(async move {
            cornu_core::runner::run(&client, plan, &CancelToken::default(), tx).await
        });
        let mut results = Vec::new();
        while let Some(result) = rx.recv().await {
            each(&result);
            results.push(result);
        }
        let summary = run.await.map_err(|error| error.to_string())?;
        self.save_state(&cookies, &tokens);
        results.sort_by_key(|result| (result.iteration, result.step));
        Ok((results, summary))
    }

    async fn run(
        &self,
        (plan, names): (RunPlan, Vec<String>),
        junit: Option<&Path>,
    ) -> Result<bool, String> {
        let masker = self.masker()?;
        let iterations = plan.iterations();
        let json = self.json;
        let (results, summary) = self
            .execute(plan, |result| {
                if !json {
                    report::print_result(result, &names, iterations);
                }
            })
            .await?;
        if let Some(path) = junit {
            let suite = &self.workspace.tree.settings.name;
            let xml = report::junit(suite, &results, &names, iterations, &summary);
            std::fs::write(path, xml)
                .map_err(|error| format!("could not write {}: {error}", path.display()))?;
        }
        if json {
            let results: Vec<_> = results
                .iter()
                .map(|result| report::result_json(result, &names, &masker))
                .collect();
            println!(
                "{}",
                json!({ "results": results, "summary": report::summary_json(&summary) })
            );
        } else {
            println!();
            report::print_summary(&summary);
        }
        Ok(summary.failed == 0 && summary.errors == 0)
    }

    async fn send(&self, (plan, names): (RunPlan, Vec<String>)) -> Result<bool, String> {
        let masker = self.masker()?;
        let (results, _) = self.execute(plan, |_| {}).await?;
        let Some(result) = results.first() else {
            return Err("the request was not sent".into());
        };
        if self.json {
            println!("{}", report::result_json(result, &names, &masker));
        } else {
            report::print_result(result, &names, 1);
            if let Ok(response) = &result.outcome {
                report::print_response(response);
            }
        }
        Ok(result.passed())
    }
}
