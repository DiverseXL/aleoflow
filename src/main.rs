mod leo_cmd;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, Args};
use colored::*;
use serde::Deserialize;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

// ---------------------------------------------------------------------------
// Optional aleo.toml config file support
// ---------------------------------------------------------------------------

/// Configuration loaded from aleo.toml (optional, in current working directory).
/// Falls back gracefully if the file is missing or malformed.
#[derive(Deserialize, Default)]
struct AleoFlowConfig {
    #[serde(default)]
    default_network: Option<String>,
    #[serde(default)]
    default_template: Option<String>,
}

/// Try to load aleo.toml from the current working directory.
/// Returns None (no crash) if the file doesn't exist or is malformed.
fn load_aleoflow_config() -> AleoFlowConfig {
    let config_path = Path::new("aleo.toml");
    if !config_path.exists() {
        return AleoFlowConfig::default();
    }
    match fs::read_to_string(config_path) {
        Ok(content) => match toml::from_str(&content) {
            Ok(cfg) => cfg,
            Err(e) => {
                eprintln!(
                    "[warning] aleo.toml found but could not be parsed: {}",
                    e
                );
                AleoFlowConfig::default()
            }
        },
        Err(e) => {
            eprintln!(
                "[warning] Could not read aleo.toml: {}",
                e
            );
            AleoFlowConfig::default()
        }
    }
}

/// Parse a network name from the config into the Network enum.
fn parse_network(s: &str) -> Option<Network> {
    match s.to_lowercase().as_str() {
        "testnet" => Some(Network::Testnet),
        "mainnet" => Some(Network::Mainnet),
        "canary" => Some(Network::Canary),
        _ => None,
    }
}

/// Parse a template name from the config into the Template enum.
fn parse_template(s: &str) -> Option<Template> {
    match s.to_lowercase().as_str() {
        "payment" => Some(Template::Payment),
        "defi" => Some(Template::Defi),
        "ai-agent" => Some(Template::AiAgent),
        "gamefi" => Some(Template::Gamefi),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Compile-time embedded template files
// ---------------------------------------------------------------------------

/// A single embedded template file with its relative path (within the template).
struct EmbeddedFile {
    rel_path: &'static str,
    contents: &'static str,
}

/// A scaffold template definition.
struct EmbeddedTemplate {
    name: &'static str,
    files: &'static [EmbeddedFile],
}

/// All scaffold templates, embedded at compile time via `include_str!`.
static TEMPLATES: &[EmbeddedTemplate] = &[
    EmbeddedTemplate {
        name: "payment",
        files: &[
            EmbeddedFile { rel_path: "program.json",   contents: include_str!("../templates/payment/program.json") },
            EmbeddedFile { rel_path: "src/main.leo",   contents: include_str!("../templates/payment/src/main.leo") },
            EmbeddedFile { rel_path: "README.md",      contents: include_str!("../templates/payment/README.md") },
        ],
    },
    EmbeddedTemplate {
        name: "defi",
        files: &[
            EmbeddedFile { rel_path: "program.json",   contents: include_str!("../templates/defi/program.json") },
            EmbeddedFile { rel_path: "src/main.leo",   contents: include_str!("../templates/defi/src/main.leo") },
            EmbeddedFile { rel_path: "README.md",      contents: include_str!("../templates/defi/README.md") },
        ],
    },
    EmbeddedTemplate {
        name: "ai-agent",
        files: &[
            EmbeddedFile { rel_path: "program.json",   contents: include_str!("../templates/ai-agent/program.json") },
            EmbeddedFile { rel_path: "src/main.leo",   contents: include_str!("../templates/ai-agent/src/main.leo") },
            EmbeddedFile { rel_path: "README.md",      contents: include_str!("../templates/ai-agent/README.md") },
        ],
    },
    EmbeddedTemplate {
        name: "gamefi",
        files: &[
            EmbeddedFile { rel_path: "program.json",   contents: include_str!("../templates/gamefi/program.json") },
            EmbeddedFile { rel_path: "src/main.leo",   contents: include_str!("../templates/gamefi/src/main.leo") },
            EmbeddedFile { rel_path: "README.md",      contents: include_str!("../templates/gamefi/README.md") },
        ],
    },
];

/// Look up an embedded template by its CLI name.
fn find_template(name: &str) -> Option<&'static EmbeddedTemplate> {
    TEMPLATES.iter().find(|t| t.name == name)
}

#[derive(Parser)]
#[command(name = "aleoflow")]
#[command(about = "A developer toolkit for building on Aleo", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Suppress [info] messages for quieter output (useful with --json-output)
    #[arg(short = 'q', long, global = true)]
    quiet: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Scaffold a new Aleo project from a template
    Init(InitArgs),
    /// Start a local devnet node
    Devnet(DevnetArgs),
    /// Compile the current Aleo project
    Build(BuildArgs),
    /// Run project tests
    Test(TestArgs),
    /// Deploy a compiled Aleo program to a network
    Deploy(DeployArgs),
    /// Run a security audit on an Aleo project
    Audit(AuditArgs),
    /// Generate TypeScript bindings from a compiled Aleo program's ABI
    Bindings(BindingsArgs),
}

#[derive(Args)]
struct InitArgs {
    /// Name of the new project
    name: String,
    /// Project template to use (defaults to 'payment', or to aleo.toml's default_template)
    #[arg(long = "template", value_parser = clap::value_parser!(Template))]
    template: Option<Template>,
}

#[derive(Args)]
struct BuildArgs {
    /// Path to the Aleo project directory (defaults to current dir)
    #[arg(long)]
    path: Option<PathBuf>,
    /// Write command results as JSON (optionally --json-output=<FILE> for a custom path)
    #[arg(long)]
    json_output: Option<Option<PathBuf>>,
}

#[derive(Args)]
struct TestArgs {
    /// Path to the Aleo project directory (defaults to current dir)
    #[arg(long)]
    path: Option<PathBuf>,
    /// Write command results as JSON (optionally --json-output=<FILE> for a custom path)
    #[arg(long)]
    json_output: Option<Option<PathBuf>>,
}

#[derive(Args)]
struct DevnetArgs {
    /// Path to the Aleo project directory
    #[arg(long)]
    path: Option<PathBuf>,
    /// Network to connect the devnet to (testnet, mainnet, canary)
    #[arg(long)]
    network: Option<Network>,
    /// Write command results as JSON (optionally --json-output=<FILE> for a custom path)
    #[arg(long)]
    json_output: Option<Option<PathBuf>>,
}

#[derive(Args)]
struct DeployArgs {
    /// Path to the Leo program root folder
    #[arg(long)]
    path: Option<PathBuf>,
    /// Target network (defaults to 'testnet', or to aleo.toml's default_network)
    #[arg(long = "network", value_parser = clap::value_parser!(Network))]
    network: Option<Network>,
    /// Actually broadcast the deployment transaction (without this, runs in dry-run mode)
    #[arg(long)]
    broadcast: bool,
    /// Write command results as JSON (optionally --json-output=<FILE> for a custom path)
    #[arg(long)]
    json_output: Option<Option<PathBuf>>,
}

#[derive(Args)]
struct AuditArgs {
    /// Path to the Aleo project to audit
    path: String,
}

#[derive(Args)]
struct BindingsArgs {
    /// Path to the Aleo project directory
    path: PathBuf,
    /// Output path for the generated TypeScript file (defaults to <path>/bindings/<program_name>.ts)
    #[arg(long)]
    output: Option<PathBuf>,
}

#[derive(clap::ValueEnum, Clone)]
enum Template {
    Payment,
    Defi,
    AiAgent,
    Gamefi,
}

#[derive(clap::ValueEnum, Clone)]
enum Network {
    Testnet,
    Mainnet,
    Canary,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let quiet = cli.quiet;

    match &cli.command {
        Commands::Init(args) => handle_init(args, quiet),
        Commands::Devnet(args) => handle_devnet(args, quiet),
        Commands::Build(args) => handle_build(args, quiet),
        Commands::Test(args) => handle_test(args, quiet),
        Commands::Deploy(args) => handle_deploy(args, quiet),
        Commands::Audit(args) => handle_audit(args, quiet),
        Commands::Bindings(args) => handle_bindings(args, quiet),
    }
}

/// Print an [info] message unless --quiet is active.
fn print_info(msg: &str, quiet: bool) {
    if !quiet {
        println!("{} {}", "[info]".cyan().bold(), msg);
    }
}

fn handle_init(args: &InitArgs, quiet: bool) -> Result<()> {
    let cfg = load_aleoflow_config();

    // Resolve template: CLI flag > config > default (Payment)
    let template_enum = args.template.clone().or_else(move || {
        cfg.default_template
            .as_deref()
            .and_then(parse_template)
            .inspect(|t| {
                if !quiet {
                    let name = match t {
                        Template::Payment => "payment",
                        Template::Defi => "defi",
                        Template::AiAgent => "ai-agent",
                        Template::Gamefi => "gamefi",
                    };
                    println!(
                        "{} Using default_template '{}' from aleo.toml",
                        "[info]".cyan().bold(),
                        name
                    );
                }
            })
    }).unwrap_or(Template::Payment);

    let template_name = match template_enum {
        Template::Payment => "payment",
        Template::Defi => "defi",
        Template::AiAgent => "ai-agent",
        Template::Gamefi => "gamefi",
    };

    let template = find_template(template_name).with_context(|| {
        format!(
            "Template '{}' not found. This is a bug -- please reinstall aleoflow.",
            template_name
        )
    })?;

    let dest_dir = Path::new(&args.name);
    if dest_dir.exists() {
        bail!(
            "Destination directory '{}' already exists -- not overwriting",
            dest_dir.display()
        );
    }

    // Sanitize the project name for use as a program ID:
    // hyphens are illegal in Aleo program identifiers.
    let program_id = args.name.replace('-', "_");

    // Write each embedded template file to the destination with substitution.
    for file in template.files {
        let dest = dest_dir.join(file.rel_path);

        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!("Failed to create directory '{}'", parent.display())
            })?;
        }

        let substituted = file.contents.replace("{{PROJECT_NAME}}", &program_id);

        fs::write(&dest, &substituted).with_context(|| {
            format!("Failed to write project file '{}'", dest.display())
        })?;
    }

    println!(
        "{} Created new project '{}' from '{}' template",
        "[done]".green().bold(),
        args.name.cyan(),
        template_name.yellow()
    );
    println!(
        "  {}",
        dest_dir
            .canonicalize()
            .unwrap_or_else(|_| dest_dir.to_path_buf())
            .display()
    );
    println!();
    println!("  {} cd {}", "$".dimmed(), args.name);
    println!("  {} leo build", "$".dimmed());

    Ok(())
}

fn handle_build(args: &BuildArgs, quiet: bool) -> Result<()> {
    let dir = args.path.as_deref();
    let json_flags = leo_cmd::json_output_flag(&args.json_output);

    if !json_flags.is_empty() {
        print_info(
            "JSON output will be written by leo (see leo's own output path).",
            quiet,
        );
    } else {
        print_info("Running 'leo build'...", quiet);
    }

    leo_cmd::run_leo_with("build", &json_flags, dir)
}

fn handle_test(args: &TestArgs, quiet: bool) -> Result<()> {
    let dir = args.path.as_deref();
    let json_flags = leo_cmd::json_output_flag(&args.json_output);

    if !json_flags.is_empty() {
        print_info(
            "JSON output will be written by leo (see leo's own output path).",
            quiet,
        );
    } else {
        print_info("Running 'leo test'...", quiet);
    }

    leo_cmd::run_leo_with("test", &json_flags, dir)
}

fn handle_devnet(args: &DevnetArgs, quiet: bool) -> Result<()> {
    if !leo_cmd::leo_is_installed() {
        bail!(
            "leo is not installed or not on PATH. Install it with: cargo binstall leo-lang"
        );
    }

    let cfg = load_aleoflow_config();

    // Resolve network: CLI flag > config > default (Testnet)
    let network = args.network.clone().or_else(move || {
        cfg.default_network
            .as_deref()
            .and_then(parse_network)
            .inspect(|n| {
                if !quiet {
                    let name = match n {
                        Network::Testnet => "testnet",
                        Network::Mainnet => "mainnet",
                        Network::Canary => "canary",
                    };
                    println!(
                        "{} Using default_network '{}' from aleo.toml",
                        "[info]".cyan().bold(),
                        name
                    );
                }
            })
    });

    let json_flags = leo_cmd::json_output_flag(&args.json_output);
    if !json_flags.is_empty() {
        print_info(
            "JSON output will be written by leo (see leo's own output path).",
            quiet,
        );
    } else {
        print_info("Starting local devnet...", quiet);
    }

    let mut cmd = std::process::Command::new("leo");
    cmd.arg("devnet");

    for flag in &json_flags {
        cmd.arg(flag);
    }

    if let Some(path) = &args.path {
        cmd.args(["--path", &path.to_string_lossy()]);
    }

    if let Some(net) = &network {
        let net_str = match net {
            Network::Testnet => "testnet",
            Network::Mainnet => "mainnet",
            Network::Canary => "canary",
        };
        cmd.args(["--network", net_str]);
    }

    let status = cmd.status().with_context(|| {
        "Failed to execute 'leo devnet'".to_string()
    })?;

    if !status.success() {
        let code = status.code().unwrap_or(-1);
        bail!("'leo devnet' failed with exit code {}. \
               Make sure snarkOS is installed. Run 'leo devnet --install' to install it.", code);
    }
    Ok(())
}

fn handle_deploy(args: &DeployArgs, quiet: bool) -> Result<()> {
    let cfg = load_aleoflow_config();

    // Resolve network: CLI flag > config > default (Testnet)
    let network = args.network.clone().or_else(move || {
        cfg.default_network
            .as_deref()
            .and_then(parse_network)
            .inspect(|n| {
                if !quiet {
                    let name = match n {
                        Network::Testnet => "testnet",
                        Network::Mainnet => "mainnet",
                        Network::Canary => "canary",
                    };
                    println!(
                        "{} Using default_network '{}' from aleo.toml",
                        "[info]".cyan().bold(),
                        name
                    );
                }
            })
    }).unwrap_or(Network::Testnet);

    let network_str = match network {
        Network::Testnet => "testnet",
        Network::Mainnet => "mainnet",
        Network::Canary => "canary",
    };

    if !leo_cmd::leo_is_installed() {
        bail!("leo is not installed or not on PATH. Install it with: cargo binstall leo-lang");
    }

    let json_flags = leo_cmd::json_output_flag(&args.json_output);

    // Mainnet + broadcast: print informational warning
    if args.broadcast && matches!(network, Network::Mainnet) {
        println!(
            "{} {}",
            "[warning]".yellow().bold(),
            "Deploying to MAINNET with --broadcast. This is irreversible and costs real fees."
        );
    }

    if !json_flags.is_empty() {
        print_info(
            "JSON output will be written by leo (see leo's own output path).",
            quiet,
        );
    } else if args.broadcast {
        print_info(
            &format!("Broadcasting deployment to '{}'...", network_str),
            quiet,
        );
    } else {
        print_info(
            "Running in dry-run mode (no --broadcast passed). Add --broadcast to actually deploy.",
            quiet,
        );
    }

    // Build the leo command using --path as a CLI arg (matches leo deploy's own flags)
    let mut cmd = std::process::Command::new("leo");
    cmd.args(["deploy", "--network", network_str]);

    for flag in &json_flags {
        cmd.arg(flag);
    }

    if let Some(path) = &args.path {
        cmd.args(["--path", &path.to_string_lossy()]);
    }

    if args.broadcast {
        cmd.arg("--broadcast");
    }

    // Do NOT pass --yes to leo. Leo's own help text warns against it:
    // "DO NOT SET THIS FLAG UNLESS YOU KNOW WHAT YOU ARE DOING"
    // Let leo's own confirmation prompts surface via inherited stdout/stderr.

    let status = cmd.status().with_context(|| {
        format!("Failed to execute 'leo deploy --network {}'", network_str)
    })?;

    if !status.success() {
        let code = status.code().unwrap_or(-1);
        bail!("'leo deploy --network {}' failed with exit code {}", network_str, code);
    }

    Ok(())
}

fn handle_bindings(args: &BindingsArgs, quiet: bool) -> Result<()> {
    let project_dir = &args.path;
    if !project_dir.is_dir() {
        bail!("Project directory '{}' does not exist", project_dir.display());
    }

    // Read program.json to get the program name
    let program_json_path = project_dir.join("program.json");
    let program_json_str = fs::read_to_string(&program_json_path)
        .with_context(|| format!("Failed to read '{}'", program_json_path.display()))?;
    let program_json: serde_json::Value = serde_json::from_str(&program_json_str)
        .context("Failed to parse program.json")?;
    let program_name = program_json["program"]
        .as_str()
        .context("program.json is missing the 'program' field")?;
    let program_id = program_name.trim_end_matches(".aleo");

    // Locate the ABI JSON file (generated by leo build)
    let abi_path = project_dir.join("build").join(program_id).join("abi.json");

    let abi_content = if abi_path.exists() {
        fs::read_to_string(&abi_path)?
    } else {
        // Build first if ABI doesn't exist yet
        print_info(
            &format!(
                "No ABI found at '{}'. Running 'leo build' first...",
                abi_path.display()
            ),
            quiet,
        );
        leo_cmd::run_leo("build", Some(project_dir))?;
        if !abi_path.exists() {
            bail!(
                "ABI was not generated at '{}' after building. \
                 Check that 'leo build' succeeds inside the project.",
                abi_path.display()
            );
        }
        fs::read_to_string(&abi_path)?
    };

    let abi: serde_json::Value = serde_json::from_str(&abi_content)
        .context("Failed to parse ABI JSON")?;
    let functions = abi["functions"]
        .as_array()
        .context("ABI is missing 'functions' array")?;

    // Read the Leo source file to extract parameter names and distinguish
    // `transition`s (public, should be in bindings) from `fn`s (private, skipped).
    // The ABI JSON does not preserve parameter names or kind (fn vs transition).
    let leo_source_path = project_dir.join("src").join("main.leo");
    let leo_source = if leo_source_path.exists() {
        Some(fs::read_to_string(&leo_source_path)?)
    } else {
        None
    };

    // --- Generate TypeScript bindings ---
    let mut ts = String::new();

    ts.push_str("// Auto-generated TypeScript bindings for '");
    ts.push_str(program_name);
    ts.push_str("'\n");
    ts.push_str("//\n");
    ts.push_str("// This is a scaffold, not a finished SDK integration.\n");
    ts.push_str("// Wire up actual execution using the @provablehq/sdk:\n");
    ts.push_str("//   https://docs.aleo.org/build/sdk/getting_started\n");
    ts.push_str("//\n");
    ts.push_str("// Generated by AleoFlow bindings\n");
    ts.push_str("\n");
    ts.push_str("// import { ProgramManager, AleoNetworkClient } from '@provablehq/sdk';\n");
    ts.push_str("\n");
    ts.push_str(&format!("// --- Program: {} ---\n", program_name));
    ts.push_str("\n");

    for func in functions {
        let name = func["name"].as_str().unwrap_or("unknown");
        let inputs = func["inputs"].as_array().map(|v| &v[..]).unwrap_or(&[]);
        let outputs = func["outputs"].as_array().map(|v| &v[..]).unwrap_or(&[]);

        // Issue 3: In Leo 4.x the `transition` keyword has been removed;
        // all callables use `fn`. The ABI does not distinguish kinds, so
        // ALL functions in the ABI are included in the bindings.
        // (Retained as a comment: if `transition` is ever reintroduced,
        // add a check via `leo_function_kind` and skip `fn`-only entries.)

        // Issue 1: Extract real parameter names from the Leo source.
        // The ABI only provides types, not names.
        let param_names = leo_source.as_deref()
            .and_then(|src| leo_param_names(src, name))
            .unwrap_or_default();

        // Build parameter list with TypeScript types
        let mut params: Vec<String> = Vec::new();
        for (i, input) in inputs.iter().enumerate() {
            let ts_type = param_leo_type(input);
            // Use the real parameter name if available, otherwise fall back to arg0/arg1
            let pname: String = param_names.get(i)
                .cloned()
                .unwrap_or_else(|| format!("arg{}", i));
            params.push(format!("{}: {}", pname, ts_type));
        }

        // Build return type
        let return_ts = if outputs.is_empty() {
            "void".to_string()
        } else if outputs.len() == 1 {
            param_leo_type(&outputs[0]).to_string()
        } else {
            let types: Vec<String> = outputs
                .iter()
                .map(|o| param_leo_type(o).to_string())
                .collect();
            format!("[{}]", types.join(", "))
        };

        // Issue 2: No reserved-keyword collision with function names.
        // (Leo function names like `transfer` are not TypeScript reserved words.
        // If collisions arise with a specific SDK method, add the function name
        // to a RESERVED_KEYWORDS list below and the generator will append `_`.)
        //
        // Reserved keywords that trigger `_` suffix:
        //   (none currently)
        ts.push_str(&format!("// {}\n", name));
        ts.push_str(&format!(
            "export async function {}(\n  {}\n): Promise<{}> {{\n",
            name,
            params.join(",\n  "),
            return_ts
        ));
        ts.push_str("  // TODO: Wire up with @provablehq/sdk\n");
        ts.push_str("  // const network = new AleoNetworkClient('https://api.explorer.provable.com/v1');\n");
        ts.push_str("  // const programManager = new ProgramManager(network);\n");
        ts.push_str("  // return await programManager.execute({\n");
        ts.push_str(&format!("  //   programName: '{}',\n", program_name));
        ts.push_str(&format!("  //   functionName: '{}',\n", name));
        ts.push_str("  //   inputs: [");
        for (i, _) in inputs.iter().enumerate() {
            if i > 0 {
                ts.push_str(", ");
            }
            let pname: String = param_names.get(i)
                .cloned()
                .unwrap_or_else(|| format!("arg{}", i));
            ts.push_str(&format!("{}.toString()", pname));
        }
        ts.push_str("],\n");
        ts.push_str("  //   privateKey: process.env.PRIVATE_KEY,\n");
        ts.push_str("  // });\n");
        ts.push_str("  throw new Error('Not implemented — wire up with @provablehq/sdk');\n");
        ts.push_str("}\n\n");
    }

    // Determine output path
    let output_path = if let Some(out) = &args.output {
        out.clone()
    } else {
        project_dir
            .join("bindings")
            .join(format!("{}.ts", program_id))
    };

    // Create parent directory
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)?;
    }

    fs::write(&output_path, &ts).with_context(|| {
        format!("Failed to write '{}'", output_path.display())
    })?;

    // Summary
    println!(
        "{} Generated TypeScript bindings for '{}' with {} function(s)",
        "[done]".green().bold(),
        program_name,
        functions.len()
    );
    println!("  Output: {}", output_path.display());

    Ok(())
}

// ---------------------------------------------------------------------------
// Leo source parsing helpers (for binding generation)
// ---------------------------------------------------------------------------

/// Extract parameter names from a Leo function/transition signature.
/// The ABI JSON does not preserve parameter names, so we fall back to
/// parsing the .leo source.
///
/// Example line:
///   `fn transfer(sender: address, receiver: address, amount: u64) -> u64 {`
///   → returns ["sender", "receiver", "amount"]
fn leo_param_names(source: &str, func_name: &str) -> Option<Vec<String>> {
    for line in source.lines() {
        let trimmed = line.trim();
        if !trimmed.contains(func_name) || !trimmed.contains('(') {
            continue;
        }

        // Extract the content between ( and )
        let paren_open = trimmed.find('(')?;
        // Find matching closing paren: scan forward counting nesting
        let after_open = &trimmed[paren_open + 1..];
        let mut paren_close = None;
        let mut depth = 1i32;
        for (i, ch) in after_open.char_indices() {
            match ch {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        paren_close = Some(paren_open + 1 + i);
                        break;
                    }
                }
                _ => {}
            }
        }
        let params_str = &trimmed[paren_open + 1..paren_close?];

        // Split by commas, extract the identifier before `:` for each
        let mut names = Vec::new();
        for segment in params_str.split(',') {
            let segment = segment.trim();
            if segment.is_empty() {
                continue;
            }
            let name = if let Some(col) = segment.find(':') {
                segment[..col].trim().to_string()
            } else {
                segment.to_string()
            };
            names.push(name);
        }

        if !names.is_empty() {
            return Some(names);
        }
    }
    None
}

/// Extract the TypeScript type string from an ABI parameter value.
fn param_leo_type(param: &serde_json::Value) -> String {
    // The parameter is wrapped in a variant key like "Plaintext" or "Record"
    let inner = if let Some(pt) = param.get("Plaintext") {
        pt
    } else if let Some(rec) = param.get("Record") {
        return format!(
            "string /* record {} */",
            rec["record"].as_str().unwrap_or("unknown")
        );
    } else {
        return "unknown".to_string();
    };

    // `inner` has { "ty": ..., "mode": "Private" | "Public" }
    leo_ty_to_ts(&inner["ty"])
}

/// Map a Leo ABI type object to a TypeScript type name.
///
/// The ABI encodes types as nested JSON, e.g.:
///   `{ "Primitive": { "UInt": "U64" } }`  → unsigned 64-bit → `bigint`
///   `{ "Primitive": { "UInt": "U32" } }`  → unsigned 32-bit → `number`
///   `{ "Primitive": { "Boolean": null } }` → `boolean`
/// The key is the type family and the *value* carries the bit-width for UInt.
fn leo_ty_to_ts(ty: &serde_json::Value) -> String {
    if let Some(prim) = ty.get("Primitive") {
        if let Some(obj) = prim.as_object() {
            for (type_name, size_val) in obj {
                return match type_name.as_str() {
                    "Boolean" => "boolean".to_string(),
                    "Int8" | "Int16" | "Int32" => "number".to_string(),
                    "Int64" | "Int128" => "bigint".to_string(),
                    // UInt uses the value for bit-width: "U8", "U16", "U32", "U64", "U128"
                    "UInt" | "UInt8" | "UInt16" | "UInt32" | "UInt64" | "UInt128" => {
                        match size_val.as_str() {
                            Some("U8") | Some("U16") | Some("U32") => "number".to_string(),
                            _ => "bigint".to_string(), // default to bigint for safety
                        }
                    }
                    "Field" | "Scalar" => "bigint".to_string(),
                    "Address" | "Group" | "Signature" | "String" => "string".to_string(),
                    _ => format!("unknown /* {} */", type_name),
                };
            }
        }
    }
    if let Some(struct_val) = ty.get("Struct") {
        if let Some(name) = struct_val.get("name").and_then(|n| n.as_str()) {
            return name.to_string();
        }
    }
    "unknown".to_string()
}

fn handle_audit(args: &AuditArgs, quiet: bool) -> Result<()> {
    // NOTE: This is a heuristic linter for hackathon-demo purposes.
    // It performs line-based static analysis and is NOT a formal verifier.
    // Real security audits require formal verification tools.

    let audit_path = Path::new(&args.path);
    if !audit_path.exists() {
        bail!("Audit path '{}' does not exist", audit_path.display());
    }

    // Determine the source directory to scan (prefer src/ subfolder if present)
    let src_dir = if audit_path.join("src").is_dir() {
        audit_path.join("src")
    } else {
        audit_path.to_path_buf()
    };

    // Sensitive identifiers to watch for
    // These are flagged when used outside a record (on-chain public data)
    // or when declared as 'public' fields inside a record.
    let sensitive_ids = ["password", "secret", "private_key", "ssn"];

    // Findings grouped by file path
    let mut grouped: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    let mut total_files = 0u32;

    for entry in WalkDir::new(&src_dir) {
        let entry = entry.with_context(|| {
            format!("Failed to read entry in '{}'", src_dir.display())
        })?;

        if !entry.file_type().is_file() {
            continue;
        }

        let path = entry.path();
        if path.extension().map_or(true, |ext| ext != "leo") {
            continue;
        }

        total_files += 1;
        let content = fs::read_to_string(path).with_context(|| {
            format!("Failed to read '{}'", path.display())
        })?;

        let lines: Vec<&str> = content.lines().collect();

        // Produce a display path relative to the scanned root
        let rel = path
            .strip_prefix(audit_path)
            .unwrap_or(path)
            .display()
            .to_string()
            .replace('\\', "/");

        let file_findings = grouped.entry(rel.clone()).or_default();

        // Simple state machine to track record block boundaries.
        // Only record types have private/public visibility in Leo.
        // Data outside a record is public on-chain.
        let mut record_active = false;
        let mut record_brace_depth: i32 = 0;

        let mut i = 0;
        while i < lines.len() {
            let trimmed = lines[i].trim();

            if !record_active {
                // Check if this line opens a record block
                if trimmed.starts_with("record ") && trimmed.contains('{') {
                    record_active = true;
                    record_brace_depth = trimmed.matches('{').count() as i32
                        - trimmed.matches('}').count() as i32;
                    if record_brace_depth <= 0 {
                        record_active = false;
                    }
                    i += 1;
                    continue;
                }

                // --- Outside any record block ---
                // Check 1: Sensitive identifiers outside a record.
                // In Aleo, data outside records is public on-chain.
                for id in &sensitive_ids {
                    if trimmed.contains(id) && !trimmed.starts_with("//") {
                        file_findings.push((
                            "[warning]".yellow().bold().to_string(),
                            format!(
                                "Line {}: '{}' appears outside a record — this data is \
                                 public on-chain. Wrap it in a private record if it should \
                                 be confidential.",
                                i + 1,
                                id
                            ),
                        ));
                    }
                }

                // Check 2: TODO / FIXME comments (informational)
                let upper = trimmed.to_uppercase();
                if upper.contains("TODO") || upper.contains("FIXME") {
                    file_findings.push((
                        "[info]".cyan().bold().to_string(),
                        format!(
                            "Line {}: Contains TODO/FIXME: '{}'",
                            i + 1,
                            trimmed
                        ),
                    ));
                }

                // Check 3: Functions returning address/numeric without access control
                // (heuristic: scan the function body for assert/require/assert_neq guards)
                if trimmed.starts_with("fn ") {
                    // Find the function signature end (the line with '{')
                    let mut sig_end = i;
                    let mut found_brace = false;
                    for j in i..lines.len() {
                        if lines[j].trim().contains('{') {
                            sig_end = j;
                            found_brace = true;
                            break;
                        }
                    }

                    if found_brace {
                        // Build the full signature
                        let sig: Vec<&str> = lines[i..=sig_end]
                            .iter()
                            .map(|l| l.trim())
                            .collect();
                        let sig_text = sig.join(" ");

                        let returns_address = sig_text.contains("> address");
                        let returns_numeric = sig_text.contains("> u");

                        // Scan the function body by counting braces to find its end.
                        // Track whether we find a guard (only matters for sensitive returns).
                        let mut brace_depth = 1u32;
                        let mut has_guard = false;
                        let mut k = sig_end + 1;

                        while k < lines.len() && brace_depth > 0 {
                            let bline = lines[k].trim();
                            brace_depth = (brace_depth as i32
                                + bline.matches('{').count() as i32
                                - bline.matches('}').count() as i32)
                                .max(0) as u32;

                            if (returns_address || returns_numeric)
                                && (bline.contains("assert") || bline.contains("require"))
                            {
                                has_guard = true;
                            }
                            k += 1;
                        }

                        // Report missing guard only for functions that return sensitive types
                        if (returns_address || returns_numeric) && !has_guard {
                            let return_type = if returns_address {
                                "address"
                            } else {
                                "numeric"
                            };
                            file_findings.push((
                                "[info]".cyan().bold().to_string(),
                                format!(
                                    "Line {}: Function returns {} value with no \
                                     assert/require guard found in function body. \
                                     Ensure access control is enforced.",
                                    i + 1,
                                    return_type
                                ),
                            ));
                        }

                        // Skip past the function body so its lines are not re-scanned
                        i = k;
                        continue;
                    }
                }

                i += 1;
            } else {
                // --- Inside a record block ---
                record_brace_depth += trimmed.matches('{').count() as i32;
                record_brace_depth -= trimmed.matches('}').count() as i32;

                if record_brace_depth <= 0 {
                    record_active = false;
                    i += 1;
                    continue;
                }

                // Check for public visibility on sensitive record fields
                // Leo's record syntax: {visibility} field_name: type,
                // Fields default to 'private' unless explicitly marked 'public'.
                if trimmed.contains("public ") {
                    for id in &sensitive_ids {
                        if trimmed.contains(id) {
                            file_findings.push((
                                "[warning]".yellow().bold().to_string(),
                                format!(
                                    "Line {}: Record field '{}' is declared 'public' and \
                                     may expose sensitive data on-chain. Consider omitting \
                                     the 'public' modifier (default is 'private').",
                                    i + 1,
                                    id
                                ),
                            ));
                        }
                    }
                }

                i += 1;
            }
        }
    }

    // --- Reporting ---
    if total_files == 0 {
        print_info(
            &format!("No .leo files found in '{}'", audit_path.display()),
            quiet,
        );
        return Ok(());
    }

    if grouped.is_empty() {
        println!(
            "{} No issues found in {} .leo file(s)",
            "[done]".green().bold(),
            total_files
        );
        return Ok(());
    }

    let mut total_warnings = 0usize;
    let mut total_infos = 0usize;

    for (file, findings) in &grouped {
        println!("\n  {}:", file);
        for (severity, message) in findings {
            if severity.contains("warning") {
                total_warnings += 1;
            } else {
                total_infos += 1;
            }
            println!("    {} {}", severity, message);
        }
    }

    println!();
    println!(
        "{} Found {} issue(s) in {} file(s) ({} warning(s), {} info)",
        "[done]".green().bold(),
        total_warnings + total_infos,
        grouped.len(),
        total_warnings,
        total_infos,
    );
    if !quiet {
        println!(
            "{} This is a heuristic linter for demonstration purposes, not a formal verifier.",
            "[info]".dimmed()
        );
    }

    Ok(())
}
