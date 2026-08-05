//! MCP (Model Context Protocol) server for AleoFlow.
//!
//! `aleoflow mcp` runs AleoFlow as a local MCP server over stdio so that AI
//! coding assistants (Claude Desktop, Claude Code, and other MCP clients)
//! can scaffold, build, test, audit, query, and deploy Aleo programs through
//! AleoFlow during a coding session.
//!
//! Each MCP tool maps 1:1 to an existing `aleoflow` CLI subcommand (the same
//! pattern as the official `mcp-server-git` reference implementation): the
//! handler re-invokes the currently running binary as a subprocess with the
//! corresponding CLI arguments, captures stdout/stderr, and returns the real
//! command output as MCP text content.
//!
//! # Safety model
//!
//! MCP has no built-in "requires confirmation" primitive, so this server
//! follows the established ecosystem convention of a two-step dry-run /
//! broadcast split. `aleoflow deploy` and `aleoflow execute` already default
//! to dry-run and only spend funds with an explicit `--broadcast` flag; the
//! MCP tools mirror that:
//!
//! - `aleoflow_deploy_dry_run` / `aleoflow_execute_dry_run` never pass
//!   `--broadcast`, so no funds can ever be spent through them.
//! - `aleoflow_deploy_broadcast` / `aleoflow_execute_broadcast` require a
//!   `confirm: true` argument AND are only registered when the server is
//!   started with `ALEOFLOW_MCP_ALLOW_BROADCAST=true`. When the variable is
//!   unset these tools are genuinely absent from the tool list, so a calling
//!   model cannot even attempt to spend funds.

use rmcp::{
    ErrorData as McpError,
    handler::server::{tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, ContentBlock},
    schemars, tool, tool_handler, tool_router, ServerHandler, ServiceExt,
    transport::stdio,
};
use serde::Deserialize;
use std::process::Stdio;
use tokio::process::Command as TokioCommand;

/// Environment variable that opts into registering the broadcast tools.
const ALLOW_BROADCAST_ENV: &str = "ALEOFLOW_MCP_ALLOW_BROADCAST";

/// Names of the tools that spend real funds and are gated behind the env var.
const BROADCAST_TOOLS: [&str; 2] = [
    "aleoflow_deploy_broadcast",
    "aleoflow_execute_broadcast",
];

/// Returns `true` only when `ALEOFLOW_MCP_ALLOW_BROADCAST=true` is set.
fn broadcast_allowed() -> bool {
    std::env::var(ALLOW_BROADCAST_ENV)
        .map(|v| v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Run the MCP server over stdio until the client closes the transport.
pub async fn run_server() -> anyhow::Result<()> {
    // stdout is reserved for the MCP protocol stream, so all diagnostics go
    // to stderr. One clear line about the broadcast gate at startup.
    if broadcast_allowed() {
        eprintln!(
            "[aleoflow-mcp] broadcast tools ENABLED (ALEOFLOW_MCP_ALLOW_BROADCAST=true): \
             aleoflow_deploy_broadcast and aleoflow_execute_broadcast are registered"
        );
    } else {
        eprintln!(
            "[aleoflow-mcp] broadcast tools DISABLED: set ALEOFLOW_MCP_ALLOW_BROADCAST=true \
             to enable the funds-spending tools aleoflow_deploy_broadcast and \
             aleoflow_execute_broadcast"
        );
    }

    let server = AleoflowMcp::new().serve(stdio()).await?;
    server.waiting().await?;
    Ok(())
}

/// Synchronous entry point used by the CLI (`aleoflow mcp`).
pub fn run() -> anyhow::Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(run_server())
}

// ---------------------------------------------------------------------------
// Self-invocation: run an `aleoflow` CLI subcommand as a subprocess of the
// currently running binary and capture its real output.
// ---------------------------------------------------------------------------

struct CommandOutput {
    stdout: String,
    stderr: String,
    code: i32,
}

/// Maximum time a wrapped AleoFlow subcommand may run. Deploy dry-runs and
/// queries can legitimately take minutes (leo compilation, network calls), so
/// this is generous; a hung process is killed rather than blocking the
/// assistant's session forever.
const SUBCOMMAND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);

async fn run_aleoflow_cli(args: &[String]) -> CommandOutput {
    let exe = std::env::current_exe().unwrap_or_else(|_| "aleoflow".into());
    let output = tokio::time::timeout(
        SUBCOMMAND_TIMEOUT,
        TokioCommand::new(exe)
            .args(args)
            // Never let the subprocess read from stdin: stdin belongs to the
            // MCP protocol stream and must not be consumed by a child command.
            .stdin(Stdio::null())
            // If the timeout fires, the dropped Command kills the child.
            .kill_on_drop(true)
            .output(),
    )
    .await;
    match output {
        Ok(Ok(o)) => CommandOutput {
            stdout: String::from_utf8_lossy(&o.stdout).trim().to_string(),
            stderr: String::from_utf8_lossy(&o.stderr).trim().to_string(),
            code: o.status.code().unwrap_or(-1),
        },
        Ok(Err(e)) => CommandOutput {
            stdout: String::new(),
            stderr: format!("failed to spawn aleoflow: {e}"),
            code: -1,
        },
        Err(_) => CommandOutput {
            stdout: String::new(),
            stderr: format!(
                "aleoflow subcommand timed out after {} seconds",
                SUBCOMMAND_TIMEOUT.as_secs()
            ),
            code: -1,
        },
    }
}

/// Build the MCP result from the captured command output. The real stdout is
/// returned verbatim (never summarized or reformatted); stderr is appended as
/// a separate content block when non-empty. Non-zero exit codes surface as a
/// tool-level error so the calling model sees the failure output.
fn command_result(cmd_line: &str, out: &CommandOutput) -> CallToolResult {
    let mut blocks: Vec<ContentBlock> = Vec::new();

    let mut main = format!("$ aleoflow {cmd_line}\nexit code: {}\n", out.code);
    if !out.stdout.is_empty() {
        main.push('\n');
        main.push_str(&out.stdout);
    }
    blocks.push(ContentBlock::text(main));

    if !out.stderr.is_empty() {
        blocks.push(ContentBlock::text(format!("[stderr]\n{}", out.stderr)));
    }

    if out.code == 0 {
        CallToolResult::success(blocks)
    } else {
        CallToolResult::error(blocks)
    }
}

/// Refusal message returned when a broadcast tool is called without the
/// explicit user opt-in at server start.
fn broadcast_disabled_message() -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(
        "Refused: broadcast tools are not enabled in this session. \
         The server must be started with ALEOFLOW_MCP_ALLOW_BROADCAST=true \
         for funds-spending tools to be available.",
    )])
}

// ---------------------------------------------------------------------------
// Shared argument types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
enum NetworkArg {
    Testnet,
    Mainnet,
    Canary,
}

impl NetworkArg {
    fn as_str(&self) -> &'static str {
        match self {
            NetworkArg::Testnet => "testnet",
            NetworkArg::Mainnet => "mainnet",
            NetworkArg::Canary => "canary",
        }
    }
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
enum TemplateArg {
    Payment,
    Defi,
    AiAgent,
    Gamefi,
    Token,
}

impl TemplateArg {
    fn as_str(&self) -> &'static str {
        match self {
            TemplateArg::Payment => "payment",
            TemplateArg::Defi => "defi",
            TemplateArg::AiAgent => "ai-agent",
            TemplateArg::Gamefi => "gamefi",
            TemplateArg::Token => "token",
        }
    }
}

/// `--json-output` accepts either a bare flag (`true`) or a custom file path
/// (`--json-output=<path>`).
#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
enum JsonOutputArg {
    Enabled(bool),
    Path(String),
}

impl JsonOutputArg {
    fn cli_flag(&self) -> Option<String> {
        match self {
            JsonOutputArg::Enabled(true) => Some("--json-output".to_string()),
            JsonOutputArg::Enabled(false) => None,
            JsonOutputArg::Path(p) => Some(format!("--json-output={p}")),
        }
    }
}

// Small argument-builders shared by the tool handlers.

fn push_flag(args: &mut Vec<String>, flag: &str, enabled: Option<bool>) {
    if enabled == Some(true) {
        args.push(flag.to_string());
    }
}

fn push_opt(args: &mut Vec<String>, flag: &str, value: Option<&str>) {
    if let Some(v) = value {
        args.push(flag.to_string());
        args.push(v.to_string());
    }
}

fn push_positional(args: &mut Vec<String>, value: Option<&str>) {
    if let Some(v) = value {
        args.push(v.to_string());
    }
}

fn push_network(args: &mut Vec<String>, network: &Option<NetworkArg>) {
    if let Some(n) = network {
        args.push("--network".to_string());
        args.push(n.as_str().to_string());
    }
}

fn push_json_output(args: &mut Vec<String>, json_output: &Option<JsonOutputArg>) {
    if let Some(j) = json_output {
        if let Some(flag) = j.cli_flag() {
            args.push(flag);
        }
    }
}

fn push_profile(args: &mut Vec<String>, profile: &Option<String>) {
    push_opt(args, "--profile", profile.as_deref());
}

// ---------------------------------------------------------------------------
// Tool parameter structs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
struct InitParams {
    /// Name of the new project (directory and program name)
    name: String,
    /// Project template: payment, defi, ai-agent, gamefi, or token
    /// (defaults to 'payment', or to aleo.toml's default_template)
    template: Option<TemplateArg>,
    /// Comma-separated workspace member names (e.g. "token,governance").
    /// When set, creates a workspace root and scaffolds each member.
    workspace: Option<String>,
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
struct PathParams {
    /// Path to the Aleo project directory (defaults to current dir)
    path: Option<String>,
    /// Write command results as JSON (pass true or a custom file path)
    json_output: Option<JsonOutputArg>,
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
struct AuditParams {
    /// Path to the Aleo project to audit (required)
    path: String,
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
struct BindingsParams {
    /// Path to the Aleo project directory (required unless --remote is set)
    path: Option<String>,
    /// Output path for the generated TypeScript file
    /// (defaults to <path>/bindings/<program_name>.ts)
    output: Option<String>,
    /// Remote program ID to generate bindings for (e.g. "credits.aleo").
    /// When set, fetches the compiled program from the network instead.
    remote: Option<String>,
    /// Network to use for fetching the remote program (defaults to testnet)
    network: Option<NetworkArg>,
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
enum QuerySubcommandArg {
    Block,
    Transaction,
    Program,
    Stateroot,
    Committee,
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
struct QueryParams {
    /// Which `aleoflow query` subcommand to run
    subcommand: QuerySubcommandArg,
    /// Block ID (height or hash); used by 'block' and 'transaction'
    id: Option<String>,
    /// Query the latest block
    latest: Option<bool>,
    /// Get the latest block hash only
    latest_hash: Option<bool>,
    /// Get the latest block height only
    latest_height: Option<bool>,
    /// Get consecutive blocks: [start, end] (max 50 per request)
    range: Option<Vec<String>>,
    /// Include transactions in block output
    transactions: Option<bool>,
    /// Include the cumulative height in output
    to_height: Option<bool>,
    /// Query confirmed transactions only
    confirmed: Option<bool>,
    /// Query unconfirmed transactions only
    unconfirmed: Option<bool>,
    /// Filter transactions by program IO ID
    from_io: Option<String>,
    /// Filter transactions by transition ID
    from_transition: Option<String>,
    /// Filter transactions by program name
    from_program: Option<String>,
    /// Deployed program name (e.g. "credits.aleo"); used by 'program'
    name: Option<String>,
    /// Program edition number
    edition: Option<u32>,
    /// List all mapping names of the program
    mappings: Option<bool>,
    /// Look up a mapping value: [mapping_name, key]
    mapping_value: Option<Vec<String>>,
    /// Target network: testnet, mainnet, or canary (defaults to testnet)
    network: Option<NetworkArg>,
    /// Aleo network endpoint URL
    endpoint: Option<String>,
    /// Write command results as JSON (pass true or a custom file path)
    json_output: Option<JsonOutputArg>,
    /// Named environment profile from aleo.toml (sets network/endpoint)
    profile: Option<String>,
}

impl QueryParams {
    /// Build the full `aleoflow query ...` argument list for this request.
    fn to_cli_args(&self) -> Vec<String> {
        let sub = match self.subcommand {
            QuerySubcommandArg::Block => "block",
            QuerySubcommandArg::Transaction => "transaction",
            QuerySubcommandArg::Program => "program",
            QuerySubcommandArg::Stateroot => "stateroot",
            QuerySubcommandArg::Committee => "committee",
        };
        let mut args = vec!["query".to_string(), sub.to_string()];

        match self.subcommand {
            QuerySubcommandArg::Block => {
                push_positional(&mut args, self.id.as_deref());
                push_flag(&mut args, "--latest", self.latest);
                push_flag(&mut args, "--latest-hash", self.latest_hash);
                push_flag(&mut args, "--latest-height", self.latest_height);
                push_flag(&mut args, "--transactions", self.transactions);
                push_flag(&mut args, "--to-height", self.to_height);
                if let Some(range) = &self.range {
                    args.push("--range".to_string());
                    args.extend(range.iter().cloned());
                }
            }
            QuerySubcommandArg::Transaction => {
                push_positional(&mut args, self.id.as_deref());
                push_flag(&mut args, "--confirmed", self.confirmed);
                push_flag(&mut args, "--unconfirmed", self.unconfirmed);
                push_opt(&mut args, "--from-io", self.from_io.as_deref());
                push_opt(&mut args, "--from-transition", self.from_transition.as_deref());
                push_opt(&mut args, "--from-program", self.from_program.as_deref());
            }
            QuerySubcommandArg::Program => {
                push_positional(&mut args, self.name.as_deref());
                if let Some(edition) = self.edition {
                    args.push("--edition".to_string());
                    args.push(edition.to_string());
                }
                push_flag(&mut args, "--mappings", self.mappings);
                if let Some(mv) = &self.mapping_value {
                    args.push("--mapping-value".to_string());
                    args.extend(mv.iter().cloned());
                }
            }
            QuerySubcommandArg::Stateroot | QuerySubcommandArg::Committee => {}
        }

        push_network(&mut args, &self.network);
        push_opt(&mut args, "--endpoint", self.endpoint.as_deref());
        push_json_output(&mut args, &self.json_output);
        push_profile(&mut args, &self.profile);
        args
    }
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
struct RunParams {
    /// Name of the transition/function to run (defaults to "main")
    name: Option<String>,
    /// Input arguments as raw Leo literal strings (e.g. "1u32", "aleo1...")
    #[serde(default)]
    inputs: Vec<String>,
    /// Path to the Aleo project directory (defaults to current dir)
    path: Option<String>,
    /// Target network: testnet, mainnet, or canary
    network: Option<NetworkArg>,
    /// Aleo network endpoint URL
    endpoint: Option<String>,
    /// Write command results as JSON (pass true or a custom file path)
    json_output: Option<JsonOutputArg>,
    /// Named environment profile from aleo.toml (sets network/endpoint)
    profile: Option<String>,
}

impl RunParams {
    /// Build the trailing `aleoflow <run|execute> <name> [inputs...]` args.
    fn to_cli_args(&self, subcommand: &str) -> Vec<String> {
        let mut args = vec![subcommand.to_string()];
        args.push(self.name.clone().unwrap_or_else(|| "main".to_string()));
        args.extend(self.inputs.iter().cloned());
        push_opt(&mut args, "--path", self.path.as_deref());
        push_network(&mut args, &self.network);
        push_opt(&mut args, "--endpoint", self.endpoint.as_deref());
        push_json_output(&mut args, &self.json_output);
        push_profile(&mut args, &self.profile);
        args
    }
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
struct ExecuteParams {
    /// Name of the transition/function to execute (defaults to "main")
    name: Option<String>,
    /// Input arguments as raw Leo literal strings (e.g. "1u32", "aleo1...")
    #[serde(default)]
    inputs: Vec<String>,
    /// Path to the Aleo project directory (defaults to current dir)
    path: Option<String>,
    /// Target network: testnet, mainnet, or canary
    network: Option<NetworkArg>,
    /// Aleo network endpoint URL
    endpoint: Option<String>,
    /// Write command results as JSON (pass true or a custom file path)
    json_output: Option<JsonOutputArg>,
    /// Named environment profile from aleo.toml (sets network/endpoint)
    profile: Option<String>,
}

impl ExecuteParams {
    /// Build the trailing `aleoflow execute ...` args. `broadcast` controls
    /// whether the funds-spending `--broadcast` flag is appended.
    fn to_cli_args(&self, broadcast: bool) -> Vec<String> {
        let mut args = vec!["execute".to_string()];
        args.push(self.name.clone().unwrap_or_else(|| "main".to_string()));
        args.extend(self.inputs.iter().cloned());
        push_opt(&mut args, "--path", self.path.as_deref());
        push_network(&mut args, &self.network);
        push_opt(&mut args, "--endpoint", self.endpoint.as_deref());
        push_json_output(&mut args, &self.json_output);
        push_profile(&mut args, &self.profile);
        if broadcast {
            args.push("--broadcast".to_string());
        }
        args
    }
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
struct DeployParams {
    /// Path to the Leo program root folder (defaults to current dir)
    path: Option<String>,
    /// Target network: testnet, mainnet, or canary (defaults to testnet)
    network: Option<NetworkArg>,
    /// Aleo network endpoint URL
    endpoint: Option<String>,
    /// Target a specific workspace member by name (requires workspace root)
    package: Option<String>,
    /// Deploy all workspace members sequentially (requires workspace root)
    all: Option<bool>,
    /// Write command results as JSON (pass true or a custom file path)
    json_output: Option<JsonOutputArg>,
    /// Named environment profile from aleo.toml (sets network/endpoint)
    profile: Option<String>,
}

impl DeployParams {
    /// Build the trailing `aleoflow deploy ...` args. `broadcast` controls
    /// whether the funds-spending `--broadcast` flag is appended.
    fn to_cli_args(&self, broadcast: bool) -> Vec<String> {
        let mut args = vec!["deploy".to_string()];
        push_opt(&mut args, "--path", self.path.as_deref());
        push_network(&mut args, &self.network);
        push_opt(&mut args, "--endpoint", self.endpoint.as_deref());
        push_opt(&mut args, "--package", self.package.as_deref());
        push_flag(&mut args, "--all", self.all);
        push_json_output(&mut args, &self.json_output);
        push_profile(&mut args, &self.profile);
        if broadcast {
            args.push("--broadcast".to_string());
        }
        args
    }
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
struct BroadcastDeployParams {
    /// Explicit user acknowledgment that real funds may be spent. Must be true.
    confirm: bool,
    /// Path to the Leo program root folder (defaults to current dir)
    path: Option<String>,
    /// Target network: testnet, mainnet, or canary (defaults to testnet)
    network: Option<NetworkArg>,
    /// Aleo network endpoint URL
    endpoint: Option<String>,
    /// Target a specific workspace member by name (requires workspace root)
    package: Option<String>,
    /// Deploy all workspace members sequentially (requires workspace root)
    all: Option<bool>,
    /// Write command results as JSON (pass true or a custom file path)
    json_output: Option<JsonOutputArg>,
    /// Named environment profile from aleo.toml (sets network/endpoint)
    profile: Option<String>,
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
struct BroadcastExecuteParams {
    /// Explicit user acknowledgment that real funds may be spent. Must be true.
    confirm: bool,
    /// Name of the transition/function to execute (defaults to "main")
    name: Option<String>,
    /// Input arguments as raw Leo literal strings (e.g. "1u32", "aleo1...")
    #[serde(default)]
    inputs: Vec<String>,
    /// Path to the Aleo project directory (defaults to current dir)
    path: Option<String>,
    /// Target network: testnet, mainnet, or canary
    network: Option<NetworkArg>,
    /// Aleo network endpoint URL
    endpoint: Option<String>,
    /// Write command results as JSON (pass true or a custom file path)
    json_output: Option<JsonOutputArg>,
    /// Named environment profile from aleo.toml (sets network/endpoint)
    profile: Option<String>,
}

// ---------------------------------------------------------------------------
// The MCP server: tools-only server using a ToolRouter field so broadcast
// tools can be disabled at startup based on ALEOFLOW_MCP_ALLOW_BROADCAST.
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct AleoflowMcp {
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl AleoflowMcp {
    /// Scaffold a new Aleo project from a template.
    /// Maps to: `aleoflow init <name> [--template <template>] [--workspace <members>]`.
    /// Safe: creates local files only.
    #[tool(description = "Scaffold a new Aleo project from a template (local file creation only). Maps to: aleoflow init <name> [--template <template>] [--workspace <members>]. Templates: payment (default), defi, ai-agent, gamefi, token. With --workspace, creates a multi-member workspace root.")]
    async fn aleoflow_init(
        &self,
        Parameters(p): Parameters<InitParams>,
    ) -> Result<CallToolResult, McpError> {
        let mut args = vec!["init".to_string(), p.name.clone()];
        if let Some(t) = &p.template {
            args.push("--template".to_string());
            args.push(t.as_str().to_string());
        }
        push_opt(&mut args, "--workspace", p.workspace.as_deref());
        let out = run_aleoflow_cli(&args).await;
        Ok(command_result(&args.join(" "), &out))
    }

    /// Compile the current Aleo project.
    /// Maps to: `aleoflow build [--path <path>] [--json-output[=<file>]]`.
    #[tool(description = "Compile the Aleo project with leo (local build only, no network interaction). Maps to: aleoflow build [--path <path>] [--json-output[=<file>]]. Pass --path to target a project directory other than the current one.")]
    async fn aleoflow_build(
        &self,
        Parameters(p): Parameters<PathParams>,
    ) -> Result<CallToolResult, McpError> {
        let mut args = vec!["build".to_string()];
        push_opt(&mut args, "--path", p.path.as_deref());
        push_json_output(&mut args, &p.json_output);
        let out = run_aleoflow_cli(&args).await;
        Ok(command_result(&args.join(" "), &out))
    }

    /// Run project tests.
    /// Maps to: `aleoflow test [--path <path>] [--json-output[=<file>]]`.
    #[tool(description = "Run the Aleo project's test suite with leo (local only). Maps to: aleoflow test [--path <path>] [--json-output[=<file>]]. Pass --path to target a project directory other than the current one.")]
    async fn aleoflow_test(
        &self,
        Parameters(p): Parameters<PathParams>,
    ) -> Result<CallToolResult, McpError> {
        let mut args = vec!["test".to_string()];
        push_opt(&mut args, "--path", p.path.as_deref());
        push_json_output(&mut args, &p.json_output);
        let out = run_aleoflow_cli(&args).await;
        Ok(command_result(&args.join(" "), &out))
    }

    /// Run a security audit on an Aleo project.
    /// Maps to: `aleoflow audit <path>`.
    #[tool(description = "Run AleoFlow's privacy/security audit on an Aleo project (static local analysis only). Maps to: aleoflow audit <path>, where path is the project directory to audit.")]
    async fn aleoflow_audit(
        &self,
        Parameters(p): Parameters<AuditParams>,
    ) -> Result<CallToolResult, McpError> {
        let args = vec!["audit".to_string(), p.path.clone()];
        let out = run_aleoflow_cli(&args).await;
        Ok(command_result(&args.join(" "), &out))
    }

    /// Query Aleo network state.
    /// Maps to: `aleoflow query <subcommand>`.
    #[tool(description = "Query Aleo network state (read-only). Maps to: aleoflow query <subcommand>. Subcommands: block, transaction, program, stateroot, committee. Relevant args per subcommand: block (id or --latest/--latest-hash/--latest-height/--range [start,end]/--transactions/--to-height), transaction (id or --confirmed/--unconfirmed/--from-io/--from-transition/--from-program), program (name, --edition/--mappings/--mapping-value [name,key]), stateroot, committee. Optional --network (testnet/mainnet/canary), --endpoint, and --json-output apply to all.")]
    async fn aleoflow_query(
        &self,
        Parameters(p): Parameters<QueryParams>,
    ) -> Result<CallToolResult, McpError> {
        let args = p.to_cli_args();
        let out = run_aleoflow_cli(&args).await;
        Ok(command_result(&args.join(" "), &out))
    }

    /// Diagnose the local Aleo development environment.
    /// Maps to: `aleoflow doctor`.
    #[tool(description = "Run AleoFlow's environment diagnostics (checks rustc, cargo, leo, snarkos, leo-fmt, env vars, git). Read-only. Maps to: aleoflow doctor. Never exposes secret values, only whether variables are set.")]
    async fn aleoflow_doctor(&self) -> Result<CallToolResult, McpError> {
        let args = vec!["doctor".to_string()];
        let out = run_aleoflow_cli(&args).await;
        Ok(command_result(&args.join(" "), &out))
    }

    /// Generate TypeScript bindings from a compiled Aleo program's ABI.
    /// Maps to: `aleoflow bindings [path] [--output <file>] [--remote <id>] [--network <network>]`.
    #[tool(description = "Generate TypeScript bindings from a compiled Aleo program's ABI. Maps to: aleoflow bindings [path] [--output <file>] [--remote <program_id>] [--network <network>]. Use a local project path, or --remote <program_id> to fetch a deployed program (e.g. credits.aleo) from the network.")]
    async fn aleoflow_bindings(
        &self,
        Parameters(p): Parameters<BindingsParams>,
    ) -> Result<CallToolResult, McpError> {
        let mut args = vec!["bindings".to_string()];
        push_positional(&mut args, p.path.as_deref());
        push_opt(&mut args, "--output", p.output.as_deref());
        push_opt(&mut args, "--remote", p.remote.as_deref());
        push_network(&mut args, &p.network);
        let out = run_aleoflow_cli(&args).await;
        Ok(command_result(&args.join(" "), &out))
    }

    /// Locally execute a transition/function (dry-run, no transaction sent).
    /// Maps to: `aleoflow run <name> [inputs...]`.
    /// Safe: purely local execution, no funds are ever spent.
    #[tool(description = "Locally execute an Aleo transition/function. Maps to: aleoflow run <name> [inputs...] [--path <path>] [--network <network>] [--endpoint <url>]. Purely local execution (leo run): no transaction is sent and no funds are ever spent. Inputs are raw Leo literals (e.g. \"1u32\", \"aleo1...\").")]
    async fn aleoflow_run(
        &self,
        Parameters(p): Parameters<RunParams>,
    ) -> Result<CallToolResult, McpError> {
        let args = p.to_cli_args("run");
        let out = run_aleoflow_cli(&args).await;
        Ok(command_result(&args.join(" "), &out))
    }

    /// Execute a transition/function on-chain (dry-run, no funds spent).
    /// Maps to: `aleoflow execute <name> [inputs...]` WITHOUT --broadcast.
    #[tool(description = "Execute an Aleo transition/function in DRY-RUN mode. Maps to: aleoflow execute <name> [inputs...] [--path <path>] [--network <network>] [--endpoint <url>], without the --broadcast flag: leo simulates the execution and reports the would-be result and fee, but NO transaction is broadcast and NO funds are spent. If the user wants to actually spend funds, show them this dry-run output first, then use aleoflow_execute_broadcast only after they explicitly confirm.")]
    async fn aleoflow_execute_dry_run(
        &self,
        Parameters(p): Parameters<ExecuteParams>,
    ) -> Result<CallToolResult, McpError> {
        let args = p.to_cli_args(false);
        let out = run_aleoflow_cli(&args).await;
        Ok(command_result(&args.join(" "), &out))
    }

    /// Deploy a compiled program (dry-run, no funds spent).
    /// Maps to: `aleoflow deploy` WITHOUT --broadcast.
    #[tool(description = "Deploy a compiled Aleo program in DRY-RUN mode. Maps to: aleoflow deploy [--path <path>] [--network <network>] [--endpoint <url>] [--package <name> | --all], without the --broadcast flag: leo compiles and reports the deployment plan and cost estimate, but NO transaction is broadcast and NO funds are spent. If the user wants to actually deploy, show them this dry-run output first, then use aleoflow_deploy_broadcast only after they explicitly confirm. Private keys are never passed as arguments; the key is read from the PRIVATE_KEY environment variable or the project's .env file.")]
    async fn aleoflow_deploy_dry_run(
        &self,
        Parameters(p): Parameters<DeployParams>,
    ) -> Result<CallToolResult, McpError> {
        let args = p.to_cli_args(false);
        let out = run_aleoflow_cli(&args).await;
        Ok(command_result(&args.join(" "), &out))
    }

    /// Deploy a compiled program and BROADCAST the transaction (spends funds).
    /// Maps to: `aleoflow deploy --broadcast`. Requires confirm=true.
    /// Only registered when ALEOFLOW_MCP_ALLOW_BROADCAST=true.
    #[tool(description = "Deploy a compiled Aleo program and BROADCAST the deployment transaction, spending REAL funds (network fees + 10 credits deposit). Maps to: aleoflow deploy --broadcast. Do not call this tool unless the user has explicitly reviewed the dry-run output and confirmed they want to spend real funds. Always call aleoflow_deploy_dry_run first and show the user the result before calling this. The confirm parameter MUST be set to true, which is your acknowledgment that the user has explicitly approved spending funds. Private keys are never passed as arguments; the key is read from the PRIVATE_KEY environment variable or the project's .env file.")]
    async fn aleoflow_deploy_broadcast(
        &self,
        Parameters(p): Parameters<BroadcastDeployParams>,
    ) -> Result<CallToolResult, McpError> {
        if !p.confirm {
            return Ok(CallToolResult::error(vec![ContentBlock::text(
                "Refused: this tool spends real funds and requires confirm=true. \
                 Call aleoflow_deploy_dry_run first, show the user its output, and \
                 only retry after they explicitly approve spending funds.",
            )]));
        }
        if !broadcast_allowed() {
            return Ok(broadcast_disabled_message());
        }
        let args = DeployParams {
            path: p.path.clone(),
            network: p.network.clone(),
            endpoint: p.endpoint.clone(),
            package: p.package.clone(),
            all: p.all,
            json_output: p.json_output.clone(),
            profile: p.profile.clone(),
        }
        .to_cli_args(true);
        let out = run_aleoflow_cli(&args).await;
        Ok(command_result(&args.join(" "), &out))
    }

    /// Execute a transition/function on-chain and BROADCAST (spends funds).
    /// Maps to: `aleoflow execute --broadcast`. Requires confirm=true.
    /// Only registered when ALEOFLOW_MCP_ALLOW_BROADCAST=true.
    #[tool(description = "Execute an Aleo transition/function and BROADCAST the execution transaction, spending REAL funds (network fees, and the executed program's own costs). Maps to: aleoflow execute <name> [inputs...] --broadcast. Do not call this tool unless the user has explicitly reviewed the dry-run output and confirmed they want to spend real funds. Always call aleoflow_execute_dry_run first and show the user the result before calling this. The confirm parameter MUST be set to true, which is your acknowledgment that the user has explicitly approved spending funds. Private keys are never passed as arguments; the key is read from the PRIVATE_KEY environment variable or the project's .env file.")]
    async fn aleoflow_execute_broadcast(
        &self,
        Parameters(p): Parameters<BroadcastExecuteParams>,
    ) -> Result<CallToolResult, McpError> {
        if !p.confirm {
            return Ok(CallToolResult::error(vec![ContentBlock::text(
                "Refused: this tool spends real funds and requires confirm=true. \
                 Call aleoflow_execute_dry_run first, show the user its output, and \
                 only retry after they explicitly approve spending funds.",
            )]));
        }
        if !broadcast_allowed() {
            return Ok(broadcast_disabled_message());
        }
        let args = ExecuteParams {
            name: p.name.clone(),
            inputs: p.inputs.clone(),
            path: p.path.clone(),
            network: p.network.clone(),
            endpoint: p.endpoint.clone(),
            json_output: p.json_output.clone(),
            profile: p.profile.clone(),
        }
        .to_cli_args(true);
        let out = run_aleoflow_cli(&args).await;
        Ok(command_result(&args.join(" "), &out))
    }
}

impl AleoflowMcp {
    /// Construct the server, registering all tools but hiding the broadcast
    /// tools unless ALEOFLOW_MCP_ALLOW_BROADCAST=true is set.
    fn new() -> Self {
        let mut router = Self::tool_router();
        if !broadcast_allowed() {
            for name in BROADCAST_TOOLS {
                router.disable_route(name);
            }
        }
        Self { tool_router: router }
    }
}

#[tool_handler(
    router = self.tool_router,
    name = "aleoflow",
    instructions = "AleoFlow MCP server: scaffold, build, test, audit, query, and deploy Aleo programs. \
        Safe and dry-run tools never spend funds. The broadcast tools \
        (aleoflow_deploy_broadcast, aleoflow_execute_broadcast) spend REAL funds: \
        always run the matching dry-run tool first, show the user its output, and only \
        call a broadcast tool after the user explicitly confirms. Broadcast tools are \
        only available when the server was started with ALEOFLOW_MCP_ALLOW_BROADCAST=true. \
        Private keys are never accepted as tool arguments; they come from the PRIVATE_KEY \
        environment variable or the project's .env file."
)]
impl ServerHandler for AleoflowMcp {}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Safety invariant: dry-run arg builders must never emit --broadcast
    // -----------------------------------------------------------------------

    fn deploy_params() -> DeployParams {
        DeployParams {
            path: Some(".".to_string()),
            network: Some(NetworkArg::Testnet),
            endpoint: Some("https://api.example.com".to_string()),
            package: None,
            all: None,
            json_output: None,
            profile: None,
        }
    }

    fn execute_params() -> ExecuteParams {
        ExecuteParams {
            name: Some("transfer".to_string()),
            inputs: vec!["1u64".to_string(), "aleo1abc".to_string()],
            path: None,
            network: None,
            endpoint: None,
            json_output: None,
            profile: None,
        }
    }

    #[test]
    fn test_deploy_dry_run_never_emits_broadcast() {
        let args = deploy_params().to_cli_args(false);
        assert_eq!(args.first().map(String::as_str), Some("deploy"));
        assert!(
            !args.iter().any(|a| a == "--broadcast"),
            "dry-run deploy must not pass --broadcast: {args:?}"
        );
    }

    #[test]
    fn test_deploy_broadcast_emits_broadcast() {
        let args = deploy_params().to_cli_args(true);
        assert!(
            args.iter().any(|a| a == "--broadcast"),
            "broadcast deploy must pass --broadcast: {args:?}"
        );
    }

    #[test]
    fn test_execute_dry_run_never_emits_broadcast() {
        let args = execute_params().to_cli_args(false);
        assert_eq!(args.first().map(String::as_str), Some("execute"));
        assert_eq!(args[1], "transfer");
        assert!(
            !args.iter().any(|a| a == "--broadcast"),
            "dry-run execute must not pass --broadcast: {args:?}"
        );
    }

    #[test]
    fn test_execute_broadcast_emits_broadcast() {
        let args = execute_params().to_cli_args(true);
        assert!(
            args.iter().any(|a| a == "--broadcast"),
            "broadcast execute must pass --broadcast: {args:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Arg builder mapping
    // -----------------------------------------------------------------------

    #[test]
    fn test_run_args_name_defaults_to_main() {
        let p = RunParams {
            name: None,
            inputs: vec![],
            path: None,
            network: None,
            endpoint: None,
            json_output: None,
            profile: None,
        };
        assert_eq!(p.to_cli_args("run"), vec!["run", "main"]);
    }

    #[test]
    fn test_run_args_with_inputs_and_network() {
        let p = RunParams {
            name: Some("submit".to_string()),
            inputs: vec!["100u64".to_string()],
            path: None,
            network: Some(NetworkArg::Mainnet),
            endpoint: Some("https://api.provable.com/v2".to_string()),
            json_output: None,
            profile: None,
        };
        assert_eq!(
            p.to_cli_args("run"),
            vec![
                "run",
                "submit",
                "100u64",
                "--network",
                "mainnet",
                "--endpoint",
                "https://api.provable.com/v2",
            ]
        );
    }

    #[test]
    fn test_query_block_flags_mapping() {
        let p = QueryParams {
            subcommand: QuerySubcommandArg::Block,
            id: Some("100".to_string()),
            latest: Some(true),
            latest_hash: Some(false),
            latest_height: None,
            range: Some(vec!["100".to_string(), "120".to_string()]),
            transactions: Some(true),
            to_height: None,
            confirmed: None,
            unconfirmed: None,
            from_io: None,
            from_transition: None,
            from_program: None,
            name: None,
            edition: None,
            mappings: None,
            mapping_value: None,
            network: Some(NetworkArg::Testnet),
            endpoint: None,
            json_output: None,
            profile: None,
        };
        let args = p.to_cli_args();
        assert_eq!(
            args,
            vec![
                "query",
                "block",
                "100",
                "--latest",
                "--transactions",
                "--range",
                "100",
                "120",
                "--network",
                "testnet",
            ]
        );
        assert!(!args.contains(&"--latest-hash".to_string()));
        assert!(!args.contains(&"--latest-height".to_string()));
    }

    #[test]
    fn test_query_program_mapping_value() {
        let p = QueryParams {
            subcommand: QuerySubcommandArg::Program,
            id: None,
            latest: None,
            latest_hash: None,
            latest_height: None,
            range: None,
            transactions: None,
            to_height: None,
            confirmed: None,
            unconfirmed: None,
            from_io: None,
            from_transition: None,
            from_program: None,
            name: Some("credits.aleo".to_string()),
            edition: Some(2),
            mappings: None,
            mapping_value: Some(vec!["account".to_string(), "aleo1abc".to_string()]),
            network: None,
            endpoint: None,
            json_output: None,
            profile: None,
        };
        assert_eq!(
            p.to_cli_args(),
            vec![
                "query",
                "program",
                "credits.aleo",
                "--edition",
                "2",
                "--mapping-value",
                "account",
                "aleo1abc",
            ]
        );
    }

    #[test]
    fn test_json_output_flag_mapping() {
        assert_eq!(
            JsonOutputArg::Enabled(true).cli_flag(),
            Some("--json-output".to_string())
        );
        assert_eq!(JsonOutputArg::Enabled(false).cli_flag(), None);
        assert_eq!(
            JsonOutputArg::Path("out.json".to_string()).cli_flag(),
            Some("--json-output=out.json".to_string())
        );
    }

    #[test]
    fn test_network_and_template_arg_names() {
        assert_eq!(NetworkArg::Testnet.as_str(), "testnet");
        assert_eq!(NetworkArg::Mainnet.as_str(), "mainnet");
        assert_eq!(NetworkArg::Canary.as_str(), "canary");
        assert_eq!(TemplateArg::Payment.as_str(), "payment");
        assert_eq!(TemplateArg::AiAgent.as_str(), "ai-agent");
        assert_eq!(TemplateArg::Gamefi.as_str(), "gamefi");
        assert_eq!(TemplateArg::Token.as_str(), "token");
        assert_eq!(TemplateArg::Defi.as_str(), "defi");
    }
}
