mod leo_cmd;
mod mcp;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, Args};
use colored::*;
use serde::Deserialize;

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

// ---------------------------------------------------------------------------
// Remote bindings: structs for parsed .aleo assembly
// ---------------------------------------------------------------------------

/// A single parameter extracted from .aleo assembly
#[allow(dead_code)]
struct AleoParam {
    /// The Leo type string (e.g. "u64", "address", "credits.record")
    param_type: String,
    /// "public" or "private"
    visibility: String,
}

/// A function extracted from .aleo assembly
struct AleoFunction {
    name: String,
    inputs: Vec<AleoParam>,
}

/// Parameter info for the shared TS generation template
struct FuncParam {
    name: String,
    ts_type: String,
    converter_expr: String,
    is_record: bool,
}

/// Function info for the shared TS generation template
struct FuncInfo {
    name: String,
    params: Vec<FuncParam>,
}

// ---------------------------------------------------------------------------
// Optional aleo.toml config file support
// ---------------------------------------------------------------------------

/// A single profile entry in aleo.toml's [profiles] table.
/// NEVER store private keys here. Use .env files or shell env vars.
#[derive(Deserialize, Clone, Default)]
struct ProfileConfig {
    endpoint: Option<String>,
    network: Option<String>,
}

/// Configuration loaded from aleo.toml (optional, in current working directory).
/// Falls back gracefully if the file is missing or malformed.
//
// Precedence order (most to least specific):
//   explicit CLI --network/--endpoint flags
//   > --profile <name> values from aleo.toml's [profiles.<name>]
//   > aleo.toml's default_network
//   > built-in hardcoded defaults
#[derive(Deserialize, Default)]
struct AleoFlowConfig {
    #[serde(default)]
    default_network: Option<String>,
    #[serde(default)]
    default_template: Option<String>,
    /// Named environment profiles: [profiles.<name>] with endpoint/network.
    #[serde(default)]
    profiles: Option<HashMap<String, ProfileConfig>>,
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

/// Resolved values from an optional --profile flag.
struct ProfileResolution {
    network: Option<Network>,
    endpoint: Option<String>,
}

/// Look up a named profile from aleo.toml and return its network/endpoint values.
/// Errors if the profile name does not exist in the config, listing available ones.
fn resolve_profile(
    profile_name: Option<&str>,
    cfg: &AleoFlowConfig,
    quiet: bool,
) -> Result<ProfileResolution> {
    let pname = match profile_name {
        None => return Ok(ProfileResolution { network: None, endpoint: None }),
        Some(n) => n,
    };

    let profiles = cfg.profiles.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "Profile '{}' is not defined in aleo.toml (no [profiles] section). \
             Define profiles in aleo.toml like:\n  \
             [profiles.{}]\n  \
             endpoint = \"<url>\"\n  \
             network = \"<testnet|mainnet|canary>\"",
            pname, pname
        )
    })?;

    let profile = profiles.get(pname).ok_or_else(|| {
        let mut available: Vec<&str> = profiles.keys().map(|s| s.as_str()).collect();
        available.sort();
        anyhow::anyhow!(
            "Profile '{}' not found in aleo.toml. Available profiles: {}",
            pname,
            available.join(", ")
        )
    })?;

    let network = profile.network.as_deref().and_then(parse_network);
    let endpoint = profile.endpoint.clone();

    if !quiet {
        let net_display = network
            .as_ref()
            .map(|n| match n {
                Network::Testnet => "testnet",
                Network::Mainnet => "mainnet",
                Network::Canary => "canary",
            })
            .unwrap_or("(unspecified)");
        let ep_display = endpoint.as_deref().unwrap_or("(unspecified)");
        println!(
            "{} Using profile '{}': network={}, endpoint={}",
            "[info]".cyan().bold(),
            pname,
            net_display,
            ep_display
        );
    }

    Ok(ProfileResolution { network, endpoint })
}
fn parse_template(s: &str) -> Option<Template> {
    match s.to_lowercase().as_str() {
        "payment" => Some(Template::Payment),
        "defi" => Some(Template::Defi),
        "ai-agent" => Some(Template::AiAgent),
        "gamefi" => Some(Template::Gamefi),
        "token" => Some(Template::Token),
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
            EmbeddedFile { rel_path: "tests/test_program.leo", contents: include_str!("../templates/payment/tests/test_program.leo") },
        ],
    },
    EmbeddedTemplate {
        name: "defi",
        files: &[
            EmbeddedFile { rel_path: "program.json",   contents: include_str!("../templates/defi/program.json") },
            EmbeddedFile { rel_path: "src/main.leo",   contents: include_str!("../templates/defi/src/main.leo") },
            EmbeddedFile { rel_path: "README.md",      contents: include_str!("../templates/defi/README.md") },
            EmbeddedFile { rel_path: "tests/test_program.leo", contents: include_str!("../templates/defi/tests/test_program.leo") },
        ],
    },
    EmbeddedTemplate {
        name: "ai-agent",
        files: &[
            EmbeddedFile { rel_path: "program.json",   contents: include_str!("../templates/ai-agent/program.json") },
            EmbeddedFile { rel_path: "src/main.leo",   contents: include_str!("../templates/ai-agent/src/main.leo") },
            EmbeddedFile { rel_path: "README.md",      contents: include_str!("../templates/ai-agent/README.md") },
            EmbeddedFile { rel_path: "tests/test_program.leo", contents: include_str!("../templates/ai-agent/tests/test_program.leo") },
        ],
    },
    EmbeddedTemplate {
        name: "gamefi",
        files: &[
            EmbeddedFile { rel_path: "program.json",   contents: include_str!("../templates/gamefi/program.json") },
            EmbeddedFile { rel_path: "src/main.leo",   contents: include_str!("../templates/gamefi/src/main.leo") },
            EmbeddedFile { rel_path: "README.md",      contents: include_str!("../templates/gamefi/README.md") },
            EmbeddedFile { rel_path: "tests/test_program.leo", contents: include_str!("../templates/gamefi/tests/test_program.leo") },
        ],
    },
    EmbeddedTemplate {
        name: "token",
        files: &[
            EmbeddedFile { rel_path: "program.json",   contents: include_str!("../templates/token/program.json") },
            EmbeddedFile { rel_path: "src/main.leo",   contents: include_str!("../templates/token/src/main.leo") },
            EmbeddedFile { rel_path: "README.md",      contents: include_str!("../templates/token/README.md") },
            EmbeddedFile { rel_path: "tests/test_program.leo", contents: include_str!("../templates/token/tests/test_program.leo") },
        ],
    },
];

/// Look up an embedded template by its CLI name.
fn find_template(name: &str) -> Option<&'static EmbeddedTemplate> {
    TEMPLATES.iter().find(|t| t.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    // NOTE: The following existing functions are tightly coupled to file I/O or
    // CLI arg parsing and are not unit-tested here:
    //   - handle_audit      (WalkDir, fs::read_to_string, CLI args)
    //   - handle_init       (directory creation, template file copy)
    //   - handle_bindings   (file reads, leo binary subprocess)
    //   - handle_build/test/deploy/devnet (shell out to leo binary)
    //   - find_transition_signatures (indirectly exercised by taint tests;
    //     a dedicated inline test would require parsing a multi-line body)

    // -----------------------------------------------------------------------
    // Audit: parse_record_declarations
    // -----------------------------------------------------------------------

    #[test]
    fn test_parse_record_declarations_with_public_private_fields() {
        let leo_source = r#"program test.aleo;

    record MyRecord {
        owner: address,
        gates: u64,
        public balance: u64,
        data: u64,
    }

    function main:
        input r0 as u32.public;
        input r1 as u32.private;
        add r0 r1 into r2;
        output r2 as u32.private;
"#;
        let lines: Vec<&str> = leo_source.lines().collect();
        let records = parse_record_declarations(&lines);

        // Should find exactly one record with 4 fields
        assert_eq!(records.len(), 1);
        let fields = records.get("MyRecord").expect("expected MyRecord");
        assert_eq!(fields.len(), 4);

        // Check each field's visibility
        let owner = fields.iter().find(|(n, _)| n == "owner").expect("owner");
        assert_eq!(owner.1, "private");

        let balance = fields.iter().find(|(n, _)| n == "balance").expect("balance");
        assert_eq!(balance.1, "public");

        let gates = fields.iter().find(|(n, _)| n == "gates").expect("gates");
        assert_eq!(gates.1, "private");

        let data = fields.iter().find(|(n, _)| n == "data").expect("data");
        assert_eq!(data.1, "private");
    }

    #[test]
    fn test_parse_record_declarations_clean_no_records() {
        let leo_source = r#"program test.aleo;

    function main:
        input r0 as u32.public;
        input r1 as u32.private;
        add r0 r1 into r2;
        output r2 as u32.private;
"#;
        let lines: Vec<&str> = leo_source.lines().collect();
        let records = parse_record_declarations(&lines);
        assert!(records.is_empty(), "expected no records in clean source");
    }

    // -----------------------------------------------------------------------
    // Audit: parse_let_record_field
    // -----------------------------------------------------------------------

    #[test]
    fn test_parse_let_record_field_taint_case() {
        let leo_source = r#"program test.aleo;

    record Credential {
        owner: address,
        secret: u64,
        public label: u64,
    }

    transition submit(credential: Credential, amount: u64) -> u64 {
        let tmp = credential.secret;
        return tmp;
    }
"#;
        let lines: Vec<&str> = leo_source.lines().collect();

        // Parse the record declaration
        let record_decls = parse_record_declarations(&lines);
        assert!(record_decls.contains_key("Credential"));

        // Extract transition params like handle_audit does
        let sig_line = lines[8]; // transition submit(...
        let params = leo_func_params(sig_line);
        let record_params: Vec<(String, &str)> = params
            .iter()
            .filter_map(|(pname, ptype)| {
                let clean_ty = ptype.trim_end_matches(',');
                if record_decls.contains_key(clean_ty) {
                    Some((pname.clone(), clean_ty))
                } else {
                    None
                }
            })
            .collect();

        // Should have identified 'credential' as a record-typed param
        assert_eq!(record_params.len(), 1);
        assert_eq!(record_params[0].0, "credential");
        assert_eq!(record_params[0].1, "Credential");

        // Line 9: `let tmp = credential.secret;` -- private field, should match
        let result = parse_let_record_field(lines[9], &record_params, &record_decls);
        assert_eq!(
            result,
            Some(("tmp".to_string(), "credential.secret".to_string()))
        );

        // Same pattern with public field should NOT match
        let result_public =
            parse_let_record_field("let x = credential.label;", &record_params, &record_decls);
        assert_eq!(result_public, None);
    }

    // -----------------------------------------------------------------------
    // Audit: parse_direct_field_access
    // -----------------------------------------------------------------------

    #[test]
    fn test_parse_direct_field_access_private_field() {
        let mut record_decls: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
        record_decls.insert(
            "Token".to_string(),
            vec![
                ("owner".to_string(), "private".to_string()),
                ("amount".to_string(), "private".to_string()),
                ("public_data".to_string(), "public".to_string()),
            ],
        );

        let record_params: Vec<(String, &str)> =
            vec![("cred".to_string(), "Token")];

        // Private field access should be detected
        let result = parse_direct_field_access("cred.amount", &record_params, &record_decls);
        assert_eq!(result, Some(("cred".to_string(), "amount".to_string())));

        // Public field access should NOT be detected
        let result_public =
            parse_direct_field_access("cred.public_data", &record_params, &record_decls);
        assert_eq!(result_public, None);
    }

    // -----------------------------------------------------------------------
    // Audit: extract_finalize_calls
    // -----------------------------------------------------------------------

    #[test]
    fn test_extract_finalize_calls_basic() {
        let line = "return (finalize_transfer(sender, receiver, amount));";
        let calls = extract_finalize_calls(line);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "finalize_transfer");
        assert_eq!(calls[0].1, vec!["sender", "receiver", "amount"]);
    }

    #[test]
    fn test_extract_finalize_calls_dot_access_args() {
        let line = "return (finalize_submit(cred.amount, cred.owner, public_value));";
        let calls = extract_finalize_calls(line);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "finalize_submit");
        assert_eq!(calls[0].1, vec!["cred.amount", "cred.owner", "public_value"]);
    }

    #[test]
    fn test_extract_finalize_calls_no_calls() {
        let line = "return (foo + bar);";
        let calls = extract_finalize_calls(line);
        assert!(calls.is_empty(), "expected no function calls");
    }

    // -----------------------------------------------------------------------
    // Bindings: leo_ty_to_ts
    // -----------------------------------------------------------------------

    #[test]
    fn test_leo_ty_to_ts_object_format() {
        // Object format: {"Primitive": {"UInt": "U64"}}
        let ty: serde_json::Value =
            serde_json::from_str(r#"{"Primitive": {"UInt": "U64"}}"#).unwrap();
        assert_eq!(leo_ty_to_ts(&ty), "bigint");

        // {"Primitive": {"UInt": "U32"}}
        let ty: serde_json::Value =
            serde_json::from_str(r#"{"Primitive": {"UInt": "U32"}}"#).unwrap();
        assert_eq!(leo_ty_to_ts(&ty), "number");

        // {"Primitive": {"UInt": "U8"}}
        let ty: serde_json::Value =
            serde_json::from_str(r#"{"Primitive": {"UInt": "U8"}}"#).unwrap();
        assert_eq!(leo_ty_to_ts(&ty), "number");

        // {"Primitive": {"Boolean": null}}
        let ty: serde_json::Value =
            serde_json::from_str(r#"{"Primitive": {"Boolean": null}}"#).unwrap();
        assert_eq!(leo_ty_to_ts(&ty), "boolean");
    }

    #[test]
    fn test_leo_ty_to_ts_string_format() {
        // String format: {"Primitive": "Field"}
        let ty: serde_json::Value =
            serde_json::from_str(r#"{"Primitive": "Field"}"#).unwrap();
        assert_eq!(leo_ty_to_ts(&ty), "bigint");

        let ty: serde_json::Value =
            serde_json::from_str(r#"{"Primitive": "Address"}"#).unwrap();
        assert_eq!(leo_ty_to_ts(&ty), "string");

        let ty: serde_json::Value =
            serde_json::from_str(r#"{"Primitive": "Boolean"}"#).unwrap();
        assert_eq!(leo_ty_to_ts(&ty), "boolean");
    }

    // -----------------------------------------------------------------------
    // Bindings: leo_type_converter_expr
    // -----------------------------------------------------------------------

    #[test]
    fn test_leo_type_converter_expr_object_format() {
        // {"Primitive": {"UInt": "U64"}} -> toU64(x)
        let ty: serde_json::Value =
            serde_json::from_str(r#"{"Primitive": {"UInt": "U64"}}"#).unwrap();
        assert_eq!(leo_type_converter_expr(&ty, "x"), "toU64(x)");

        // {"Primitive": {"UInt": "U32"}} -> toU32(x)
        let ty: serde_json::Value =
            serde_json::from_str(r#"{"Primitive": {"UInt": "U32"}}"#).unwrap();
        assert_eq!(leo_type_converter_expr(&ty, "x"), "toU32(x)");

        // {"Primitive": {"UInt": "U8"}} -> toU8(x)
        let ty: serde_json::Value =
            serde_json::from_str(r#"{"Primitive": {"UInt": "U8"}}"#).unwrap();
        assert_eq!(leo_type_converter_expr(&ty, "x"), "toU8(x)");

        // {"Primitive": {"Boolean": null}} -> toBoolean(x)
        let ty: serde_json::Value =
            serde_json::from_str(r#"{"Primitive": {"Boolean": null}}"#).unwrap();
        assert_eq!(leo_type_converter_expr(&ty, "x"), "toBoolean(x)");
    }

    #[test]
    fn test_leo_type_converter_expr_string_format() {
        // {"Primitive": "Field"} -> toField(x)
        let ty: serde_json::Value =
            serde_json::from_str(r#"{"Primitive": "Field"}"#).unwrap();
        assert_eq!(leo_type_converter_expr(&ty, "x"), "toField(x)");

        // {"Primitive": "Address"} -> x (passes through as string)
        let ty: serde_json::Value =
            serde_json::from_str(r#"{"Primitive": "Address"}"#).unwrap();
        assert_eq!(leo_type_converter_expr(&ty, "x"), "x");

        // {"Primitive": "Boolean"} -> toBoolean(x)
        let ty: serde_json::Value =
            serde_json::from_str(r#"{"Primitive": "Boolean"}"#).unwrap();
        assert_eq!(leo_type_converter_expr(&ty, "x"), "toBoolean(x)");
    }

    // -----------------------------------------------------------------------
    // Query: build_query_args
    // -----------------------------------------------------------------------

    #[test]
    fn test_build_query_args_stateroot_no_flags() {
        let args = build_query_args("stateroot", &[], None, None, &None);
        assert_eq!(args, vec!["stateroot"]);
    }

    #[test]
    fn test_build_query_args_with_network_and_endpoint() {
        let args = build_query_args(
            "block",
            &["123".to_string()],
            Some(&Network::Testnet),
            Some("https://api.testnet.com"),
            &None,
        );
        assert_eq!(
            args,
            vec![
                "block",
                "123",
                "--network",
                "testnet",
                "--endpoint",
                "https://api.testnet.com",
            ]
        );
    }

    #[test]
    fn test_build_query_args_with_json_output() {
        let jo: Option<Option<PathBuf>> = Some(None);
        let args = build_query_args("committee", &[], None, None, &jo);
        assert_eq!(args, vec!["committee", "--json-output"]);
    }

    #[test]
    fn test_build_query_args_program_with_flags() {
        let args = build_query_args(
            "program",
            &["credits.aleo".to_string(), "--mappings".to_string()],
            Some(&Network::Mainnet),
            None,
            &None,
        );
        assert_eq!(
            args,
            vec!["program", "credits.aleo", "--mappings", "--network", "mainnet"]
        );
    }

    // -----------------------------------------------------------------------
    // Init: name-sanitization
    // -----------------------------------------------------------------------

    #[test]
    fn test_init_name_sanitization() {
        let project_name = "my-aleo-project";
        let program_id = project_name.replace('-', "_");
        // Hyphens replaced with underscores in program ID
        assert_eq!(program_id, "my_aleo_project");
        // Original folder name preserved unchanged
        assert_eq!(project_name, "my-aleo-project");
    }

    // -----------------------------------------------------------------------
    // Error translation: register_to_name
    // -----------------------------------------------------------------------

    #[test]
    fn test_register_to_name_basic() {
        // When no project dir is given, should fall back to "argument N"
        assert_eq!(register_to_name("r0", "main", None), "argument 0");
        assert_eq!(register_to_name("r1", "main", None), "argument 1");
        assert_eq!(register_to_name("r2", "main", None), "argument 2");
    }

    // -----------------------------------------------------------------------
    // Run/Execute: build_leo_run_args
    // -----------------------------------------------------------------------

    #[test]
    fn test_build_leo_run_args_name_only() {
        let args = build_leo_run_args("main", &[], None, None, &None, None);
        assert_eq!(args, vec!["main"]);
    }

    #[test]
    fn test_build_leo_run_args_with_inputs() {
        let args = build_leo_run_args(
            "transfer",
            &["1u32".to_string(), "aleo1abc".to_string()],
            None,
            None,
            &None,
            None,
        );
        assert_eq!(args, vec!["transfer", "1u32", "aleo1abc"]);
    }

    #[test]
    fn test_build_leo_run_args_with_network() {
        let args = build_leo_run_args(
            "main",
            &[],
            Some(&Network::Testnet),
            None,
            &None,
            None,
        );
        assert_eq!(args, vec!["main", "--network", "testnet"]);
    }

    #[test]
    fn test_build_leo_run_args_with_endpoint_and_json_output() {
        let args = build_leo_run_args(
            "main",
            &[],
            None,
            Some("http://localhost:3030"),
            &Some(None),
            None,
        );
        assert_eq!(
            args,
            vec!["main", "--endpoint", "http://localhost:3030", "--json-output"]
        );
    }

    #[test]
    fn test_build_leo_run_args_all_flags() {
        let args = build_leo_run_args(
            "submit",
            &["100u64".to_string(), "aleo1xyz".to_string()],
            Some(&Network::Mainnet),
            Some("https://api.provable.com/v2"),
            &Some(Some(PathBuf::from("/tmp/out.json"))),
            None,
        );
        assert_eq!(
            args,
            vec![
                "submit",
                "100u64",
                "aleo1xyz",
                "--network",
                "mainnet",
                "--endpoint",
                "https://api.provable.com/v2",
                "--json-output=/tmp/out.json",
            ]
        );
    }

    #[test]
    fn test_build_leo_run_args_with_private_key() {
        let args = build_leo_run_args(
            "main",
            &[],
            None,
            None,
            &None,
            Some("APrivateKey1zkpGPDbTcP2rWRMFLa1quxwGMK2BNJ16HWjYjofTH1pMUYj"),
        );
        assert_eq!(
            args,
            vec![
                "main",
                "--private-key",
                "APrivateKey1zkpGPDbTcP2rWRMFLa1quxwGMK2BNJ16HWjYjofTH1pMUYj",
            ]
        );
    }

    // -----------------------------------------------------------------------
    // Doctor: doctor_check and CheckCounts
    // -----------------------------------------------------------------------

    #[test]
    fn test_doctor_check_increments_pass() {
        let mut counts = CheckCounts::default();
        doctor_check("test-pass", "pass", "all good", &mut counts);
        assert_eq!(counts.total, 1);
        assert_eq!(counts.passed, 1);
        assert_eq!(counts.warned, 0);
        assert_eq!(counts.failed, 0);
    }

    #[test]
    fn test_doctor_check_increments_warn() {
        let mut counts = CheckCounts::default();
        doctor_check("test-warn", "warn", "something odd", &mut counts);
        assert_eq!(counts.total, 1);
        assert_eq!(counts.passed, 0);
        assert_eq!(counts.warned, 1);
        assert_eq!(counts.failed, 0);
    }

    #[test]
    fn test_doctor_check_increments_fail() {
        let mut counts = CheckCounts::default();
        doctor_check("test-fail", "fail", "something bad", &mut counts);
        assert_eq!(counts.total, 1);
        assert_eq!(counts.passed, 0);
        assert_eq!(counts.warned, 0);
        assert_eq!(counts.failed, 1);
    }

    #[test]
    fn test_doctor_check_mixed_counts() {
        let mut counts = CheckCounts::default();
        doctor_check("p1", "pass", "", &mut counts);
        doctor_check("w1", "warn", "", &mut counts);
        doctor_check("f1", "fail", "", &mut counts);
        doctor_check("p2", "pass", "", &mut counts);
        assert_eq!(counts.total, 4);
        assert_eq!(counts.passed, 2);
        assert_eq!(counts.warned, 1);
        assert_eq!(counts.failed, 1);
    }

    #[test]
    fn test_doctor_check_unknown_severity_treated_as_fail() {
        let mut counts = CheckCounts::default();
        doctor_check("unknown", "unknown", "test fallback", &mut counts);
        assert_eq!(counts.total, 1);
        assert_eq!(counts.passed, 0);
        assert_eq!(counts.warned, 0);
        assert_eq!(counts.failed, 1);
    }

    #[test]
    fn test_check_counts_default_is_zero() {
        let counts = CheckCounts::default();
        assert_eq!(counts.total, 0);
        assert_eq!(counts.passed, 0);
        assert_eq!(counts.warned, 0);
        assert_eq!(counts.failed, 0);
    }

    // -----------------------------------------------------------------------
    // Private key format validation
    // -----------------------------------------------------------------------

    /// A valid-looking Aleo private key (59 chars, starts with APrivateKey1).
    const VALID_KEY: &str = "APrivateKey1zkpGPDbTcP2rWRMFLa1quxwGMK2BNJ16HWjYjofTH1pMUYj";

    #[test]
    fn test_validate_private_key_valid() {
        assert!(validate_private_key_format(VALID_KEY).is_none());
    }

    #[test]
    fn test_validate_private_key_angle_brackets() {
        let reason = validate_private_key_format("<fake-key>").unwrap();
        assert!(reason.contains("'<'") || reason.contains("angle"));
        // Also test the specific pattern from the spec: literal "<fake>"
        let reason2 = validate_private_key_format("<fake>").unwrap();
        assert!(reason2.contains("placeholder"));
    }

    #[test]
    fn test_validate_private_key_wrong_prefix() {
        let reason = validate_private_key_format("AViewKey1...").unwrap();
        assert!(reason.contains("APrivateKey1"));
    }

    #[test]
    fn test_validate_private_key_empty() {
        let reason = validate_private_key_format("").unwrap();
        assert!(reason.contains("APrivateKey1"));
    }

    #[test]
    fn test_validate_private_key_wrong_length() {
        // Valid prefix but truncated
        let short = "APrivateKey1abc";
        let reason = validate_private_key_format(short).unwrap();
        assert!(reason.contains("length") || reason.contains("59"));
    }

    // -----------------------------------------------------------------------
    // Send: format_send_amount
    // -----------------------------------------------------------------------

    #[test]
    fn test_format_send_amount_valid() {
        // 1 credit = 1,000,000 microcredits; formatting appends the u64 literal
        // required by credits.aleo's transfer_public(to, amount) signature.
        assert_eq!(format_send_amount("1000000").unwrap(), "1000000u64");
        assert_eq!(format_send_amount("0").unwrap(), "0u64");
        assert_eq!(format_send_amount("18446744073709551615").unwrap(), "18446744073709551615u64");
    }

    #[test]
    fn test_format_send_amount_rejects_non_u64() {
        assert!(format_send_amount("-5").is_err());
        assert!(format_send_amount("1.5").is_err());
        assert!(format_send_amount("abc").is_err());
        assert!(format_send_amount("").is_err());
        assert!(format_send_amount("99999999999999999999").is_err());
    }
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

    /// Named environment profile from aleo.toml (sets network/endpoint)
    #[arg(long, global = true)]
    profile: Option<String>,
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
    /// Format Leo source files with leo-fmt
    Fmt(FmtArgs),
    /// Run a security audit on an Aleo project
    Audit(AuditArgs),
    /// Generate TypeScript bindings from a compiled Aleo program's ABI
    Bindings(BindingsArgs),
    /// Locally execute a transition/function (dry-run, no transaction sent).
    /// Best-effort error translation is applied to known leo failure patterns.
    Run(RunArgs),
    /// Execute a transition/function on-chain (dry-run unless --broadcast).
    /// Best-effort error translation is applied to known leo failure patterns.
    Execute(ExecuteArgs),
    /// Send testnet credits to an Aleo address via credits.aleo's transfer_public
    /// transition (dry-run unless --broadcast). Amount is in microcredits.
    Send(SendArgs),
    /// Scan, list, and manage Aleo records via snarkOS
    #[command(subcommand)]
    Records(RecordsCmd),
    /// Open the Aleo faucet in your browser for testnet credits
    Faucet(FaucetArgs),
    /// Diagnose the local Aleo development environment
    Doctor(DoctorArgs),
    /// Manage Aleo accounts: generate, import, sign, verify, and decrypt
    #[command(subcommand)]
    Account(AccountCmd),
    /// Preview resolved configuration (network, endpoint, profile, env vars)
    Env(EnvArgs),
    /// Query Aleo network state (block, transaction, program, stateroot, committee)
    #[command(subcommand)]
    Query(QueryCmd),
    /// Run AleoFlow as a local MCP (Model Context Protocol) server over stdio,
    /// exposing AleoFlow commands as tools for AI coding assistants.
    /// Broadcast (funds-spending) tools require ALEOFLOW_MCP_ALLOW_BROADCAST=true.
    Mcp(McpArgs),
}

#[derive(Subcommand)]
enum RecordsCmd {
    /// List decrypted records for a view key by scanning a local snarkOS node
    List(RecordsListArgs),
}

#[derive(Args)]
struct RecordsListArgs {
    /// Aleo view key to scan records for
    #[arg(long = "view-key")]
    view_key: String,
    /// Start block height (defaults to 0)
    #[arg(long, default_value_t = 0)]
    start: u32,
    /// End block height (required)
    #[arg(long)]
    end: u32,
    /// Local snarkOS endpoint URL (defaults to http://localhost:3030)
    #[arg(long)]
    endpoint: Option<String>,
}

// ---------------------------------------------------------------------------
// Query command: nested subcommands mirroring the RecordsCmd pattern
// ---------------------------------------------------------------------------

#[derive(Subcommand)]
enum QueryCmd {
    /// Query a block by ID, latest, or range (max 50 per range request)
    Block(QueryBlockArgs),
    /// Query a transaction by ID or filters
    Transaction(QueryTransactionArgs),
    /// Query a deployed program's structure or mapping values
    Program(QueryProgramArgs),
    /// Query the current state root
    Stateroot(QueryStaterootArgs),
    /// Query the current committee information
    Committee(QueryCommitteeArgs),
}

#[derive(Args)]
struct QueryBlockArgs {
    /// Block ID (height or hash); if omitted, uses --latest
    id: Option<String>,
    /// Query the latest block
    #[arg(short = 'l', long)]
    latest: bool,
    /// Get the latest block hash only
    #[arg(long)]
    latest_hash: bool,
    /// Get the latest block height only
    #[arg(long)]
    latest_height: bool,
    /// Get consecutive blocks (max 50 per request)
    #[arg(short = 'r', long, num_args = 2)]
    range: Option<Vec<String>>,
    /// Include transactions in output
    #[arg(short = 't', long)]
    transactions: bool,
    /// Include the cumulative height in output
    #[arg(long)]
    to_height: bool,
    /// Target network
    #[arg(long, value_parser = clap::value_parser!(Network))]
    network: Option<Network>,
    /// Aleo network endpoint URL
    #[arg(long)]
    endpoint: Option<String>,
    /// Write command results as JSON
    #[arg(long)]
    json_output: Option<Option<PathBuf>>,
}

#[derive(Args)]
struct QueryTransactionArgs {
    /// Transaction ID; if omitted, must use one of the --from-* flags
    id: Option<String>,
    /// Query confirmed transactions only
    #[arg(short = 'c', long)]
    confirmed: bool,
    /// Query unconfirmed transactions only
    #[arg(short = 'u', long)]
    unconfirmed: bool,
    /// Filter by program IO ID
    #[arg(long)]
    from_io: Option<String>,
    /// Filter by transition ID
    #[arg(long)]
    from_transition: Option<String>,
    /// Filter by program name
    #[arg(long)]
    from_program: Option<String>,
    /// Target network
    #[arg(long, value_parser = clap::value_parser!(Network))]
    network: Option<Network>,
    /// Aleo network endpoint URL
    #[arg(long)]
    endpoint: Option<String>,
    /// Write command results as JSON
    #[arg(long)]
    json_output: Option<Option<PathBuf>>,
}

#[derive(Args)]
struct QueryProgramArgs {
    /// Deployed program name (e.g. credits.aleo)
    name: String,
    /// Program edition number
    #[arg(long)]
    edition: Option<u32>,
    /// List all mapping names
    #[arg(long)]
    mappings: bool,
    /// Look up a specific mapping value: --mapping-value <MAPPING> <KEY>
    #[arg(long, num_args = 2)]
    mapping_value: Option<Vec<String>>,
    /// Target network
    #[arg(long, value_parser = clap::value_parser!(Network))]
    network: Option<Network>,
    /// Aleo network endpoint URL
    #[arg(long)]
    endpoint: Option<String>,
    /// Write command results as JSON
    #[arg(long)]
    json_output: Option<Option<PathBuf>>,
}

#[derive(Args)]
struct QueryStaterootArgs {
    /// Target network
    #[arg(long, value_parser = clap::value_parser!(Network))]
    network: Option<Network>,
    /// Aleo network endpoint URL
    #[arg(long)]
    endpoint: Option<String>,
    /// Write command results as JSON
    #[arg(long)]
    json_output: Option<Option<PathBuf>>,
}

#[derive(Args)]
struct QueryCommitteeArgs {
    /// Target network
    #[arg(long, value_parser = clap::value_parser!(Network))]
    network: Option<Network>,
    /// Aleo network endpoint URL
    #[arg(long)]
    endpoint: Option<String>,
    /// Write command results as JSON
    #[arg(long)]
    json_output: Option<Option<PathBuf>>,
}

#[derive(Args)]
struct InitArgs {
    /// Name of the new project
    name: String,
    /// Project template to use (defaults to 'payment', or to aleo.toml's default_template)
    #[arg(long = "template", value_parser = clap::value_parser!(Template))]
    template: Option<Template>,
    /// Comma-separated workspace member names (e.g. "token,governance,treasury").
    /// When set, creates a workspace root with workspace.json and scaffolds each member.
    #[arg(long)]
    workspace: Option<String>,
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
    /// Aleo network endpoint URL
    #[arg(long)]
    endpoint: Option<String>,
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
    /// Target a specific workspace member by name (requires workspace root).
    #[arg(long)]
    package: Option<String>,
    /// Deploy all workspace members sequentially (requires workspace root).
    #[arg(long)]
    all: bool,
    /// Aleo network endpoint URL
    #[arg(long)]
    endpoint: Option<String>,
    /// Account private key for deployment
    #[arg(long)]
    pub private_key: Option<String>,
}

#[derive(Args)]
struct AuditArgs {
    /// Path to the Aleo project to audit
    path: String,
}

#[derive(Args)]
struct FmtArgs {
    /// Path to the Aleo project directory (defaults to current dir)
    #[arg(long)]
    path: Option<PathBuf>,
}

#[derive(Args)]
struct BindingsArgs {
    /// Path to the Aleo project directory (required unless --remote is set)
    #[arg(required_unless_present = "remote")]
    path: Option<PathBuf>,
    /// Output path for the generated TypeScript file (defaults to <path>/bindings/<program_name>.ts)
    #[arg(long)]
    output: Option<PathBuf>,
    /// Remote program ID to generate bindings for (e.g. "credits.aleo").
    /// When set, fetches the compiled program from the network instead of using a local project.
    #[arg(long)]
    remote: Option<String>,
    /// Network to use for fetching the remote program (defaults to testnet)
    #[arg(long, value_parser = clap::value_parser!(Network))]
    network: Option<Network>,
}

#[derive(Args)]
struct RunArgs {
    /// Name of the transition/function to run (defaults to "main")
    #[arg(default_value = "main")]
    name: String,
    /// Input arguments as raw Leo literal strings (e.g. "1u32", "aleo1...")
    inputs: Vec<String>,
    /// Path to the Aleo project directory (defaults to current dir)
    #[arg(long)]
    path: Option<PathBuf>,
    /// Target network
    #[arg(long, value_parser = clap::value_parser!(Network))]
    network: Option<Network>,
    /// Aleo network endpoint URL
    #[arg(long)]
    endpoint: Option<String>,
    /// Write command results as JSON (optionally --json-output=<FILE> for a custom path)
    #[arg(long)]
    json_output: Option<Option<PathBuf>>,
    /// Account private key for execution
    #[arg(long)]
    pub private_key: Option<String>,
}

#[derive(Args)]
struct ExecuteArgs {
    /// Name of the transition/function to execute (defaults to "main")
    #[arg(default_value = "main")]
    name: String,
    /// Input arguments as raw Leo literal strings (e.g. "1u32", "aleo1...")
    inputs: Vec<String>,
    /// Path to the Aleo project directory (defaults to current dir)
    #[arg(long)]
    path: Option<PathBuf>,
    /// Target network
    #[arg(long, value_parser = clap::value_parser!(Network))]
    network: Option<Network>,
    /// Aleo network endpoint URL
    #[arg(long)]
    endpoint: Option<String>,
    /// Actually broadcast the execution transaction (without this, runs in dry-run mode)
    #[arg(long)]
    broadcast: bool,
    /// Write command results as JSON (optionally --json-output=<FILE> for a custom path)
    #[arg(long)]
    json_output: Option<Option<PathBuf>>,
    /// Account private key for execution
    #[arg(long)]
    pub private_key: Option<String>,
}

#[derive(Args)]
#[command(
    after_help = "For private transfers, use `aleoflow execute transfer_private <record> <to> <amount>` directly once you have a record from `aleoflow records list`."
)]
struct SendArgs {
    /// Recipient Aleo address (aleo1...)
    to: String,
    /// Amount to send, in microcredits (e.g. 1000000 = 1 credit)
    amount: String,
    /// Target network
    #[arg(long, value_parser = clap::value_parser!(Network))]
    network: Option<Network>,
    /// Aleo network endpoint URL
    #[arg(long)]
    endpoint: Option<String>,
    /// Actually broadcast the transfer transaction (without this, runs in dry-run mode)
    #[arg(long)]
    broadcast: bool,
    /// Account private key for the transfer
    #[arg(long)]
    pub private_key: Option<String>,
}

#[derive(Args)]
struct DoctorArgs {
    // No arguments needed; doctor diagnoses the environment automatically.
}

#[derive(Args)]
struct FaucetArgs {
    /// Aleo address to request testnet credits for
    address: Option<String>,
}

#[derive(Args)]
struct EnvArgs {
    /// Target network
    #[arg(long, value_parser = clap::value_parser!(Network))]
    network: Option<Network>,
    /// Aleo network endpoint URL
    #[arg(long)]
    endpoint: Option<String>,
}

#[derive(Args)]
struct McpArgs {
    // No arguments: the MCP server reads JSON-RPC messages from stdin and
    // writes protocol responses to stdout. All diagnostics go to stderr.
}

// ---------------------------------------------------------------------------
// Account command: nested subcommands mirroring the RecordsCmd pattern
// ---------------------------------------------------------------------------

#[derive(Subcommand)]
enum AccountCmd {
    /// Generate a new Aleo account (optionally with a seed)
    New(AccountNewArgs),
    /// Derive an Aleo account from a private key
    Import(AccountImportArgs),
    /// Sign a message using your Aleo private key
    Sign(AccountSignArgs),
    /// Verify a message from an Aleo address
    Verify(AccountVerifyArgs),
    /// Decrypt record ciphertext using your Aleo private key or view key
    Decrypt(AccountDecryptArgs),
}

#[derive(Args)]
struct AccountNewArgs {
    /// Seed the RNG with a numeric value
    #[arg(short = 's', long)]
    seed: Option<u64>,
    /// Write the private key to the .env file in the current directory
    #[arg(short = 'w', long)]
    write: bool,
    /// Print sensitive information discreetly to an alternate screen
    #[arg(long)]
    discreet: bool,
    /// Name of the network to use
    #[arg(short = 'n', long, default_value = "testnet")]
    network: String,
    /// Endpoint to record in the generated .env file (only used with --write)
    #[arg(short = 'e', long)]
    endpoint: Option<String>,
}

#[derive(Args)]
struct AccountImportArgs {
    /// Private key plaintext (omit for interactive prompt)
    private_key: Option<String>,
    /// Write the private key to the .env file in the current directory
    #[arg(short = 'w', long)]
    write: bool,
    /// Print sensitive information discreetly to an alternate screen
    #[arg(long)]
    discreet: bool,
    /// Name of the network to use
    #[arg(short = 'n', long, default_value = "testnet")]
    network: String,
    /// Endpoint to record in the generated .env file (only used with --write)
    #[arg(short = 'e', long)]
    endpoint: Option<String>,
}

#[derive(Args)]
struct AccountSignArgs {
    /// Message (Aleo value) to sign
    #[arg(short = 'm', long)]
    message: String,
    /// Specify the account private key
    #[arg(long)]
    private_key: Option<String>,
    /// Specify the path to a file containing the account private key
    #[arg(long)]
    private_key_file: Option<PathBuf>,
    /// When enabled, parses the message as bytes instead of Aleo literals
    #[arg(short = 'r', long)]
    raw: bool,
}

#[derive(Args)]
struct AccountVerifyArgs {
    /// Address to use for verification
    #[arg(short = 'a', long)]
    address: String,
    /// Signature to verify
    #[arg(short = 's', long)]
    signature: String,
    /// Message (Aleo value) to verify the signature against
    #[arg(short = 'm', long)]
    message: String,
    /// When enabled, parses the message as bytes instead of Aleo literals
    #[arg(short = 'r', long)]
    raw: bool,
}

#[derive(Args)]
struct AccountDecryptArgs {
    /// The ciphertext to decrypt
    #[arg(short = 'c', long)]
    ciphertext: String,
    /// Private key or view key to use for decryption
    #[arg(short = 'k')]
    key: Option<String>,
    /// Path to a file containing the private key or view key
    #[arg(short = 'f')]
    key_file: Option<PathBuf>,
}

#[derive(clap::ValueEnum, Clone)]
enum Template {
    Payment,
    Defi,
    AiAgent,
    Gamefi,
    Token,
}

#[derive(clap::ValueEnum, Clone)]
enum Network {
    Testnet,
    Mainnet,
    Canary,
}

/// Default endpoint used by query and execute commands when no --endpoint or
/// --profile resolves one. Required because `leo query` and `leo execute` do
/// not have a built-in fallback endpoint (unlike `leo run`/`leo deploy`, which
/// self-default to the same URL).
const DEFAULT_QUERY_ENDPOINT: &str = "https://api.explorer.provable.com/v1";

fn main() -> Result<()> {
    let cli = Cli::parse();
    let quiet = cli.quiet;
    let profile = cli.profile.as_deref();

    match &cli.command {
        Commands::Init(args) => handle_init(args, quiet),
        Commands::Devnet(args) => handle_devnet(args, quiet, profile),
        Commands::Build(args) => handle_build(args, quiet),
        Commands::Test(args) => handle_test(args, quiet),
        Commands::Deploy(args) => handle_deploy(args, quiet, profile),
        Commands::Fmt(args) => handle_fmt(args, quiet),
        Commands::Audit(args) => handle_audit(args, quiet),
        Commands::Bindings(args) => handle_bindings(args, quiet),
        Commands::Run(args) => handle_run(args, quiet, profile),
        Commands::Execute(args) => handle_execute(args, quiet, profile),
        Commands::Send(args) => handle_send(args, quiet, profile),
        Commands::Records(cmd) => handle_records(cmd, quiet, profile),
        Commands::Faucet(args) => handle_faucet(args),
        Commands::Doctor(args) => handle_doctor(args, quiet),
        Commands::Account(cmd) => handle_account(cmd, quiet),
        Commands::Env(args) => handle_env(args, quiet, profile),
        Commands::Query(cmd) => handle_query(cmd, quiet, profile),
        Commands::Mcp(_args) => mcp::run(),
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
                        Template::Token => "token",
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
        Template::Token => "token",
    };

    let template = find_template(template_name).with_context(|| {
        format!(
            "Template '{}' not found. This is a bug -- please reinstall aleoflow.",
            template_name
        )
    })?;

    // --- Workspace path ---
    if let Some(ref workspace_names) = args.workspace {
        let members: Vec<&str> = workspace_names
            .split(',')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect();
        if members.is_empty() {
            bail!("Workspace must contain at least one member name");
        }

        let dest_dir = Path::new(&args.name);
        if dest_dir.exists() {
            bail!(
                "Destination directory '{}' already exists -- not overwriting",
                dest_dir.display()
            );
        }

        fs::create_dir_all(dest_dir)?;

        // Write workspace.json
        let ws_json = serde_json::json!({ "members": members });
        let ws_path = dest_dir.join("workspace.json");
        let ws_content = serde_json::to_string_pretty(&ws_json)?;
        fs::write(&ws_path, ws_content)?;

        // Scaffold each member using the same template
        for member_name in &members {
            let member_id = member_name.replace('-', "_");
            let member_dir = dest_dir.join(member_name);

            for file in template.files {
                let dest = member_dir.join(file.rel_path);
                if let Some(parent) = dest.parent() {
                    fs::create_dir_all(parent)?;
                }
                let substituted = file.contents.replace("{{PROJECT_NAME}}", &member_id);
                fs::write(&dest, &substituted)?;
            }
        }

        println!(
            "{} Created workspace '{}' with {} member(s)",
            "[done]".green().bold(),
            args.name.cyan(),
            members.len()
        );
        for member in &members {
            println!("  - {}", member.cyan());
        }
        println!();
        println!("  {} cd {}", "$".dimmed(), args.name);
        println!("  {} leo build", "$".dimmed());

        return Ok(());
    }

    // --- Single-project path (unchanged) ---
    let dest_dir = Path::new(&args.name);
    if dest_dir.exists() {
        bail!(
            "Destination directory '{}' already exists -- not overwriting",
            dest_dir.display()
        );
    }

    let program_id = args.name.replace('-', "_");

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

fn handle_fmt(args: &FmtArgs, quiet: bool) -> Result<()> {
    if !leo_cmd::leo_fmt_is_installed() {
        bail!(
            "leo-fmt was not found on PATH. Install it first: \
             https://github.com/AleoHQ/leo-fmt"
        );
    }

    let dir = args.path.as_deref();
    print_info("Running 'leo fmt'...", quiet);
    leo_cmd::run_leo_with("fmt", &[], dir)
}

/// Print a single diagnostic check result with the appropriate color tag.
/// Increments the corresponding counter in `counts`.
fn doctor_check(
    label: &str,
    severity: &str, // "pass", "warn", or "fail"
    msg: &str,
    counts: &mut CheckCounts,
) {
    counts.total += 1;
    match severity {
        "pass" => {
            println!("{} {}: {}", "[done]".green().bold(), label, msg);
            counts.passed += 1;
        }
        "warn" => {
            println!("{} {}: {}", "[warning]".yellow().bold(), label, msg);
            counts.warned += 1;
        }
        _ => {
            println!("{} {}: {}", "[error]".red().bold(), label, msg);
            counts.failed += 1;
        }
    }
}

/// Capture the stdout of a command as a trimmed string, or return a default.
fn cmd_version_output(cmd: &str, args: &[&str]) -> String {
    std::process::Command::new(cmd)
        .args(args)
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
            } else {
                None
            }
        })
        .unwrap_or_default()
}

/// Tracks diagnostic check counts.
#[derive(Default)]
struct CheckCounts {
    total: u32,
    passed: u32,
    warned: u32,
    failed: u32,
}

/// Validate that a private key string is not obviously malformed before
/// passing it to a leo subprocess. Returns `None` if the key looks valid,
/// or `Some(reason)` describing why it is invalid.
///
/// This is a quick sanity check, not a cryptographic verification.
/// We never print the actual key value in the error message.
fn validate_private_key_format(key: &str) -> Option<&'static str> {
    if key.contains('<') || key.contains('>') {
        return Some(
            "The provided private key doesn't look valid: contains '<' or '>' characters, \
             which usually means a placeholder was pasted by mistake instead of a real key."
        );
    }
    if !key.starts_with("APrivateKey1") {
        return Some(
            "The provided private key doesn't look valid: it should start with \
             'APrivateKey1'. Make sure you are using a real Aleo private key."
        );
    }
    if key.len() != 59 {
        return Some(
            "The provided private key doesn't look valid: wrong length. \
             A valid Aleo private key is exactly 59 characters."
        );
    }
    None
}

fn handle_doctor(_args: &DoctorArgs, _quiet: bool) -> Result<()> {
    let mut counts = CheckCounts::default();

    // 1. Rust toolchain -- single invocation to check + capture output
    let rustc_ver = cmd_version_output("rustc", &["--version"]);
    if rustc_ver.is_empty() {
        doctor_check("rustc", "fail", "not found on PATH. Install from https://rustup.rs", &mut counts);
    } else {
        doctor_check("rustc", "pass", &rustc_ver, &mut counts);
    }

    let cargo_ver = cmd_version_output("cargo", &["--version"]);
    if cargo_ver.is_empty() {
        doctor_check("cargo", "fail", "not found on PATH. Install from https://rustup.rs", &mut counts);
    } else {
        doctor_check("cargo", "pass", &cargo_ver, &mut counts);
    }

    // 2. Windows-only: GNU vs MSVC check, dlltool, LIBCLANG_PATH
    if cfg!(windows) {
        let rustup_host = std::process::Command::new("rustup")
            .args(["default"])
            .output()
            .ok()
            .and_then(|o| {
                if o.status.success() {
                    Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
                } else {
                    None
                }
            });

        match rustup_host {
            Some(ref host) if host.contains("msvc") => {
                doctor_check("rustup toolchain", "warn", &format!("MSVC toolchain active ({}) -- GNU recommended for Aleo development", host), &mut counts);
            }
            Some(ref host) if host.contains("gnu") => {
                doctor_check("rustup toolchain", "pass", &format!("GNU toolchain active ({})", host), &mut counts);
                // Only check dlltool with GNU toolchain
                let dlltool_ver = cmd_version_output("dlltool", &["--version"]);
                if dlltool_ver.is_empty() {
                    doctor_check("dlltool", "warn", "not found on PATH -- may be needed for linking", &mut counts);
                } else {
                    doctor_check("dlltool", "pass", "found on PATH", &mut counts);
                }
            }
            Some(ref host) => {
                doctor_check("rustup toolchain", "pass", &format!("active ({})", host), &mut counts);
            }
            None => {
                doctor_check("rustup", "warn", "could not determine default toolchain", &mut counts);
            }
        }

        // LIBCLANG_PATH check
        let clang_path = std::env::var("LIBCLANG_PATH");
        match clang_path {
            Ok(ref path) if !path.is_empty() && std::path::Path::new(path).exists() => {
                doctor_check("LIBCLANG_PATH", "pass", "set and points to an existing directory", &mut counts);
            }
            Ok(ref path) if !path.is_empty() => {
                doctor_check("LIBCLANG_PATH", "warn", &format!("set to '{}' but path does not exist", path), &mut counts);
            }
            _ => {
                doctor_check("LIBCLANG_PATH", "warn", "not set. Install LLVM with: winget install LLVM.LLVM", &mut counts);
            }
        }
    }

    // 3. leo
    let leo_ver = cmd_version_output("leo", &["--version"]);
    if leo_ver.is_empty() {
        doctor_check("leo", "fail", "not found on PATH. Install with: cargo binstall leo-lang or from https://github.com/AleoHQ/leo", &mut counts);
    } else {
        doctor_check("leo", "pass", &leo_ver, &mut counts);
    }

    // 4. snarkos (optional -- warn only)
    let snarkos_ver = cmd_version_output("snarkos", &["--version"]);
    if snarkos_ver.is_empty() {
        doctor_check("snarkos", "warn", "not found on PATH (optional, required for local record scanning and devnet)", &mut counts);
    } else {
        doctor_check("snarkos", "pass", &snarkos_ver, &mut counts);
    }

    // 5. leo-fmt
    if leo_cmd::leo_fmt_is_installed() {
        doctor_check("leo-fmt", "pass", "found on PATH", &mut counts);
    } else {
        doctor_check("leo-fmt", "warn", "not found on PATH (optional, used by 'aleoflow fmt'). Install from https://github.com/AleoHQ/leo-fmt", &mut counts);
    }

    // 6. Environment variables (set/unset only -- never print values)
    let pk_set = std::env::var("PRIVATE_KEY").is_ok();
    let net_set = std::env::var("NETWORK").is_ok();
    let ep_set = std::env::var("ENDPOINT").is_ok();

    doctor_check("PRIVATE_KEY", if pk_set { "pass" } else { "warn" }, if pk_set { "set" } else { "not set" }, &mut counts);
    doctor_check("NETWORK", if net_set { "pass" } else { "warn" }, if net_set { "set" } else { "not set" }, &mut counts);
    doctor_check("ENDPOINT", if ep_set { "pass" } else { "warn" }, if ep_set { "set" } else { "not set" }, &mut counts);

    // 7. Git repository check (Feature 2)
    // This is a real incident-derived check: a disconnected, non-git folder
    // silently absorbed real work earlier in this project's own development.
    let git_work_tree = std::process::Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
            } else {
                None
            }
        });

    match git_work_tree {
        Some(ref val) if val == "true" => {
            doctor_check(
                "git repo",
                "pass",
                "current directory is inside a git repository",
                &mut counts,
            );
            // Additional check: does this repo have a remote configured?
            let remote_output = std::process::Command::new("git")
                .args(["remote", "-v"])
                .output()
                .ok()
                .and_then(|o| {
                    if o.status.success() {
                        let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
                        if s.is_empty() { None } else { Some(s) }
                    } else {
                        None
                    }
                });
            match remote_output {
                Some(_) => {
                    doctor_check("git remote", "pass", "remote is configured", &mut counts);
                }
                None => {
                    doctor_check(
                        "git remote",
                        "warn",
                        "no remote configured -- repo is local-only and not backed up",
                        &mut counts,
                    );
                }
            }
        }
        _ => {
            doctor_check(
                "git repo",
                "warn",
                "current directory is not a git repository. If you are actively \
                 developing here, consider running 'git init' -- uncommitted work \
                 outside version control can be lost if files are moved, deleted, \
                 or overwritten.",
                &mut counts,
            );
        }
    }

    // 8. Stale-PATH detection (Feature 4)
    // For each tool (leo, snarkos, leo-fmt), we check whether it resolves on PATH.
    // If a tool is NOT found on PATH, we would ideally check if the binary exists
    // at a known install location (e.g. ~/.cargo/bin/leo) but is missing from the
    // current PATH. However, there is no reliable cross-platform way to enumerate
    // likely install directories without OS-specific search heuristics that risk
    // false positives (e.g. searching the entire filesystem, guessing at prefix
    // paths). Common locations like ~/.cargo/bin may not be correct for all
    // installations (brew on macOS, cargo-binstall elsewhere, manual installs).
    // Skipping this check to avoid misleading output.
    //
    // If we ever add a config option for a custom install path, we could revisit
    // this. For now, `doctor` already tells users when a tool is not on PATH
    // and how to install it.

    // 9. Summary
    println!();
    println!(
        "{} {} checks run, {} passed, {} warned, {} failed",
        if counts.failed > 0 { "[error]".red().bold() } else { "[done]".green().bold() },
        counts.total,
        counts.passed,
        counts.warned,
        counts.failed
    );

    if counts.failed > 0 {
        bail!("Some checks failed. Review the [error] items above and fix them before proceeding.");
    }
    if counts.warned > 0 {
        println!(
            "{} All critical checks passed ({} warnings -- review if needed).",
            "[done]".green().bold(),
            counts.warned
        );
    } else {
        println!("{} All checks passed.", "[done]".green().bold());
    }

    Ok(())
}

fn handle_devnet(args: &DevnetArgs, quiet: bool, profile: Option<&str>) -> Result<()> {

    if !leo_cmd::leo_is_installed() {
        bail!(
            "leo is not installed or not on PATH. Install it with: cargo binstall leo-lang"
        );
    }

    let cfg = load_aleoflow_config();
    let profile_res = resolve_profile(profile, &cfg, quiet)?;

    // Resolve network: CLI --network > --profile > config > default (Testnet)
    let network = args.network.clone().or(profile_res.network).or_else(move || {
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
    }).or(Some(Network::Testnet));

    // Resolve endpoint: CLI --endpoint > --profile
    let endpoint = args.endpoint.as_deref().or(profile_res.endpoint.as_deref());

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

    if let Some(ep) = endpoint {
        cmd.args(["--endpoint", ep]);
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

fn handle_deploy(args: &DeployArgs, quiet: bool, profile: Option<&str>) -> Result<()> {
    // Validate private key format before any subprocess runs
    if let Some(ref pk) = args.private_key {
        if let Some(reason) = validate_private_key_format(pk) {
            bail!("{}", reason);
        }
    }

    let cfg = load_aleoflow_config();
    let profile_res = resolve_profile(profile, &cfg, quiet)?;

    // Resolve network: CLI --network > --profile > config > default (Testnet)
    let network = args.network.clone().or(profile_res.network).or_else(move || {
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

    // Resolve endpoint: CLI --endpoint > --profile
    let endpoint = args.endpoint.as_deref().or(profile_res.endpoint.as_deref());

    let network_str = match network {
        Network::Testnet => "testnet",
        Network::Mainnet => "mainnet",
        Network::Canary => "canary",
    };

    if !leo_cmd::leo_is_installed() {
        bail!("leo is not installed or not on PATH. Install it with: cargo binstall leo-lang");
    }

    // --- Workspace detection ---
    let target_dir = args.path.as_deref();
    let ws_path = target_dir.map(|d| d.join("workspace.json"));
    let is_workspace_root = ws_path
        .as_ref()
        .map(|p| p.exists())
        .unwrap_or(false);

    if is_workspace_root {
        // Workspace root requires --package or --all
        let has_package = args.package.is_some();
        let deploy_all = args.all;

        if !has_package && !deploy_all {
            bail!(
                "'{}' is a workspace root. Use --package <name> to deploy a specific member, \
                 or --all to deploy all members sequentially.",
                target_dir.unwrap().display()
            );
        }

        // Read workspace.json to get member names
        let ws_content = fs::read_to_string(ws_path.as_ref().unwrap())?;
        let ws_json: serde_json::Value = serde_json::from_str(&ws_content)?;
        let members: Vec<String> = ws_json["members"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        if deploy_all {
            // Loop through each member sequentially
            for member in &members {
                if args.broadcast && matches!(network, Network::Mainnet) {
                    println!(
                        "{} Deploying '{}' to MAINNET with --broadcast. This is irreversible and costs real fees.",
                        "[warning]".yellow().bold(),
                        member
                    );
                }

                if !args.broadcast {
                    print_info(
                        &format!("Dry-run deploy for '{}' (no --broadcast)...", member),
                        quiet,
                    );
                } else {
                    print_info(
                        &format!("Broadcasting deployment of '{}'...", member),
                        quiet,
                    );
                }

                let mut cmd = std::process::Command::new("leo");
                cmd.args(["deploy", "--network", network_str, "-p", member]);

                for flag in &leo_cmd::json_output_flag(&args.json_output) {
                    cmd.arg(flag);
                }

                if let Some(pk) = &args.private_key {
                    cmd.args(["--private-key", pk]);
                }

                if let Some(path) = &args.path {
                    cmd.args(["--path", &path.to_string_lossy()]);
                }

                if let Some(ep) = endpoint {
                    cmd.args(["--endpoint", ep]);
                }

                if args.broadcast {
                    cmd.arg("--broadcast");
                }

                let status = cmd.status().with_context(|| {
                    format!("Failed to execute 'leo deploy -p {}'", member)
                })?;

                if !status.success() {
                    let code = status.code().unwrap_or(-1);
                    bail!("'leo deploy -p {}' failed with exit code {}", member, code);
                }
            }
        } else if has_package {
            // Deploy a single workspace member
            let member = args.package.as_ref().unwrap();

            if args.broadcast && matches!(network, Network::Mainnet) {
                println!(
                    "{} Deploying '{}' to MAINNET with --broadcast. This is irreversible and costs real fees.",
                    "[warning]".yellow().bold(),
                    member
                );
            }

            let mut cmd = std::process::Command::new("leo");
            cmd.args(["deploy", "--network", network_str, "-p", member]);

            for flag in &leo_cmd::json_output_flag(&args.json_output) {
                cmd.arg(flag);
            }

            if let Some(pk) = &args.private_key {
                cmd.args(["--private-key", pk]);
            }

            if let Some(path) = &args.path {
                cmd.args(["--path", &path.to_string_lossy()]);
            }

            if let Some(ep) = endpoint {
                cmd.args(["--endpoint", ep]);
            }

            if args.broadcast {
                cmd.arg("--broadcast");
            }

            let status = cmd.status().with_context(|| {
                format!("Failed to execute 'leo deploy -p {}'", member)
            })?;

            if !status.success() {
                let code = status.code().unwrap_or(-1);
                bail!("'leo deploy -p {}' failed with exit code {}", member, code);
            }
        }

        return Ok(());
    }

    // --- Single-project path (unchanged) ---
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

    let mut cmd = std::process::Command::new("leo");
    cmd.args(["deploy", "--network", network_str]);

    for flag in &json_flags {
        cmd.arg(flag);
    }

    if let Some(pk) = &args.private_key {
        cmd.args(["--private-key", pk]);
    }

    if let Some(path) = &args.path {
        cmd.args(["--path", &path.to_string_lossy()]);
    }

    if let Some(ep) = endpoint {
        cmd.args(["--endpoint", ep]);
    }

    if args.broadcast {
        cmd.arg("--broadcast");
    }

    let status = cmd.status().with_context(|| {
        format!("Failed to execute 'leo deploy --network {}'", network_str)
    })?;

    if !status.success() {
        let code = status.code().unwrap_or(-1);
        bail!("'leo deploy --network {}' failed with exit code {}", network_str, code);
    }

    Ok(())
}

fn handle_records(cmd: &RecordsCmd, quiet: bool, profile: Option<&str>) -> Result<()> {
    match cmd {
        RecordsCmd::List(args) => handle_records_list(args, quiet, profile),
    }
}

// ---------------------------------------------------------------------------
// Query command handlers
// ---------------------------------------------------------------------------

/// Build the argument list for `leo query <subcommand>`.
/// Appends subcommand-specific flags, then shared --network/--endpoint/--json-output.
fn build_query_args(
    subcommand: &str,
    sub_args: &[String],
    network: Option<&Network>,
    endpoint: Option<&str>,
    json_output: &Option<Option<PathBuf>>,
) -> Vec<String> {
    let mut args = vec![subcommand.to_string()];
    args.extend(sub_args.iter().cloned());

    if let Some(net) = network {
        let net_str = match net {
            Network::Testnet => "testnet",
            Network::Mainnet => "mainnet",
            Network::Canary => "canary",
        };
        args.push("--network".to_string());
        args.push(net_str.to_string());
    }
    if let Some(ep) = endpoint {
        args.push("--endpoint".to_string());
        args.push(ep.to_string());
    }

    let json_flags = leo_cmd::json_output_flag(json_output);
    args.extend(json_flags);

    args
}

fn handle_query(cmd: &QueryCmd, quiet: bool, profile: Option<&str>) -> Result<()> {
    match cmd {
        QueryCmd::Block(args) => handle_query_block(args, quiet, profile),
        QueryCmd::Transaction(args) => handle_query_transaction(args, quiet, profile),
        QueryCmd::Program(args) => handle_query_program(args, quiet, profile),
        QueryCmd::Stateroot(args) => handle_query_stateroot(args, quiet, profile),
        QueryCmd::Committee(args) => handle_query_committee(args, quiet, profile),
    }
}

fn handle_query_block(args: &QueryBlockArgs, quiet: bool, profile: Option<&str>) -> Result<()> {
    if !leo_cmd::leo_is_installed() {
        bail!("leo is not installed or not on PATH. Install it with: cargo binstall leo-lang");
    }

    let cfg = load_aleoflow_config();
    let profile_res = resolve_profile(profile, &cfg, quiet)?;

    let network = args.network.clone().or(profile_res.network).or_else(|| {
        cfg.default_network.as_deref().and_then(parse_network).inspect(|n| {
            if !quiet {
                let name = match n {
                    Network::Testnet => "testnet",
                    Network::Mainnet => "mainnet",
                    Network::Canary => "canary",
                };
                println!("{} Using default_network '{}' from aleo.toml", "[info]".cyan().bold(), name);
            }
        })
    }).or(Some(Network::Testnet));

    // Resolve endpoint: CLI --endpoint > --profile > hardcoded default
    let endpoint = args.endpoint.as_deref()
        .or(profile_res.endpoint.as_deref())
        .unwrap_or(DEFAULT_QUERY_ENDPOINT);

    let mut sub_args: Vec<String> = Vec::new();
    if let Some(ref id) = args.id {
        sub_args.push(id.clone());
    }
    if args.latest {
        sub_args.push("--latest".to_string());
    }
    if args.latest_hash {
        sub_args.push("--latest-hash".to_string());
    }
    if args.latest_height {
        sub_args.push("--latest-height".to_string());
    }
    if let Some(ref range) = args.range {
        sub_args.push("--range".to_string());
        sub_args.extend(range.iter().cloned());
    }
    if args.transactions {
        sub_args.push("--transactions".to_string());
    }
    if args.to_height {
        sub_args.push("--to-height".to_string());
    }

    let extra_args = build_query_args("block", &sub_args, network.as_ref(), Some(endpoint), &args.json_output);
    print_info(&format!("Running 'leo query block'..."), quiet);
    leo_cmd::run_leo_with("query", &extra_args, None)
}

fn handle_query_transaction(args: &QueryTransactionArgs, quiet: bool, profile: Option<&str>) -> Result<()> {
    if !leo_cmd::leo_is_installed() {
        bail!("leo is not installed or not on PATH. Install it with: cargo binstall leo-lang");
    }

    let cfg = load_aleoflow_config();
    let profile_res = resolve_profile(profile, &cfg, quiet)?;

    let network = args.network.clone().or(profile_res.network).or_else(|| {
        cfg.default_network.as_deref().and_then(parse_network).inspect(|n| {
            if !quiet {
                let name = match n {
                    Network::Testnet => "testnet",
                    Network::Mainnet => "mainnet",
                    Network::Canary => "canary",
                };
                println!("{} Using default_network '{}' from aleo.toml", "[info]".cyan().bold(), name);
            }
        })
    }).or(Some(Network::Testnet));

    // Resolve endpoint: CLI --endpoint > --profile > hardcoded default
    let endpoint = args.endpoint.as_deref()
        .or(profile_res.endpoint.as_deref())
        .unwrap_or(DEFAULT_QUERY_ENDPOINT);

    let mut sub_args: Vec<String> = Vec::new();
    if let Some(ref id) = args.id {
        sub_args.push(id.clone());
    }
    if args.confirmed {
        sub_args.push("--confirmed".to_string());
    }
    if args.unconfirmed {
        sub_args.push("--unconfirmed".to_string());
    }
    if let Some(ref io) = args.from_io {
        sub_args.push("--from-io".to_string());
        sub_args.push(io.clone());
    }
    if let Some(ref tid) = args.from_transition {
        sub_args.push("--from-transition".to_string());
        sub_args.push(tid.clone());
    }
    if let Some(ref pname) = args.from_program {
        sub_args.push("--from-program".to_string());
        sub_args.push(pname.clone());
    }

    let extra_args = build_query_args("transaction", &sub_args, network.as_ref(), Some(endpoint), &args.json_output);
    print_info(&format!("Running 'leo query transaction'..."), quiet);
    leo_cmd::run_leo_with("query", &extra_args, None)
}

fn handle_query_program(args: &QueryProgramArgs, quiet: bool, profile: Option<&str>) -> Result<()> {
    if !leo_cmd::leo_is_installed() {
        bail!("leo is not installed or not on PATH. Install it with: cargo binstall leo-lang");
    }

    let cfg = load_aleoflow_config();
    let profile_res = resolve_profile(profile, &cfg, quiet)?;

    let network = args.network.clone().or(profile_res.network).or_else(|| {
        cfg.default_network.as_deref().and_then(parse_network).inspect(|n| {
            if !quiet {
                let name = match n {
                    Network::Testnet => "testnet",
                    Network::Mainnet => "mainnet",
                    Network::Canary => "canary",
                };
                println!("{} Using default_network '{}' from aleo.toml", "[info]".cyan().bold(), name);
            }
        })
    }).or(Some(Network::Testnet));

    // Resolve endpoint: CLI --endpoint > --profile > hardcoded default
    let endpoint = args.endpoint.as_deref()
        .or(profile_res.endpoint.as_deref())
        .unwrap_or(DEFAULT_QUERY_ENDPOINT);

    let mut sub_args: Vec<String> = Vec::new();
    sub_args.push(args.name.clone());
    if let Some(ref ed) = args.edition {
        sub_args.push("--edition".to_string());
        sub_args.push(ed.to_string());
    }
    if args.mappings {
        sub_args.push("--mappings".to_string());
    }
    if let Some(ref mv) = args.mapping_value {
        sub_args.push("--mapping-value".to_string());
        sub_args.extend(mv.iter().cloned());
    }

    let extra_args = build_query_args("program", &sub_args, network.as_ref(), Some(endpoint), &args.json_output);
    print_info(&format!("Running 'leo query program'..."), quiet);
    leo_cmd::run_leo_with("query", &extra_args, None)
}

fn handle_query_stateroot(args: &QueryStaterootArgs, quiet: bool, profile: Option<&str>) -> Result<()> {
    if !leo_cmd::leo_is_installed() {
        bail!("leo is not installed or not on PATH. Install it with: cargo binstall leo-lang");
    }

    let cfg = load_aleoflow_config();
    let profile_res = resolve_profile(profile, &cfg, quiet)?;

    let network = args.network.clone().or(profile_res.network).or_else(|| {
        cfg.default_network.as_deref().and_then(parse_network).inspect(|n| {
            if !quiet {
                let name = match n {
                    Network::Testnet => "testnet",
                    Network::Mainnet => "mainnet",
                    Network::Canary => "canary",
                };
                println!("{} Using default_network '{}' from aleo.toml", "[info]".cyan().bold(), name);
            }
        })
    }).or(Some(Network::Testnet));

    // Resolve endpoint: CLI --endpoint > --profile > hardcoded default
    let endpoint = args.endpoint.as_deref()
        .or(profile_res.endpoint.as_deref())
        .unwrap_or(DEFAULT_QUERY_ENDPOINT);

    let extra_args = build_query_args("stateroot", &[], network.as_ref(), Some(endpoint), &args.json_output);
    print_info(&format!("Running 'leo query stateroot'..."), quiet);
    leo_cmd::run_leo_with("query", &extra_args, None)
}

fn handle_query_committee(args: &QueryCommitteeArgs, quiet: bool, profile: Option<&str>) -> Result<()> {
    if !leo_cmd::leo_is_installed() {
        bail!("leo is not installed or not on PATH. Install it with: cargo binstall leo-lang");
    }

    let cfg = load_aleoflow_config();
    let profile_res = resolve_profile(profile, &cfg, quiet)?;

    let network = args.network.clone().or(profile_res.network).or_else(|| {
        cfg.default_network.as_deref().and_then(parse_network).inspect(|n| {
            if !quiet {
                let name = match n {
                    Network::Testnet => "testnet",
                    Network::Mainnet => "mainnet",
                    Network::Canary => "canary",
                };
                println!("{} Using default_network '{}' from aleo.toml", "[info]".cyan().bold(), name);
            }
        })
    }).or(Some(Network::Testnet));

    // Resolve endpoint: CLI --endpoint > --profile > hardcoded default
    let endpoint = args.endpoint.as_deref()
        .or(profile_res.endpoint.as_deref())
        .unwrap_or(DEFAULT_QUERY_ENDPOINT);

    let extra_args = build_query_args("committee", &[], network.as_ref(), Some(endpoint), &args.json_output);
    print_info(&format!("Running 'leo query committee'..."), quiet);
    leo_cmd::run_leo_with("query", &extra_args, None)
}

fn handle_account(cmd: &AccountCmd, quiet: bool) -> Result<()> {
    match cmd {
        AccountCmd::New(args) => handle_account_new(args, quiet),
        AccountCmd::Import(args) => handle_account_import(args, quiet),
        AccountCmd::Sign(args) => handle_account_sign(args, quiet),
        AccountCmd::Verify(args) => handle_account_verify(args, quiet),
        AccountCmd::Decrypt(args) => handle_account_decrypt(args, quiet),
    }
}

fn handle_account_new(args: &AccountNewArgs, quiet: bool) -> Result<()> {
    if !leo_cmd::leo_is_installed() {
        bail!("leo is not installed or not on PATH. Install it with: cargo binstall leo-lang");
    }

    let mut cmd = std::process::Command::new("leo");
    cmd.args(["account", "new"]);

    if let Some(seed) = args.seed {
        cmd.args(["--seed", &seed.to_string()]);
    }
    if args.write {
        cmd.arg("--write");
    }
    if args.discreet {
        cmd.arg("--discreet");
    }
    cmd.args(["--network", &args.network]);
    if let Some(ref endpoint) = args.endpoint {
        cmd.args(["--endpoint", endpoint]);
    }

    if !quiet {
        println!("{} Generating new Aleo account...", "[info]".cyan().bold());
    }

    let status = cmd.status().with_context(|| "Failed to execute 'leo account new'")?;
    if !status.success() {
        bail!("'leo account new' failed with exit code {}", status.code().unwrap_or(-1));
    }
    Ok(())
}

fn handle_account_import(args: &AccountImportArgs, quiet: bool) -> Result<()> {
    if !leo_cmd::leo_is_installed() {
        bail!("leo is not installed or not on PATH. Install it with: cargo binstall leo-lang");
    }

    // Validate private key format before shelling out to leo
    if let Some(ref pk) = args.private_key {
        if let Some(reason) = validate_private_key_format(pk) {
            bail!("{}", reason);
        }
    }

    let mut cmd = std::process::Command::new("leo");
    cmd.args(["account", "import"]);

    if let Some(ref pk) = args.private_key {
        cmd.arg(pk);
    }
    if args.write {
        cmd.arg("--write");
    }
    if args.discreet {
        cmd.arg("--discreet");
    }
    cmd.args(["--network", &args.network]);
    if let Some(ref endpoint) = args.endpoint {
        cmd.args(["--endpoint", endpoint]);
    }

    if !quiet {
        println!("{} Importing Aleo account...", "[info]".cyan().bold());
    }

    let status = cmd.status().with_context(|| "Failed to execute 'leo account import'")?;
    if !status.success() {
        bail!("'leo account import' failed with exit code {}", status.code().unwrap_or(-1));
    }
    Ok(())
}

fn handle_account_sign(args: &AccountSignArgs, quiet: bool) -> Result<()> {
    if !leo_cmd::leo_is_installed() {
        bail!("leo is not installed or not on PATH. Install it with: cargo binstall leo-lang");
    }

    // Validate private key format before shelling out to leo
    if let Some(ref pk) = args.private_key {
        if let Some(reason) = validate_private_key_format(pk) {
            bail!("{}", reason);
        }
    }

    let mut cmd = std::process::Command::new("leo");
    cmd.args(["account", "sign", "--message", &args.message]);

    if let Some(ref pk) = args.private_key {
        cmd.args(["--private-key", pk]);
    }
    if let Some(ref pk_file) = args.private_key_file {
        cmd.args(["--private-key-file", &pk_file.to_string_lossy()]);
    }
    if args.raw {
        cmd.arg("--raw");
    }

    if !quiet {
        println!("{} Signing message...", "[info]".cyan().bold());
    }

    let status = cmd.status().with_context(|| "Failed to execute 'leo account sign'")?;
    if !status.success() {
        bail!("'leo account sign' failed with exit code {}", status.code().unwrap_or(-1));
    }
    Ok(())
}

fn handle_account_verify(args: &AccountVerifyArgs, quiet: bool) -> Result<()> {
    if !leo_cmd::leo_is_installed() {
        bail!("leo is not installed or not on PATH. Install it with: cargo binstall leo-lang");
    }

    let mut cmd = std::process::Command::new("leo");
    cmd.args([
        "account", "verify",
        "--address", &args.address,
        "--signature", &args.signature,
        "--message", &args.message,
    ]);

    if args.raw {
        cmd.arg("--raw");
    }

    if !quiet {
        println!("{} Verifying message signature...", "[info]".cyan().bold());
    }

    let status = cmd.status().with_context(|| "Failed to execute 'leo account verify'")?;
    if !status.success() {
        bail!("'leo account verify' failed with exit code {}", status.code().unwrap_or(-1));
    }
    Ok(())
}

fn handle_account_decrypt(args: &AccountDecryptArgs, quiet: bool) -> Result<()> {
    if !leo_cmd::leo_is_installed() {
        bail!("leo is not installed or not on PATH. Install it with: cargo binstall leo-lang");
    }

    // Validate key format before shelling out to leo.
    // Use the shared function for the angle-bracket / placeholder check,
    // then allow both private key (APrivateKey1...) and view key (AViewKey1...) prefixes.
    if let Some(ref key) = args.key {
        // Delegate placeholder detection to the shared validation function
        if let Some(reason) = validate_private_key_format(key) {
            // If it's just a prefix/length error, that's OK for view keys;
            // only bail if it's an angle-bracket placeholder issue.
            if reason.contains('<') || reason.contains('>') || reason.contains("placeholder") {
                bail!("{}", reason);
            }
            // For prefix/length errors, also allow view key prefix
            if !key.starts_with("AViewKey1") {
                bail!("{}", reason);
            }
        }
    }

    let mut cmd = std::process::Command::new("leo");
    cmd.args(["account", "decrypt", "--ciphertext", &args.ciphertext]);

    if let Some(ref key) = args.key {
        cmd.args(["-k", key]);
    }
    if let Some(ref key_file) = args.key_file {
        cmd.args(["-f", &key_file.to_string_lossy()]);
    }

    if !quiet {
        println!("{} Decrypting record ciphertext...", "[info]".cyan().bold());
    }

    let status = cmd.status().with_context(|| "Failed to execute 'leo account decrypt'")?;
    if !status.success() {
        bail!("'leo account decrypt' failed with exit code {}", status.code().unwrap_or(-1));
    }
    Ok(())
}

fn handle_records_list(args: &RecordsListArgs, quiet: bool, profile: Option<&str>) -> Result<()> {
    if !leo_cmd::snarkos_is_installed() {
        bail!(
            "snarkos is not installed or not on PATH. Install it with: \
             leo devnet --snarkos <path> --install"
        );
    }

    // Resolve endpoint: CLI --endpoint > --profile > built-in default
    let cfg = load_aleoflow_config();
    let profile_res = resolve_profile(profile, &cfg, quiet)?;
    let endpoint = args.endpoint.clone()
        .or(profile_res.endpoint)
        .unwrap_or_else(|| "http://localhost:3030".to_string());

    print_info(
        "Scanning for records via snarkOS. This requires a locally running snarkOS \
         node (e.g. via 'leo devnet') -- it will not work against the public \
         testnet API.",
        quiet,
    );

    let mut cmd = std::process::Command::new("snarkos");
    cmd.args([
        "developer",
        "scan",
        "--view-key",
        &args.view_key,
        "--start",
        &args.start.to_string(),
        "--end",
        &args.end.to_string(),
        "--endpoint",
        &endpoint,
    ]);

    let status = cmd.status().with_context(|| {
        "Failed to execute 'snarkos developer scan'".to_string()
    })?;

    if !status.success() {
        let code = status.code().unwrap_or(-1);
        bail!("'snarkos developer scan' failed with exit code {}", code);
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Remote bindings: helpers for fetching and parsing .aleo assembly
// ---------------------------------------------------------------------------

/// Fetch a deployed program from the explorer API, with local caching.
fn fetch_and_cache_program(
    program_id: &str,
    network: Network,
    quiet: bool,
) -> Result<String> {
    let network_str = match network {
        Network::Testnet => "testnet",
        Network::Mainnet => "mainnet",
        Network::Canary => "canary",
    };

    let cache_dir = Path::new(".aleoflow-cache").join(network_str);
    let cache_path = cache_dir.join(program_id);

    // Check cache first
    if cache_path.exists() {
        let cached = fs::read_to_string(&cache_path)?;
        print_info(
            &format!(
                "Using cached program '{}' from '{}'",
                program_id,
                cache_path.display()
            ),
            quiet,
        );
        return Ok(cached);
    }

    // Fetch from network
    let url = format!(
        "https://api.explorer.provable.com/v1/{}/program/{}",
        network_str, program_id
    );
    print_info(
        &format!(
            "Fetching program '{}' from {}",
            program_id, url
        ),
        quiet,
    );

    let response = reqwest::blocking::get(&url)
        .with_context(|| format!("Failed to fetch program from '{}'", url))?;

    if !response.status().is_success() {
        bail!(
            "Failed to fetch program '{}' from {} (HTTP {})",
            program_id, url, response.status()
        );
    }

    let raw = response.text()?;

    // The API response wraps the .aleo source in a JSON string literal.
    // Strip surrounding quotes and unescape if needed.
    let text = if raw.starts_with('"') && raw.ends_with('"') {
        serde_json::from_str(&raw)
            .unwrap_or(raw.trim_matches('"').to_string())
    } else {
        raw
    };

    // Cache the result
    fs::create_dir_all(&cache_dir)?;
    fs::write(&cache_path, &text)?;
    print_info(
        &format!("Cached program to '{}'", cache_path.display()),
        quiet,
    );

    Ok(text)
}

/// Parse .aleo assembly text and extract function/transition declarations with their
/// input parameters. Uses register-based format:
///   function <name>:
///       input r0 as <type>.<visibility>;
///       ...
fn parse_aleo_assembly(text: &str) -> Vec<AleoFunction> {
    let mut functions: Vec<AleoFunction> = Vec::new();

    for (line_idx, line) in text.lines().enumerate() {
        let trimmed = line.trim();

        // Match `function <name>:` or `transition <name>:` declarations
        let is_function = trimmed.starts_with("function ")
            || trimmed.starts_with("transition ");
        if !is_function {
            continue;
        }

        // Extract function name (stop at `:` or `(` or whitespace after keyword)
        let after_keyword = if trimmed.starts_with("function ") {
            &trimmed[9..]
        } else {
            &trimmed[11..] // "transition "
        };
        let name = after_keyword
            .split(|c: char| c == ':' || c == '(' || c == ' ')
            .next()
            .unwrap_or("")
            .trim()
            .to_string();

        if name.is_empty() {
            continue;
        }

        // Collect input lines until next function/record/struct/finalize/mapping/empty line
        let mut inputs: Vec<AleoParam> = Vec::new();
        let mut j = line_idx + 1;
        while j < text.lines().count() {
            let next_line = text.lines().nth(j).unwrap_or("").trim().to_string();

            // Stop at known section boundaries
            if next_line.starts_with("function ")
                || next_line.starts_with("finalize ")
                || next_line.starts_with("transition ")
                || next_line.starts_with("record ")
                || next_line.starts_with("struct ")
                || next_line.starts_with("mapping ")
                || next_line.starts_with("constructor")
                || next_line.is_empty()
            {
                break;
            }

            // Parse `input r<N> as <type>.<visibility>;`
            if let Some(rest) = next_line
                .strip_prefix("input ")
                .and_then(|s| s.find(" as ").map(|pos| (s, pos)))
            {
                let after_as = &rest.0[rest.1 + 4..]; // skip " as "
                let ty_vis = after_as.trim_end_matches(';').trim();
                if let Some(dot) = ty_vis.rfind('.') {
                    let param_type = ty_vis[..dot].to_string();
                    let visibility = ty_vis[dot + 1..].to_string();
                    inputs.push(AleoParam {
                        param_type,
                        visibility,
                    });
                }
            }

            j += 1;
        }

        functions.push(AleoFunction {
            name,
            inputs,
        });
    }

    functions
}

/// Map an .aleo assembly type string to a TypeScript type name.
fn aleo_type_to_ts_type(ty: &str) -> &'static str {
    match ty {
        "address" => "string",
        "boolean" => "boolean",
        "u8" | "u16" | "u32" => "number",
        "u64" | "u128" => "bigint",
        "i8" | "i16" | "i32" => "number",
        "i64" | "i128" => "bigint",
        "field" | "scalar" => "bigint",
        "group" | "signature" | "string" => "string",
        _ => "string", // struct/record types pass through as strings
    }
}

/// Build a converter expression for an .aleo assembly type string.
fn aleo_type_converter(ty: &str, var_name: &str) -> String {
    match ty {
        "address" => var_name.to_string(),
        "boolean" => format!("toBoolean({})", var_name),
        "u8" => format!("toU8({})", var_name),
        "u16" => format!("toU16({})", var_name),
        "u32" => format!("toU32({})", var_name),
        "u64" => format!("toU64({})", var_name),
        "u128" => format!("toU128({})", var_name),
        "i8" => format!("toI8({})", var_name),
        "i16" => format!("toI16({})", var_name),
        "i32" => format!("toI32({})", var_name),
        "i64" => format!("toI64({})", var_name),
        "i128" => format!("toI128({})", var_name),
        "field" => format!("toField({})", var_name),
        "scalar" => format!("toScalar({})", var_name),
        "group" | "signature" | "string" => var_name.to_string(),
        _ => format!("String({})", var_name),
    }
}

// ---------------------------------------------------------------------------
// Shared TypeScript binding generation template
// ---------------------------------------------------------------------------

/// Generate TypeScript bindings from parsed function info and write to disk.
fn generate_ts_bindings(
    program_name: &str,
    program_id: &str,
    functions: &[FuncInfo],
    source_desc: &str,
    network: &str,
    output_path: &Path,
    is_remote: bool,
) -> Result<()> {
    let mut ts = String::new();

    // Header comment
    ts.push_str("// Auto-generated TypeScript bindings for '");
    ts.push_str(program_name);
    ts.push_str("'\n");
    ts.push_str("//\n");
    ts.push_str("// This file generates real, API-correct execution calls based on the\n");
    ts.push_str("// documented buildExecutionTransaction interface from @provablehq/sdk.\n");
    ts.push_str("//\n");
    ts.push_str(&format!("// Source: {} | Network: {}\n", source_desc, network));
    ts.push_str(&format!("// Explorer: https://explorer.aleo.org/program/{}\n", program_name));
    ts.push_str("//\n");
    ts.push_str("// REQUIRED ENVIRONMENT VARIABLES:\n");
    ts.push_str("//   PRIVATE_KEY    - The Aleo private key for transaction execution\n");
    ts.push_str("//   ALEO_ENDPOINT  - The Aleo network endpoint URL (e.g. https://api.provable.com/v2)\n");
    ts.push_str("//\n");
    ts.push_str("// Record-typed parameters need additional manual wiring -- see\n");
    ts.push_str("// https://developer.aleo.org/sdk/typescript/program_manager/ for details.\n");
    ts.push_str("//\n");
    if is_remote {
        ts.push_str("// Note: Parameter names are generic (arg0, arg1...) because compiled .aleo\n");
        ts.push_str("// assembly does not preserve original source-level names. For\n");
        ts.push_str("// semantically-named bindings, use --path <local_project> instead.\n");
        ts.push_str("//\n");
    }
    ts.push_str("// Generated by AleoFlow bindings\n");
    ts.push_str("\n");

    // Imports
    ts.push_str("import {\n");
    ts.push_str("  AleoKeyProvider,\n");
    ts.push_str("  AleoNetworkClient,\n");
    ts.push_str("  ProgramManager,\n");
    ts.push_str("  NetworkRecordProvider,\n");
    ts.push_str("  Account,\n");
    ts.push_str("  initializeWasm,\n");
    ts.push_str("} from \"@provablehq/sdk\";\n");
    ts.push_str("\n");

    // Shared setup block
    ts.push_str("// ---------------------------------------------------------------------------\n");
    ts.push_str("// Shared setup: initialize WASM and create the ProgramManager\n");
    ts.push_str("// ---------------------------------------------------------------------------\n");
    ts.push_str("\n");
    ts.push_str("let _initialized = false;\n");
    ts.push_str("let _programManager: ProgramManager | null = null;\n");
    ts.push_str("\n");
    ts.push_str("async function getProgramManager(): Promise<ProgramManager> {\n");
    ts.push_str("  if (!_initialized) {\n");
    ts.push_str("    await initializeWasm();\n");
    ts.push_str("    _initialized = true;\n");
    ts.push_str("  }\n");
    ts.push_str("  if (!_programManager) {\n");
    ts.push_str("    const keyProvider = new AleoKeyProvider();\n");
    ts.push_str("    keyProvider.useCache = true;\n");
    ts.push_str("\n");
    ts.push_str("    const account = new Account({ privateKey: process.env.PRIVATE_KEY });\n");
    ts.push_str("    const networkClient = new AleoNetworkClient(process.env.ALEO_ENDPOINT);\n");
    ts.push_str("    const recordProvider = new NetworkRecordProvider(account, networkClient);\n");
    ts.push_str("\n");
    ts.push_str("    _programManager = new ProgramManager(process.env.ALEO_ENDPOINT, keyProvider, recordProvider);\n");
    ts.push_str("  }\n");
    ts.push_str("  return _programManager;\n");
    ts.push_str("}\n");
    ts.push_str("\n");

    // Type conversion helpers
    ts.push_str("// ---------------------------------------------------------------------------\n");
    ts.push_str("// Type conversion helpers\n");
    ts.push_str("// Convert TypeScript values to Leo-formatted strings for @provablehq/sdk\n");
    ts.push_str("// ---------------------------------------------------------------------------\n");
    ts.push_str("\n");
    ts.push_str("function toU8(n: number): string { return `${n}u8`; }\n");
    ts.push_str("function toU16(n: number): string { return `${n}u16`; }\n");
    ts.push_str("function toU32(n: number): string { return `${n}u32`; }\n");
    ts.push_str("function toU64(n: bigint): string { return `${n}u64`; }\n");
    ts.push_str("function toU128(n: bigint): string { return `${n}u128`; }\n");
    ts.push_str("function toI8(n: number): string { return `${n}i8`; }\n");
    ts.push_str("function toI16(n: number): string { return `${n}i16`; }\n");
    ts.push_str("function toI32(n: number): string { return `${n}i32`; }\n");
    ts.push_str("function toI64(n: bigint): string { return `${n}i64`; }\n");
    ts.push_str("function toI128(n: bigint): string { return `${n}i128`; }\n");
    ts.push_str("function toBoolean(b: boolean): string { return `${b}`; }\n");
    ts.push_str("function toAddress(s: string): string { return s; }\n");
    ts.push_str("function toField(n: bigint): string { return `${n}field`; }\n");
    ts.push_str("function toScalar(n: bigint): string { return `${n}scalar`; }\n");
    ts.push_str("function toGroup(s: string): string { return s; }\n");
    ts.push_str("function toSignature(s: string): string { return s; }\n");
    ts.push_str("\n");

    ts.push_str(&format!("// --- Program: {} ---\n", program_name));
    ts.push_str("\n");

    for func in functions {
        let mut params: Vec<String> = Vec::new();
        let mut conversions: Vec<String> = Vec::new();

        for param in &func.params {
            params.push(format!("{}: {}", param.name, param.ts_type));

            if param.is_record {
                conversions.push(format!(
                    "      // TODO: Record-typed input '{}' requires fetching via RecordProvider before use.\n      // See https://developer.aleo.org/sdk/typescript/program_manager/\n      {} as unknown as string",
                    param.name, param.name
                ));
            } else {
                conversions.push(format!("      {}", param.converter_expr));
            }
        }

        ts.push_str(&format!("// {}\n", func.name));
        ts.push_str(&format!(
            "export async function {}(\n  {}\n): Promise<{{ success: true; txId: string }} | {{ success: false; error: string }}> {{\n",
            func.name,
            params.join(",\n  ")
        ));
        ts.push_str("  try {\n");
        ts.push_str("    const pm = await getProgramManager();\n");
        ts.push_str("    const tx = await pm.buildExecutionTransaction({\n");
        ts.push_str(&format!("      programName: \"{}\",\n", program_name));
        ts.push_str(&format!("      functionName: \"{}\",\n", func.name));
        ts.push_str("      priorityFee: 0.0,\n");
        ts.push_str("      privateFee: false,\n");
        ts.push_str("      inputs: [\n");
        for conv in &conversions {
            ts.push_str(conv);
            ts.push_str(",\n");
        }
        ts.push_str("      ],\n");
        ts.push_str(&format!("      keySearchParams: {{ cacheKey: \"{}:{}\" }},\n", program_id, func.name));
        ts.push_str("    });\n");
        ts.push_str("    const txId = await pm.networkClient.submitTransaction(tx.toString());\n");
        ts.push_str("    return { success: true, txId };\n");
        ts.push_str("  } catch (error) {\n");
        ts.push_str("    return { success: false, error: error instanceof Error ? error.message : String(error) };\n");
        ts.push_str("  }\n");
        ts.push_str("}\n\n");
    }

    // Create parent directory
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)?;
    }

    fs::write(output_path, &ts).with_context(|| {
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
// Bindings command handler (local + remote)
// ---------------------------------------------------------------------------

fn handle_bindings(args: &BindingsArgs, quiet: bool) -> Result<()> {
    // --- Remote path ---
    if let Some(ref remote_program_id) = args.remote {
        let network = args.network.clone().unwrap_or(Network::Testnet);
        let network_str = match network {
            Network::Testnet => "testnet",
            Network::Mainnet => "mainnet",
            Network::Canary => "canary",
        };

        // Determine source BEFORE fetching (fetch_and_cache_program writes cache)
        let was_cached = Path::new(".aleoflow-cache")
            .join(network_str)
            .join(remote_program_id)
            .exists();
        let source_desc = if was_cached { "local cache" } else { "network fetch" };

        // Fetch program text (cached or from network)
        let text = fetch_and_cache_program(remote_program_id, network, quiet)?;

        // Parse .aleo assembly
        let aleo_funcs = parse_aleo_assembly(&text);

        let program_name = remote_program_id.to_string();
        let program_id = program_name.trim_end_matches(".aleo").to_string();

        // Build FuncInfo from parsed AleoFunction
        let mut func_infos: Vec<FuncInfo> = Vec::new();
        for af in &aleo_funcs {
            let mut params: Vec<FuncParam> = Vec::new();
            for (i, input) in af.inputs.iter().enumerate() {
                let is_record = input.param_type.contains(".record");
                let ts_type = if is_record {
                    format!(
                        "string /* record {} */",
                        input.param_type.trim_end_matches(".record")
                    )
                } else {
                    aleo_type_to_ts_type(&input.param_type).to_string()
                };
                let converter_expr = if is_record {
                    format!("{} as unknown as string", format!("arg{}", i))
                } else {
                    let clean_type = input.param_type.trim_end_matches(".public")
                        .trim_end_matches(".private");
                    aleo_type_converter(clean_type, &format!("arg{}", i))
                };

                params.push(FuncParam {
                    name: format!("arg{}", i),
                    ts_type,
                    converter_expr,
                    is_record,
                });
            }
            func_infos.push(FuncInfo {
                name: af.name.clone(),
                params,
            });
        }

        // Determine output path: bindings/<program_id>.ts in cwd
        let output_path = if let Some(out) = &args.output {
            out.clone()
        } else {
            Path::new("bindings").join(format!("{}.ts", program_id))
        };

        generate_ts_bindings(
            &program_name,
            &program_id,
            &func_infos,
            source_desc,
            network_str,
            &output_path,
            true,
        )?;

        print_info(
            "Remote bindings use generic parameter names (arg0, arg1...) since compiled \
             bytecode does not preserve source-level names. Use --path <local_project> \
             for named parameters.",
            quiet,
        );

        return Ok(());
    }

    // --- Local path (unchanged behavior) ---
    let project_dir = args.path.as_deref().context(
        "Project path is required for local bindings"
    )?;
    if !project_dir.is_dir() {
        bail!("Project directory '{}' does not exist", project_dir.display());
    }

    let program_json_path = project_dir.join("program.json");
    let program_json_str = fs::read_to_string(&program_json_path)
        .with_context(|| format!("Failed to read '{}'", program_json_path.display()))?;
    let program_json: serde_json::Value = serde_json::from_str(&program_json_str)
        .context("Failed to parse program.json")?;
    let program_name = program_json["program"]
        .as_str()
        .context("program.json is missing the 'program' field")?;
    let program_id = program_name.trim_end_matches(".aleo");

    let abi_path = project_dir.join("build").join(program_id).join("abi.json");
    let abi_content = if abi_path.exists() {
        fs::read_to_string(&abi_path)?
    } else {
        print_info(
            &format!("No ABI found at '{}'. Running 'leo build' first...", abi_path.display()),
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

    let leo_source_path = project_dir.join("src").join("main.leo");
    let leo_source = if leo_source_path.exists() {
        Some(fs::read_to_string(&leo_source_path)?)
    } else {
        None
    };

    // Build FuncInfo from local ABI functions
    let mut func_infos: Vec<FuncInfo> = Vec::new();
    for func in functions {
        let name = func["name"].as_str().unwrap_or("unknown");
        let inputs = func["inputs"].as_array().map(|v| &v[..]).unwrap_or(&[]);
        let param_names = leo_source.as_deref()
            .and_then(|src| leo_param_names(src, name))
            .unwrap_or_default();

        let mut params: Vec<FuncParam> = Vec::new();
        for (i, input) in inputs.iter().enumerate() {
            let ts_type = param_leo_type(input);
            let pname: String = param_names.get(i)
                .cloned()
                .unwrap_or_else(|| format!("arg{}", i));

            let (converter_expr, is_record) = if input.get("Record").is_some() {
                (format!(
                    "// TODO: Record-typed input '{}' requires fetching via RecordProvider before use.\n      // See https://developer.aleo.org/sdk/typescript/program_manager/\n      {} as unknown as string",
                    pname, pname
                ), true)
            } else if let Some(pt) = input.get("Plaintext") {
                (leo_type_converter_expr(&pt["ty"], &pname), false)
            } else {
                (format!("String({})", pname), false)
            };

            params.push(FuncParam {
                name: pname,
                ts_type,
                converter_expr,
                is_record,
            });
        }
        func_infos.push(FuncInfo {
            name: name.to_string(),
            params,
        });
    }

    let output_path = if let Some(out) = &args.output {
        out.clone()
    } else {
        project_dir
            .join("bindings")
            .join(format!("{}.ts", program_id))
    };

    generate_ts_bindings(
        program_name,
        program_id,
        &func_infos,
        "local build",
        "local",
        &output_path,
        false,
    )?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Run / Execute command helpers
// ---------------------------------------------------------------------------

/// Build the argument list for `leo run` or `leo execute`.
/// Returns the extra flags (name, inputs, --network, --endpoint,
/// --json-output) to be passed to run_leo_with. The project path is
/// handled by run_leo_with via current_dir (same pattern as build/test).
fn build_leo_run_args(
    name: &str,
    inputs: &[String],
    network: Option<&Network>,
    endpoint: Option<&str>,
    json_output: &Option<Option<PathBuf>>,
    private_key: Option<&str>,
) -> Vec<String> {
    let mut args = vec![name.to_string()];
    args.extend(inputs.iter().cloned());

    if let Some(pk) = private_key {
        args.push("--private-key".to_string());
        args.push(pk.to_string());
    }

    if let Some(net) = network {
        let net_str = match net {
            Network::Testnet => "testnet",
            Network::Mainnet => "mainnet",
            Network::Canary => "canary",
        };
        args.push("--network".to_string());
        args.push(net_str.to_string());
    }
    if let Some(ep) = endpoint {
        args.push("--endpoint".to_string());
        args.push(ep.to_string());
    }

    let json_flags = leo_cmd::json_output_flag(json_output);
    args.extend(json_flags);

    args
}

// ---------------------------------------------------------------------------
// Best-effort error translation for run/execute commands
// ---------------------------------------------------------------------------

/// Try to find the .leo source file for a given function name in a project
/// directory and return the parameter names for that function.
fn resolve_func_param_names(func_name: &str, project_dir: Option<&Path>) -> Option<Vec<String>> {
    let search_dir = match project_dir {
        Some(d) => d.to_path_buf(),
        None => std::env::current_dir().ok()?,
    };

    // Look for .leo files in <project>/src/ and <project>/
    let mut candidates = Vec::new();
    let src_dir = search_dir.join("src");
    if src_dir.is_dir() {
        if let Ok(entries) = fs::read_dir(&src_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().map_or(false, |e| e == "leo") {
                    candidates.push(path);
                }
            }
        }
    }
    // Also check the project root itself
    if let Ok(entries) = fs::read_dir(&search_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().map_or(false, |e| e == "leo")
                && !candidates.contains(&path)
            {
                candidates.push(path);
            }
        }
    }

    for leo_path in &candidates {
        let content = fs::read_to_string(leo_path).ok()?;
        if let Some(names) = leo_param_names(&content, func_name) {
            if !names.is_empty() {
                return Some(names);
            }
        }
    }
    None
}

/// Map a register (e.g. "r0") to a parameter name from the source, or fall
/// back to "argument <N>" if the source can't be parsed.
fn register_to_name(register: &str, func_name: &str, project_dir: Option<&Path>) -> String {
    // Parse the register number: "r0" -> 0, "r1" -> 1, etc.
    let idx = register
        .strip_prefix('r')
        .and_then(|s| s.parse::<usize>().ok());
    let idx = match idx {
        Some(i) => i,
        None => return format!("register {}", register),
    };

    // Try to resolve via leo source parsing
    if let Some(param_names) = resolve_func_param_names(func_name, project_dir) {
        if let Some(name) = param_names.get(idx) {
            return name.clone();
        }
    }

    format!("argument {}", idx)
}

/// Try to parse an assert.neq / assert.eq failure from captured stderr.
/// Returns a friendly summary string if matched, None otherwise.
fn try_translate_assert_error(stderr: &str, func_name: &str, project_dir: Option<&Path>) -> Option<String> {
    // Expected format:
    // Instruction (<opcode> <r#> <val>;) at index <N> failed: '<opcode>' failed: '<val1>' <comparison> '<val2>' (<expectation>)

    let instr_start = stderr.find("Instruction (")?;
    let after_instr = &stderr[instr_start + 13..]; // skip "Instruction ("

    // Find the closing ";)" of the instruction
    let paren_close = after_instr.find(";)")?;
    let instr_body = &after_instr[..paren_close];

    // Parse: <opcode> <r#> <val>
    let parts: Vec<&str> = instr_body.split_whitespace().collect();
    if parts.len() < 3 {
        return None;
    }
    let opcode = parts[0];
    let register = parts[1];
    let _val1 = parts[2];

    // Find "' failed: '" to locate the comparison details
    let failed_marker = "' failed: '";
    let failed_pos = after_instr.find(failed_marker)?;
    let after_failed = &after_instr[failed_pos + failed_marker.len()..];

    // Extract val1: it starts right after the marker until the next "'"
    let val1_end = after_failed.find('\'')?;
    let actual_val1 = &after_failed[..val1_end];

    // Skip "' " to get the comparison phrase
    let after_val1 = &after_failed[val1_end + 2..]; // skip "' "
    let comparison_end = after_val1.find('\'')?;
    let _comparison = &after_val1[..comparison_end];

    // Skip "' " to get val2
    let after_comp = &after_val1[comparison_end + 2..]; // skip "' "
    let val2_end = after_comp.find('\'')?;
    let actual_val2 = &after_comp[..val2_end];

    // Extract expectation from the trailing (...) 
    let paren_start = stderr.rfind('(')?;
    let paren_end = stderr.rfind(')')?;
    let _expectation = if paren_start < paren_end {
        &stderr[paren_start + 1..paren_end]
    } else {
        ""
    };

    let param_name = register_to_name(register, func_name, project_dir);

    let is_neq = opcode.contains("neq");
    let expected_phrase = if is_neq {
        format!("to not equal {}", actual_val2)
    } else {
        format!("to equal {}", actual_val2)
    };

    Some(format!(
        "\n{} Assertion failed: expected '{}' {}, but it was {}.\
         \n{} This is a best-effort translation of the raw AVM error above \
         -- always check the full output if this doesn't match what you expect.\n",
        "[aleoflow]".cyan().bold(),
        param_name,
        expected_phrase,
        actual_val1,
        "[aleoflow]".cyan().bold(),
    ))
}

/// Best-effort translation of known leo run/execute error patterns.
/// Prints a friendly summary block AFTER leo's raw output.
/// Falls through silently for unrecognized patterns.
fn translate_run_execute_error(stderr: &str, func_name: &str, project_dir: Option<&Path>) {
    // 1. Assert.neq / assert.eq failure
    if stderr.contains("Instruction (") && (stderr.contains("assert.neq") || stderr.contains("assert.eq")) {
        if let Some(msg) = try_translate_assert_error(stderr, func_name, project_dir) {
            println!("{}", msg);
            return;
        }
    }

    // 2. PRIVATE_KEY missing
    if stderr.contains("Failed to load 'PRIVATE_KEY'") {
        println!(
            "  {} Set PRIVATE_KEY via 'aleoflow account new' or your .env file, then retry.",
            "[aleoflow]".cyan().bold()
        );
        return;
    }

    // 3. Insufficient balance
    if stderr.contains("insufficient to pay the base fee") {
        println!(
            "  {} The account does not have enough balance to cover the transaction fee. \
             Fund the account or use a different key.",
            "[aleoflow]".cyan().bold()
        );
        return;
    }

    // 4. Connection refused
    if stderr.contains("Connection refused") {
        println!(
            "  {} Could not connect to the endpoint. Check that the endpoint URL is \
             correct and reachable. Use --endpoint or --profile to set a different endpoint.",
            "[aleoflow]".cyan().bold()
        );
        return;
    }

    // 5. Invalid project path
    if stderr.contains("failed to load Leo project") {
        println!(
            "  {} The specified path does not contain a valid Leo project. \
             Use --path to point to a directory with program.json.",
            "[aleoflow]".cyan().bold()
        );
        return;
    }
}

fn handle_run(args: &RunArgs, quiet: bool, profile: Option<&str>) -> Result<()> {
    // Validate private key format before any subprocess runs
    if let Some(ref pk) = args.private_key {
        if let Some(reason) = validate_private_key_format(pk) {
            bail!("{}", reason);
        }
    }

    if !leo_cmd::leo_is_installed() {
        bail!(
            "leo is not installed or not on PATH. Install it with: cargo binstall leo-lang"
        );
    }

    let cfg = load_aleoflow_config();
    let profile_res = resolve_profile(profile, &cfg, quiet)?;

    // Resolve network: CLI --network > --profile > config > default (Testnet)
    let network = args.network.clone().or(profile_res.network).or_else(|| {
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
    }).or(Some(Network::Testnet));

    // Resolve endpoint: CLI --endpoint > --profile
    let endpoint = args.endpoint.as_deref().or(profile_res.endpoint.as_deref());

    let dir = args.path.as_deref();
    let private_key = args.private_key.as_deref();
    let extra_args = build_leo_run_args(
        &args.name,
        &args.inputs,
        network.as_ref(),
        endpoint,
        &args.json_output,
        private_key,
    );

    print_info(&format!("Running 'leo run {}'...", args.name), quiet);

    // Run with stderr capture for best-effort error translation
    let (result, captured_stderr) = leo_cmd::run_leo_captured("run", &extra_args, dir);
    if let Err(_e) = &result {
        translate_run_execute_error(&captured_stderr, &args.name, args.path.as_deref());
    }
    result
}

fn handle_execute(args: &ExecuteArgs, quiet: bool, profile: Option<&str>) -> Result<()> {
    // Validate private key format before any subprocess runs
    if let Some(ref pk) = args.private_key {
        if let Some(reason) = validate_private_key_format(pk) {
            bail!("{}", reason);
        }
    }

    if !leo_cmd::leo_is_installed() {
        bail!(
            "leo is not installed or not on PATH. Install it with: cargo binstall leo-lang"
        );
    }

    let cfg = load_aleoflow_config();
    let profile_res = resolve_profile(profile, &cfg, quiet)?;

    // Resolve network: CLI --network > --profile > config > default (Testnet)
    let network = args.network.clone().or(profile_res.network).or_else(|| {
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
    }).or(Some(Network::Testnet));

    let dir = args.path.as_deref();

    // Resolve endpoint: CLI --endpoint > --profile > default (explorer API)
    let endpoint = args.endpoint
        .as_deref()
        .or(profile_res.endpoint.as_deref())
        .or(Some(DEFAULT_QUERY_ENDPOINT));

    // Mainnet + broadcast: print informational warning (same pattern as deploy)
    if args.broadcast && matches!(network, Some(Network::Mainnet)) {
        println!(
            "{} {}",
            "[warning]".yellow().bold(),
            "Executing on MAINNET with --broadcast. This is irreversible and costs real fees."
        );
    }

    let private_key = args.private_key.as_deref();
    let mut extra_args = build_leo_run_args(
        &args.name,
        &args.inputs,
        network.as_ref(),
        endpoint,
        &args.json_output,
        private_key,
    );

    if args.broadcast {
        extra_args.push("--broadcast".to_string());
    }

    // Do NOT pass --yes to leo. Leo's own help text warns against it:
    // "DO NOT SET THIS FLAG UNLESS YOU KNOW WHAT YOU ARE DOING"
    // Let leo's own confirmation prompts surface via inherited stdout/stderr.

    if args.broadcast {
        print_info(
            &format!(
                "Broadcasting execution to '{}'...",
                match network {
                    Some(Network::Testnet) => "testnet",
                    Some(Network::Mainnet) => "mainnet",
                    Some(Network::Canary) => "canary",
                    None => "default",
                }
            ),
            quiet,
        );
    } else {
        print_info(
            "Running in dry-run mode (no --broadcast passed). Add --broadcast to actually execute.",
            quiet,
        );
    }

    // Run with stderr capture for best-effort error translation
    let (result, captured_stderr) = leo_cmd::run_leo_captured("execute", &extra_args, dir);
    if let Err(ref _e) = result {
        translate_run_execute_error(&captured_stderr, &args.name, args.path.as_deref());
    }
    result
}

// ---------------------------------------------------------------------------
// Send command: wrap leo execute credits.aleo::transfer_public
// ---------------------------------------------------------------------------

/// Validate that `amount` is a whole number of microcredits and format it as a
/// Leo u64 literal for credits.aleo's transfer_public transition.
/// Returns an error message instead of shelling out to leo with garbage input.
fn format_send_amount(amount: &str) -> Result<String> {
    if amount.parse::<u64>().is_err() {
        bail!(
            "Invalid amount '{}': expected a whole number of microcredits, e.g. 1000000 = 1 credit.",
            amount
        );
    }
    Ok(format!("{}u64", amount))
}

/// `aleoflow send <to> <amount>`: a convenience wrapper around
/// `leo execute credits.aleo::transfer_public <to> <amount>u64` for easy fund
/// transfers. The transition is program-qualified per leo's CLI (verified
/// against the live credits.aleo interface: transfer_public(to, amount)).
/// Dry-run unless --broadcast, same safety convention as deploy/execute.
fn handle_send(args: &SendArgs, quiet: bool, profile: Option<&str>) -> Result<()> {
    // Validate private key format before any subprocess runs
    if let Some(ref pk) = args.private_key {
        if let Some(reason) = validate_private_key_format(pk) {
            bail!("{}", reason);
        }
    }

    // Validate the amount and format it as a u64 literal before shelling out.
    let amount_lit = format_send_amount(&args.amount)?;

    if !leo_cmd::leo_is_installed() {
        bail!(
            "leo is not installed or not on PATH. Install it with: cargo binstall leo-lang"
        );
    }

    let cfg = load_aleoflow_config();
    let profile_res = resolve_profile(profile, &cfg, quiet)?;

    // Resolve network: CLI --network > --profile > config > default (Testnet)
    let network = args.network.clone().or(profile_res.network).or_else(|| {
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
    }).or(Some(Network::Testnet));

    // Resolve endpoint: CLI --endpoint > --profile > default (explorer API)
    let endpoint = args.endpoint
        .as_deref()
        .or(profile_res.endpoint.as_deref())
        .or(Some(DEFAULT_QUERY_ENDPOINT));

    // Mainnet + broadcast: print informational warning (same pattern as deploy/execute)
    if args.broadcast && matches!(network, Some(Network::Mainnet)) {
        println!(
            "{} {}",
            "[warning]".yellow().bold(),
            "Sending funds on MAINNET with --broadcast. This is irreversible and costs real fees."
        );
    }

    // credits.aleo transfer_public signature (verified live):
    //   input r0 as address.public;  // to
    //   input r1 as u64.public;      // amount (microcredits)
    let private_key = args.private_key.as_deref();
    let mut extra_args = build_leo_run_args(
        "credits.aleo::transfer_public",
        &[args.to.clone(), amount_lit],
        network.as_ref(),
        endpoint,
        &None,
        private_key,
    );

    if args.broadcast {
        extra_args.push("--broadcast".to_string());
    }

    // Do NOT pass --yes to leo. Leo's own help text warns against it:
    // "DO NOT SET THIS FLAG UNLESS YOU KNOW WHAT YOU ARE DOING"
    // Let leo's own confirmation prompts surface via inherited stdout/stderr.

    if args.broadcast {
        print_info(
            &format!(
                "Broadcasting transfer_public to '{}'...",
                match network {
                    Some(Network::Testnet) => "testnet",
                    Some(Network::Mainnet) => "mainnet",
                    Some(Network::Canary) => "canary",
                    None => "default",
                }
            ),
            quiet,
        );
    } else {
        print_info(
            "Running in dry-run mode (no --broadcast passed). Add --broadcast to actually send the transfer.",
            quiet,
        );
    }

    // Run with stderr capture for best-effort error translation
    let (result, captured_stderr) = leo_cmd::run_leo_captured("execute", &extra_args, None);
    if let Err(_e) = &result {
        translate_run_execute_error(&captured_stderr, "transfer_public", None);
    }
    result
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

/// Return the Leo-formatted string conversion expression for an ABI type.
/// Maps each Leo primitive to its corresponding helper function call.
/// Handles both ABI formats:
///   - Object: `{"Primitive": {"UInt": "U64"}}` (parameterized primitives)
///   - String: `{"Primitive": "Field"}` (simple primitives)
fn leo_type_converter_expr(ty: &serde_json::Value, var_name: &str) -> String {
    if let Some(prim) = ty.get("Primitive") {
        // Handle string format: {"Primitive": "Field"}
        if let Some(prim_str) = prim.as_str() {
            return match prim_str {
                "Boolean" => format!("toBoolean({})", var_name),
                "Address" => var_name.to_string(),
                "Field" => format!("toField({})", var_name),
                "Scalar" => format!("toScalar({})", var_name),
                "Group" => var_name.to_string(),
                "Signature" => var_name.to_string(),
                "String" => var_name.to_string(),
                _ => format!("String({}) /* {} */", var_name, prim_str),
            };
        }
        // Handle object format: {"Primitive": {"UInt": "U64"}}
        if let Some(obj) = prim.as_object() {
            for (type_name, size_val) in obj {
                return match type_name.as_str() {
                    "Boolean" => format!("toBoolean({})", var_name),
                    "Int8" => format!("toI8({})", var_name),
                    "Int16" => format!("toI16({})", var_name),
                    "Int32" => format!("toI32({})", var_name),
                    "Int64" => format!("toI64({})", var_name),
                    "Int128" => format!("toI128({})", var_name),
                    "UInt" | "UInt8" | "UInt16" | "UInt32" | "UInt64" | "UInt128" => {
                        let fn_name = match size_val.as_str() {
                            Some("U8") => "toU8",
                            Some("U16") => "toU16",
                            Some("U32") => "toU32",
                            Some("U64") => "toU64",
                            Some("U128") => "toU128",
                            _ => "toU64",
                        };
                        format!("{}({})", fn_name, var_name)
                    }
                    "Field" => format!("toField({})", var_name),
                    "Scalar" => format!("toScalar({})", var_name),
                    "Address" => var_name.to_string(),
                    "Group" => var_name.to_string(),
                    "Signature" => var_name.to_string(),
                    "String" => var_name.to_string(),
                    _ => format!("String({}) /* unknown primitive */", var_name),
                };
            }
        }
    }
    if ty.get("Struct").is_some() {
        return var_name.to_string();
    }
    format!("String({})", var_name)
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
        // Handle string format: {"Primitive": "Field"}
        if let Some(prim_str) = prim.as_str() {
            return match prim_str {
                "Boolean" => "boolean".to_string(),
                "Address" => "string".to_string(),
                "Field" | "Scalar" => "bigint".to_string(),
                "Group" | "Signature" | "String" => "string".to_string(),
                _ => format!("unknown /* {} */", prim_str),
            };
        }
        // Handle object format: {"Primitive": {"UInt": "U64"}}
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

// ---------------------------------------------------------------------------
// Audit helper functions
// ---------------------------------------------------------------------------

/// Parse record declarations from Leo source lines.
/// Returns a map: record_name -> Vec<(field_name, visibility)>
/// where visibility is "private" or "public".
fn parse_record_declarations(lines: &[&str]) -> BTreeMap<String, Vec<(String, String)>> {
    let mut records: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    let mut i = 0;
    while i < lines.len() {
        let trimmed = lines[i].trim();
        if trimmed.starts_with("record ") && trimmed.contains('{') {
            // Extract record name
            let name = trimmed
                .strip_prefix("record ")
                .and_then(|s| s.split_whitespace().next())
                .unwrap_or("")
                .to_string();
            if name.is_empty() {
                i += 1;
                continue;
            }

            // Consume opening brace depth
            let mut depth = trimmed.matches('{').count() as i32
                - trimmed.matches('}').count() as i32;
            let mut fields: Vec<(String, String)> = Vec::new();
            i += 1;

            while i < lines.len() && depth > 0 {
                let inner = lines[i].trim();
                depth += inner.matches('{').count() as i32;
                depth -= inner.matches('}').count() as i32;

                if depth > 0 && !inner.is_empty() && !inner.starts_with("//") {
                    // Parse field: [public] name: type,
                    let field_line = inner.trim_end_matches(',');
                    if field_line.contains(':') && !field_line.starts_with('}') {
                        let visibility = if field_line.starts_with("public ") {
                            "public"
                        } else {
                            "private"
                        };
                        let after_vis = if visibility == "public" {
                            field_line.strip_prefix("public ").unwrap_or(field_line)
                        } else {
                            field_line
                        };
                        if let Some(col) = after_vis.find(':') {
                            let fname = after_vis[..col].trim().to_string();
                            if !fname.is_empty() {
                                fields.push((fname, visibility.to_string()));
                            }
                        }
                    }
                }
                i += 1;
            }
            records.insert(name, fields);
        } else {
            i += 1;
        }
    }
    records
}

/// Extract function/transition parameter names and types from a signature line.
fn leo_func_params(sig_line: &str) -> Vec<(String, String)> {
    let paren_open = match sig_line.find('(') {
        Some(p) => p,
        None => return vec![],
    };
    let after_open = &sig_line[paren_open + 1..];
    let mut depth = 1i32;
    let mut paren_close = None;
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
    let params_str = match paren_close {
        Some(close) => &sig_line[paren_open + 1..close],
        None => return vec![],
    };

    let mut result = Vec::new();
    for segment in params_str.split(',') {
        let seg = segment.trim();
        if seg.is_empty() {
            continue;
        }
        if let Some(col) = seg.find(':') {
            let pname = seg[..col].trim().to_string();
            let ptype = seg[col + 1..].trim().to_string();
            result.push((pname, ptype));
        }
    }
    result
}

/// Find transition function signatures and their body line ranges.
/// Returns Vec<(name, sig_line, body_start, body_end)>
fn find_transition_signatures(lines: &[&str]) -> Vec<(String, usize, usize, usize)> {
    let mut result = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let trimmed = lines[i].trim();
        if (trimmed.starts_with("transition ") || trimmed.starts_with("async function "))
            && trimmed.contains('(')
        {
            let name = if trimmed.starts_with("transition ") {
                trimmed
                    .strip_prefix("transition ")
                    .and_then(|s| s.split('(').next())
                    .unwrap_or("")
                    .trim()
                    .to_string()
            } else {
                trimmed
                    .strip_prefix("async function ")
                    .and_then(|s| s.split('(').next())
                    .unwrap_or("")
                    .trim()
                    .to_string()
            };

            if name.is_empty() {
                i += 1;
                continue;
            }

            // Find opening brace
            let mut sig_line = i;
            let mut body_start = i;
            let mut found_brace = false;
            for j in i..lines.len() {
                if lines[j].trim().contains('{') {
                    sig_line = i;
                    body_start = j + 1;
                    found_brace = true;
                    break;
                }
            }

            if !found_brace {
                i += 1;
                continue;
            }

            // Find closing brace by counting depth
            let mut depth = 1i32;
            let mut body_end = body_start;
            for j in body_start..lines.len() {
                let inner = lines[j].trim();
                depth += inner.matches('{').count() as i32;
                depth -= inner.matches('}').count() as i32;
                if depth <= 0 {
                    body_end = j;
                    break;
                }
            }

            result.push((name, sig_line, body_start, body_end));
            i = body_end + 1;
        } else {
            i += 1;
        }
    }
    result
}

/// Parse a `let <var> = <record_param>.<field>;` pattern.
/// Returns Some((var_name, "record_param.field")) if matched.
fn parse_let_record_field(
    line: &str,
    record_params: &[(String, &str)],
    record_declarations: &BTreeMap<String, Vec<(String, String)>>,
) -> Option<(String, String)> {
    let line = line.trim_end_matches(';').trim();
    if !line.starts_with("let ") || !line.contains(" = ") {
        return None;
    }

    let after_let = line.strip_prefix("let ")?;
    let eq_pos = after_let.find(" = ")?;
    let var_name = after_let[..eq_pos].trim().to_string();
    let rhs = after_let[eq_pos + 3..].trim();

    // RHS must be of form <record_param>.<field>
    let dot_pos = rhs.find('.')?;
    let param_name = rhs[..dot_pos].trim();
    let field_name = rhs[dot_pos + 1..].trim();

    // Check if param_name is a record-typed parameter
    let record_type = record_params.iter()
        .find(|(pname, _)| pname == param_name)
        .map(|(_, ptype)| *ptype)?;

    // Check if field is private in the record declaration
    let decl = record_declarations.get(record_type)?;
    let is_private = decl.iter()
        .find(|(fname, _)| fname == &field_name)
        .map(|(_, vis)| vis == "private")
        .unwrap_or(false);

    if is_private {
        Some((var_name, format!("{}.{}", param_name, field_name)))
    } else {
        None
    }
}

/// Parse a direct `record_param.field` access.
/// Returns Some((param_name, field_name)) if it's a private record field access.
fn parse_direct_field_access(
    arg: &str,
    record_params: &[(String, &str)],
    record_declarations: &BTreeMap<String, Vec<(String, String)>>,
) -> Option<(String, String)> {
    let arg = arg.trim().trim_end_matches(',');
    let dot_pos = arg.find('.')?;
    let param_name = arg[..dot_pos].trim().to_string();
    let field = arg[dot_pos + 1..].trim().to_string();

    let record_type = record_params.iter()
        .find(|(pname, _)| pname.as_str() == param_name)?;

    let decl = record_declarations.get(record_type.1)?;
    let is_private = decl.iter()
        .find(|(fname, _)| fname == &field)
        .map(|(_, vis)| vis == "private")
        .unwrap_or(false);

    if is_private {
        Some((param_name, field))
    } else {
        None
    }
}

/// Extract all function-call names and their arguments from within a return expression.
/// For `return (..., func_name(arg1, arg2));`, returns [(func_name, [arg1, arg2])].
/// This is a best-effort heuristic -- it finds identifiers and dot-accesses
/// that look like arguments inside function calls in the return expression.
fn extract_finalize_calls(line: &str) -> Vec<(String, Vec<String>)> {
    let mut calls = Vec::new();
    let line = line.trim_end_matches(';');

    // Find the start of each function call: any `name(` inside the return
    let mut search_start = 0;
    loop {
        let paren_open = match line[search_start..].find('(') {
            Some(p) => search_start + p,
            None => break,
        };

        // Look backwards from paren_open to find if there's an identifier
        let before_paren = line[..paren_open].trim_end();
        let call_start = before_paren.rfind(|c: char| !c.is_alphanumeric() && c != '_' && c != '.')
            .map(|p| p + 1)
            .unwrap_or(0);
        let func_name = before_paren[call_start..].trim();

        if func_name.is_empty() || func_name == "return" {
            // Skip `return (` itself -- just advance past `(` and continue scanning
            // inside the return expression so nested function calls are found.
            search_start = paren_open + 1;
            continue;
        }

        // We found a function call -- parse its arguments
        let after_open = &line[paren_open + 1..];
        let mut depth = 1i32;
        let mut call_end = None;
        for (i, ch) in after_open.char_indices() {
            match ch {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        call_end = Some(paren_open + 1 + i);
                        break;
                    }
                }
                _ => {}
            }
        }

        let args_str = match call_end {
            Some(end) => &line[paren_open + 1..end],
            None => {
                search_start = paren_open + 1;
                continue;
            }
        };

        // Split arguments by comma, respecting nested parens
        let mut arg_parts = Vec::new();
        let mut part_start = 0;
        let mut pdepth = 0i32;
        for (i, ch) in args_str.char_indices() {
            match ch {
                '(' => pdepth += 1,
                ')' => pdepth -= 1,
                ',' if pdepth == 0 => {
                    let arg = args_str[part_start..i].trim().to_string();
                    if !arg.is_empty() {
                        arg_parts.push(arg);
                    }
                    part_start = i + 1;
                }
                _ => {}
            }
        }
        let last_arg = args_str[part_start..].trim().to_string();
        if !last_arg.is_empty() {
            arg_parts.push(last_arg);
        }

        if !func_name.is_empty() {
            calls.push((func_name.to_string(), arg_parts));
        }
        search_start = call_end.unwrap_or(paren_open + 1) + 1;
    }

    calls
}


fn handle_audit(args: &AuditArgs, quiet: bool) -> Result<()> {
    // NOTE: This is a heuristic linter for hackathon-demo purposes.
    // It performs line-based static analysis and is NOT a formal verifier.
    // Real security audits require formal verification tools.
    //
    // Checks:
    //   1. Sensitive identifiers (password, secret, etc.) outside records
    //   2. TODO/FIXME comment detection
    //   3. Mapping::set with sensitive key names
    //   4. Record field visibility on sensitive fields
    //   5. Finalize-leak (transition passes private record field values to
    //      async/finalize functions, where args are public on-chain).
    //      This is a single-hop/shallow taint tracker scoped to one
    //      transition body -- it does NOT follow chains of reassignments,
    //      arithmetic transforms, or values passed through helper fns.

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
        let rel = path
            .strip_prefix(audit_path)
            .unwrap_or(path)
            .display()
            .to_string()
            .replace('\\', "/");

        let file_findings = grouped.entry(rel.clone()).or_default();

        // -----------------------------------------------------------------------
        // Phase 1: Parse record declarations from the file
        // -----------------------------------------------------------------------
        let record_declarations = parse_record_declarations(&lines);

        // -----------------------------------------------------------------------
        // Phase 2: Line-by-line scans
        // -----------------------------------------------------------------------
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
                for id in &sensitive_ids {
                    if trimmed.contains(id) && !trimmed.starts_with("//") {
                        file_findings.push((
                            "[warning]".yellow().bold().to_string(),
                            format!(
                                "Line {}: '{}' appears outside a record -- this data is                                  public on-chain. Wrap it in a private record if it should                                  be confidential.",
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

                // Check 4 (old record-field check):
                // Public visibility on sensitive record fields
                if trimmed.contains("public ") {
                    for id in &sensitive_ids {
                        if trimmed.contains(id) {
                            file_findings.push((
                                "[warning]".yellow().bold().to_string(),
                                format!(
                                    "Line {}: Record field '{}' is declared 'public' and                                      may expose sensitive data on-chain. Consider omitting                                      the 'public' modifier (default is 'private').",
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

        // -----------------------------------------------------------------------
        // Phase 3: Scan for Mapping::set with sensitive key names  (Check 3)
        // -----------------------------------------------------------------------
        for (i, line) in lines.iter().enumerate() {
            let trimmed = line.trim();
            if trimmed.starts_with("//") {
                continue;
            }
            if trimmed.contains("Mapping::set")
                || trimmed.contains(".set(")
            {
                // Check if any string literal argument contains a sensitive word
                for id in &sensitive_ids {
                    if trimmed.to_lowercase().contains(id) {
                        file_findings.push((
                            "[warning]".yellow().bold().to_string(),
                            format!(
                                "Line {}: Mapping::set uses potentially sensitive key '{}'.                                  Mapping keys are public on-chain and visible to all.                                  Avoid using raw sensitive identifiers as mapping keys.",
                                i + 1,
                                id
                            ),
                        ));
                    }
                }
            }
        }

        // -----------------------------------------------------------------------
        // Phase 4: Finalize-leak check via local data-flow tracking  (Check 5)
        // -----------------------------------------------------------------------
        let transitions = find_transition_signatures(&lines);
        for (_trans_name, trans_sig_line, trans_body_start, trans_body_end) in &transitions {
            // Identify record-typed parameters for this transition
            let params = leo_func_params(lines[*trans_sig_line]);
            let record_params: Vec<(String, &str)> = params.iter()
                .filter_map(|(pname, ptype)| {
                    let clean_ty = ptype.trim_end_matches(',');
                    if record_declarations.contains_key(clean_ty) {
                        Some((pname.clone(), clean_ty))
                    } else {
                        None
                    }
                })
                .collect();

            if record_params.is_empty() {
                continue;
            }

            // Build a local taint map: local_var -> (original_record_param.field_name)
            let mut taint_map: BTreeMap<String, String> = BTreeMap::new();

            for j in *trans_body_start..=*trans_body_end {
                let line = lines[j].trim();

                // Track let-bindings from record field access
                if let Some(capture) = parse_let_record_field(line, &record_params, &record_declarations) {
                    taint_map.insert(capture.0, capture.1);
                }
            }

            // Scan for return patterns: `return (..., call_name(args));`
            for j in *trans_body_start..=*trans_body_end {
                let line = lines[j].trim();
                if !line.starts_with("return ") {
                    continue;
                }

                // Find all function-call arguments within the return expression
                let calls = extract_finalize_calls(line);
                for (finalize_name, args) in &calls {
                    for arg in args {
                        // Case A: Direct record field access: `record_param.field`
                        if let Some((_rp_name, field)) = parse_direct_field_access(arg, &record_params, &record_declarations) {
                            file_findings.push((
                                "[warning]".yellow().bold().to_string(),
                                format!(
                                    "Line {}: Record field '{}' (private) may be exposed publicly                                      via finalize function '{}' -- finalize/async function arguments                                      are public on-chain, even when derived from private record fields.                                      See https://blog.zksecurity.xyz/posts/aleo-program-security/ for                                      details and the fix (pass a commitment/hash instead).
                                       [single-hop/shallow: does not track chained reassignments,                                      arithmetic transforms, or values through helper fns.]",
                                    j + 1,
                                    field,
                                    finalize_name
                                ),
                            ));
                            continue;
                        }

                        // Case B: Local variable from taint map
                        let clean_arg = arg.trim_end_matches(',');
                        if let Some(origin) = taint_map.get(clean_arg) {
                            file_findings.push((
                                "[warning]".yellow().bold().to_string(),
                                format!(
                                    "Line {}: Variable '{}' derived from private record field '{}'                                      may be exposed publicly via finalize function '{}' --                                      finalize/async function arguments are public on-chain, even                                      when derived from private record fields.                                      See https://blog.zksecurity.xyz/posts/aleo-program-security/ for                                      details and the fix (pass a commitment/hash instead).
                                       [single-hop/shallow: does not track chained reassignments,                                      arithmetic transforms, or values through helper fns.]",
                                    j + 1,
                                    clean_arg,
                                    origin,
                                    finalize_name
                                ),
                            ));
                        }
                    }
                }
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
            "{} This is a heuristic linter for demonstration purposes, not a formal verifier.              The finalize-leak check is a single-hop/shallow taint tracker scoped to one              transition body (does not track chained reassignments, arithmetic transforms,              or values through helper fn calls).",
            "[info]".dimmed()
        );
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Faucet command: opens the Aleo testnet faucet in the user's browser
// ---------------------------------------------------------------------------

/// Open a URL in the user's default browser using the platform-specific command.
/// Falls back gracefully (prints the URL) if opening fails.
///
/// This is an intentional convenience wrapper, NOT a bypass of anti-bot protection.
/// Aleo's faucets (official web form, Stakely with captcha+tweet verification) are
/// deliberately not fully automatable, and this command must not attempt to
/// circumvent that. We only open a browser and print guidance.
fn open_url(url: &str) {
    let result = if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg(url).status()
    } else if cfg!(target_os = "windows") {
        std::process::Command::new("cmd")
            .args(["/c", "start", url])
            .status()
    } else {
        // Linux and other Unix-likes
        std::process::Command::new("xdg-open").arg(url).status()
    };

    match result {
        Ok(status) if status.success() => {}
        _ => {
            // If opening the browser fails for any reason (no default browser,
            // running headless, etc.), don't error out -- just print the URL
            // so the user can open it manually.
            eprintln!(
                "Could not open your browser automatically.\n\
                 Open this URL manually:\n\
                 {}",
                url
            );
        }
    }
}

/// Handle `aleoflow env [--profile <name>] [--network <net>] [--endpoint <url>]`.
/// Resolves and prints ALL effective configuration AleoFlow would use,
/// without actually running any command.
fn handle_env(args: &EnvArgs, quiet: bool, profile: Option<&str>) -> Result<()> {
    let cfg = load_aleoflow_config();

    // Profile comes from the global --profile flag (same as other commands).
    let profile_res = resolve_profile(profile, &cfg, true).unwrap_or_else(|_| {
        ProfileResolution { network: None, endpoint: None }
    });

    let config_path = Path::new("aleo.toml");
    let config_exists = config_path.exists();

    // Resolve network: CLI --network > --profile > config > built-in default
    let has_cli_network = args.network.is_some();
    let has_profile_network = profile_res.network.is_some();
    let has_config_network = cfg.default_network.is_some();

    let network = args.network.clone().or_else(|| profile_res.network.clone()).or_else(|| {
        cfg.default_network.as_deref().and_then(parse_network)
    });

    let endpoint = args.endpoint.as_deref()
        .or_else(|| profile_res.endpoint.as_deref())
        .map(|s| s.to_string());

    let _has_cli_endpoint = args.endpoint.is_some();
    let _has_profile_endpoint = profile_res.endpoint.is_some();

    // If a profile was requested but resolution failed, warn the user
    if let Some(pname) = profile {
        let profile_valid = cfg.profiles.as_ref().and_then(|p| p.get(pname)).is_some();
        if !profile_valid {
            eprintln!(
                "[warning] Profile '{}' was not found in aleo.toml. Configuration \
                 will use built-in defaults instead.",
                pname
            );
        }
    }

    println!("AleoFlow Configuration Preview");
    println!("-------------------------------");

    // Network and source
    let net_src = if has_cli_network {
        "CLI --network flag"
    } else if has_profile_network {
        "--profile"
    } else if has_config_network && network.is_some() {
        "aleo.toml default_network"
    } else {
        "built-in default (testnet)"
    };
    let net_display = network
        .as_ref()
        .map(|n| match n {
            Network::Testnet => "testnet",
            Network::Mainnet => "mainnet",
            Network::Canary => "canary",
        })
        .unwrap_or("testnet");
    println!("  Network:   {} (from: {})", net_display, net_src);

    // Endpoint and source
    let ep_src = if args.endpoint.is_some() {
        "CLI --endpoint flag"
    } else if profile.is_some() && profile_res.endpoint.is_some() {
        "--profile"
    } else {
        "none (leo will use its own default)"
    };
    let ep_display = endpoint.as_deref().unwrap_or("(none -- leo default)");
    println!("  Endpoint:  {} (from: {})", ep_display, ep_src);

    // PRIVATE_KEY status
    let pk_set = std::env::var("PRIVATE_KEY").is_ok();
    println!("  PRIVATE_KEY: {}", if pk_set { "set" } else { "not set" });

    // Active profile
    match profile {
        Some(name) => println!("  Profile:   {} (active via --profile)", name),
        None => println!("  Profile:   (none)"),
    }

    // aleo.toml path
    if config_exists {
        println!("  Config:    {} (found, being read)", config_path.display());
    } else {
        println!("  Config:    none found (using built-in defaults)");
    }

    if !quiet {
        println!();
        println!(
            "{} This is a preview only -- no command was executed.",
            "[info]".cyan().bold()
        );
    }

    Ok(())
}

/// Handle `aleoflow faucet [address]`.
/// Always opens the official Aleo faucet in the browser and prints guidance.
/// Never attempts to submit forms, solve captchas, or interact with APIs.
fn handle_faucet(args: &FaucetArgs) -> Result<()> {
    // Check if an address was provided. There's no existing pattern for reading
    // a saved address from env/.env in this codebase (PRIVATE_KEY is read for
    // account operations, but no ADDRESS variable is defined), so we require
    // the user to pass it explicitly.
    let address = match &args.address {
        Some(addr) => addr.clone(),
        None => {
            bail!(
                "No address provided. Usage: aleoflow faucet <ADDRESS>\n\
                 Example: aleoflow faucet aleo1064wgu5z5relqrhk6lv2ngr5zw5mf8eyp9sf03eu8q00mkv8zursd34fkt\n\
                 Pass your Aleo testnet address as the argument."
            );
        }
    };

    // Print the address clearly, on its own line, copy-ready
    println!("Requesting testnet credits for:");
    println!("{}", address);
    println!();

    // Open the faucet in the browser
    let faucet_url = "https://faucet.aleo.org/";
    open_url(faucet_url);

    // Print guidance message
    println!(
        "Faucet opened in your browser. Paste your address above, \
         complete verification, and tokens typically arrive within a few minutes."
    );
    println!();

    // Print fallback alternatives
    println!(
        "If this faucet is slow or unavailable, alternatives:"
    );
    println!(
        "  - Discord #faucet channel: use '/sendcredits <address> <amount>', \
         limited to 50 credits/hour"
    );
    println!(
        "  - https://stakely.io/faucet/aleo-aleo-testnet \
         (requires solving a captcha and posting a verification tweet)"
    );

    Ok(())
}
