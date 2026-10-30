#![allow(clippy::missing_safety_doc)]
#![allow(static_mut_refs)]

use std::mem;

const BASALT_PLUGIN_API_VERSION: u32 = 1;
const CAP_PROJECT_MODEL: u64 = 1 << 8;
const CAP_SHADOW_EXCLUDES: u64 = 1 << 25;
const FILE_BUF_OFFSET: usize = 8 * 1024 * 1024;
const FILE_BUF_CAP: usize = 256 * 1024;

#[link(wasm_import_module = "env")]
extern "C" {
    fn basalt_read_file(path_ptr: i32, path_len: i32, out_ptr: i32, out_cap: i32) -> i32;
}

#[repr(C)]
struct PluginMetaRecord {
    api_version: u32,
    _pad: u32,
    hook_flags: u64,
    name_ptr: u32,
    version_ptr: u32,
    provides_ptr: u32,
    requires_ptr: u32,
    file_globs_ptr: u32,
    activates_on_ptr: u32,
    activation_events_ptr: u32,
}

static NAME: &[u8] = b"cargo-project-model\0";
static VERSION: &[u8] = b"0.1.1\0";
static PROVIDES: &[u8] = b"project-model:cargo\0";
static REQUIRES: &[u8] = b"\0";
static FILE_GLOBS: &[u8] = b"Cargo.toml\n**/Cargo.toml\0";
static ACTIVATES_ON: &[u8] = b"Cargo.toml\0";
static ACTIVATION_EVENTS: &[u8] = b"workspace_opened\0";

static mut META: PluginMetaRecord = PluginMetaRecord {
    api_version: 0,
    _pad: 0,
    hook_flags: 0,
    name_ptr: 0,
    version_ptr: 0,
    provides_ptr: 0,
    requires_ptr: 0,
    file_globs_ptr: 0,
    activates_on_ptr: 0,
    activation_events_ptr: 0,
};

#[no_mangle]
pub extern "C" fn basalt_plugin_metadata() -> i32 {
    unsafe {
        META.api_version = BASALT_PLUGIN_API_VERSION;
        META._pad = 0;
        META.hook_flags = CAP_PROJECT_MODEL | CAP_SHADOW_EXCLUDES;
        META.name_ptr = NAME.as_ptr() as u32;
        META.version_ptr = VERSION.as_ptr() as u32;
        META.provides_ptr = PROVIDES.as_ptr() as u32;
        META.requires_ptr = REQUIRES.as_ptr() as u32;
        META.file_globs_ptr = FILE_GLOBS.as_ptr() as u32;
        META.activates_on_ptr = ACTIVATES_ON.as_ptr() as u32;
        META.activation_events_ptr = ACTIVATION_EVENTS.as_ptr() as u32;
        &raw const META as i32
    }
}

#[no_mangle]
pub unsafe extern "C" fn deallocate(ptr: i32, len: i32) {
    if ptr <= 0 || len <= 0 {
        return;
    }
    let _ = Vec::from_raw_parts(ptr as *mut u8, len as usize, len as usize);
}

/// Cargo's `target/` directory is reproducible build output.
#[no_mangle]
pub unsafe extern "C" fn basalt_shadow_excludes(_root_ptr: i32, _root_len: i32) -> i64 {
    pack_output(br#"["**/target/**"]"#.to_vec())
}

#[no_mangle]
pub unsafe extern "C" fn basalt_build_project_model(root_ptr: i32, root_len: i32) -> i64 {
    let root = read_utf8(root_ptr, root_len);
    if root.is_empty() {
        return 0;
    }

    let manifest_path = if root.ends_with('/') {
        format!("{root}Cargo.toml")
    } else {
        format!("{root}/Cargo.toml")
    };

    let manifest = read_host_file(&manifest_path).unwrap_or_default();
    let package_name = parse_package_name(&manifest).unwrap_or_default();
    let display_name = if package_name.is_empty() {
        workspace_name(&root)
    } else {
        package_name.clone()
    };
    let projects = discover_projects(&root, &manifest_path, &manifest, &display_name, &package_name);
    let json = build_project_model_json(&root, &manifest_path, &display_name, &projects);
    pack_output(json.into_bytes())
}


unsafe fn read_utf8(ptr: i32, len: i32) -> String {
    if ptr <= 0 || len <= 0 {
        return String::new();
    }
    let bytes = std::slice::from_raw_parts(ptr as *const u8, len as usize);
    String::from_utf8_lossy(bytes).into_owned()
}

fn read_host_file(path: &str) -> Option<String> {
    let n = unsafe {
        basalt_read_file(
            path.as_ptr() as i32,
            path.len() as i32,
            FILE_BUF_OFFSET as i32,
            FILE_BUF_CAP as i32,
        )
    };
    if n <= 0 {
        return None;
    }
    let bytes = unsafe { std::slice::from_raw_parts(FILE_BUF_OFFSET as *const u8, n as usize) };
    Some(String::from_utf8_lossy(bytes).into_owned())
}

fn parse_package_name(manifest: &str) -> Option<String> {
    let mut in_package = false;
    for raw_line in manifest.lines() {
        let line = raw_line.trim();
        if line.starts_with('[') && line.ends_with(']') {
            in_package = line == "[package]";
            continue;
        }
        if !in_package || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else { continue };
        if key.trim() != "name" {
            continue;
        }
        let trimmed = value.trim().trim_matches('"');
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    None
}

fn parse_workspace_members(manifest: &str) -> Vec<String> {
    let mut members = Vec::new();
    let mut in_workspace = false;
    let mut in_members = false;

    for raw_line in manifest.lines() {
        let line = raw_line.trim();
        if line.starts_with('[') && line.ends_with(']') {
            in_workspace = line == "[workspace]";
            in_members = false;
            continue;
        }
        if !in_workspace || line.starts_with('#') {
            continue;
        }

        if in_members {
            if line.starts_with(']') {
                in_members = false;
                continue;
            }
            if let Some(member) = extract_string_literal(line) {
                members.push(member);
            }
            continue;
        }

        let Some((key, value)) = line.split_once('=') else { continue };
        if key.trim() != "members" {
            continue;
        }
        let value = value.trim();
        if value.starts_with('[') {
            in_members = !value.contains(']');
            for part in value.split(',') {
                if let Some(member) = extract_string_literal(part) {
                    members.push(member);
                }
            }
        }
    }

    members
}

fn extract_string_literal(value: &str) -> Option<String> {
    let trimmed = value.trim();
    let start = trimmed.find('"')?;
    let rest = &trimmed[start + 1..];
    let end = rest.find('"')?;
    let inner = &rest[..end];
    if inner.is_empty() {
        None
    } else {
        Some(inner.to_string())
    }
}

struct CargoProject {
    id: String,
    name: String,
    kind: String,
    subtype: Option<String>,
    capabilities: Vec<String>,
    manifest_path: String,
    relative_prefix: String,
}

fn discover_projects(
    root: &str,
    manifest_path: &str,
    manifest: &str,
    display_name: &str,
    package_name: &str,
) -> Vec<CargoProject> {
    let mut projects = Vec::new();

    if !package_name.is_empty() {
        projects.push(CargoProject {
            id: sanitize_id(package_name),
            name: package_name.to_string(),
            kind: infer_project_kind(manifest),
            subtype: Some("cargo-package".to_string()),
            capabilities: infer_project_capabilities(manifest),
            manifest_path: manifest_path.to_string(),
            relative_prefix: String::new(),
        });
    }

    for member in parse_workspace_members(manifest) {
        let relative_prefix = normalize_relative_prefix(&member);
        if relative_prefix.is_empty() {
            continue;
        }
        let member_manifest = join_path(root, &format!("{relative_prefix}/Cargo.toml"));
        let member_manifest_text = read_host_file(&member_manifest).unwrap_or_default();
        let member_name = parse_package_name(&member_manifest_text)
            .unwrap_or_else(|| workspace_name(&relative_prefix));
        let member_id = sanitize_id(&member_name);
        if projects.iter().any(|existing| existing.id == member_id && existing.relative_prefix == relative_prefix) {
            continue;
        }
        projects.push(CargoProject {
            id: member_id,
            name: member_name,
            kind: infer_project_kind(&member_manifest_text),
            subtype: Some("cargo-package".to_string()),
            capabilities: infer_project_capabilities(&member_manifest_text),
            manifest_path: member_manifest,
            relative_prefix,
        });
    }

    if projects.is_empty() {
        projects.push(CargoProject {
            id: sanitize_id(display_name),
            name: display_name.to_string(),
            kind: infer_project_kind(manifest),
            subtype: Some("cargo-package".to_string()),
            capabilities: infer_project_capabilities(manifest),
            manifest_path: manifest_path.to_string(),
            relative_prefix: String::new(),
        });
    }

    projects
}

fn build_project_model_json(
    root: &str,
    manifest_path: &str,
    display_name: &str,
    projects: &[CargoProject],
) -> String {
    let mut json = String::new();
    json.push('{');
    json.push_str("\"schema_version\":1,");
    push_json_string_field(&mut json, "ecosystem", "cargo");
    json.push(',');
    push_json_string_field(&mut json, "root", root);
    json.push(',');
    push_json_string_field(&mut json, "display_name", display_name);
    json.push(',');
    push_json_string_field(&mut json, "primary_manifest_path", manifest_path);
    json.push(',');

    json.push_str("\"projects\":[");
    for (idx, project) in projects.iter().enumerate() {
        if idx > 0 {
            json.push(',');
        }
        json.push('{');
        push_json_string_field(&mut json, "id", &project.id);
        json.push(',');
        push_json_string_field(&mut json, "name", &project.name);
        json.push(',');
        push_json_string_field(&mut json, "kind", &project.kind);
        json.push(',');
        push_optional_string_field(&mut json, "subtype", project.subtype.as_deref());
        json.push(',');
        push_string_array_field(&mut json, "capabilities", &project.capabilities);
        json.push(',');
        push_json_string_field(&mut json, "manifest_path", &project.manifest_path);
        json.push(',');
        push_string_array_field(&mut json, "source_roots", &[prefixed(project, "src/")]);
        json.push(',');
        push_string_array_field(
            &mut json,
            "test_roots",
            &[
                prefixed(project, "tests/"),
                prefixed(project, "examples/"),
                prefixed(project, "benches/"),
            ],
        );
        json.push(',');
        push_string_array_field(&mut json, "build_roots", &["target/".to_string()]);
        json.push(',');
        json.push_str("\"targets\":[{");
        push_json_string_field(&mut json, "id", &project.id);
        json.push(',');
        push_json_string_field(&mut json, "name", &project.name);
        json.push(',');
        push_json_string_field(&mut json, "kind", &project.kind);
        json.push(',');
        push_optional_string_field(&mut json, "subtype", project.subtype.as_deref());
        json.push(',');
        push_string_array_field(&mut json, "capabilities", &project.capabilities);
        json.push(',');
        push_json_string_field(&mut json, "language", "rust");
        json.push(',');
        push_string_array_field(&mut json, "source_roots", &[prefixed(project, "src/")]);
        json.push(',');
        push_string_array_field(
            &mut json,
            "test_roots",
            &[
                prefixed(project, "tests/"),
                prefixed(project, "examples/"),
                prefixed(project, "benches/"),
            ],
        );
        json.push(',');
        push_string_array_field(&mut json, "build_roots", &["target/".to_string()]);
        json.push_str("}]");
        json.push('}');
    }
    json.push_str("],");

    json.push_str("\"owners\":[");
    let mut first_owner = true;
    for project in projects {
        for (path, match_kind, label, kind) in owner_records(project) {
            if !first_owner {
                json.push(',');
            }
            first_owner = false;
            json.push('{');
            push_json_string_field(&mut json, "path", &path);
            json.push(',');
            push_json_string_field(&mut json, "match_kind", match_kind);
            json.push(',');
            push_json_string_field(&mut json, "label", &label);
            json.push(',');
            push_json_string_field(&mut json, "project_id", &project.id);
            json.push(',');
            push_json_string_field(&mut json, "target_id", &project.id);
            json.push(',');
            push_json_string_field(&mut json, "kind", kind);
            json.push('}');
        }
    }
    json.push_str("]}");
    json
}

fn push_json_string_field(out: &mut String, key: &str, value: &str) {
    out.push('"');
    out.push_str(key);
    out.push_str("\":");
    out.push('"');
    out.push_str(&escape_json(value));
    out.push('"');
}

fn push_optional_string_field(out: &mut String, key: &str, value: Option<&str>) {
    out.push('"');
    out.push_str(key);
    out.push_str("\":");
    match value {
        Some(value) => {
            out.push('"');
            out.push_str(&escape_json(value));
            out.push('"');
        }
        None => out.push_str("null"),
    }
}

fn push_string_array_field(out: &mut String, key: &str, values: &[String]) {
    out.push('"');
    out.push_str(key);
    out.push_str("\":[");
    for (idx, value) in values.iter().enumerate() {
        if idx > 0 {
            out.push(',');
        }
        out.push('"');
        out.push_str(&escape_json(value));
        out.push('"');
    }
    out.push(']');
}

fn owner_records(project: &CargoProject) -> Vec<(String, &'static str, String, &'static str)> {
    vec![
        (prefixed(project, "Cargo.toml"), "exact", project.name.clone(), "manifest"),
        (prefixed(project, "build.rs"), "exact", project.name.clone(), "build-script"),
        (prefixed(project, "src/"), "prefix", project.name.clone(), "source"),
        (prefixed(project, "tests/"), "prefix", format!("{} tests", project.name), "test"),
        (prefixed(project, "examples/"), "prefix", format!("{} examples", project.name), "example"),
        (prefixed(project, "benches/"), "prefix", format!("{} benches", project.name), "bench"),
    ]
}

fn infer_project_kind(manifest: &str) -> String {
    let caps = infer_project_capabilities(manifest);
    if caps.iter().any(|cap| cap == "is-executable") {
        "app".to_string()
    } else if caps.iter().any(|cap| cap == "is-test-only") {
        "test".to_string()
    } else {
        "library".to_string()
    }
}

fn infer_project_capabilities(manifest: &str) -> Vec<String> {
    let mut caps = Vec::new();
    if manifest.contains("[[bin]]") || manifest.contains("autobins") {
        caps.push("is-executable".to_string());
    }
    if manifest.contains("[lib]") || manifest.contains("proc-macro") || manifest.contains("crate-type") {
        caps.push("is-library".to_string());
    }
    if manifest.contains("proc-macro") {
        caps.push("builds-plugin".to_string());
    }
    if manifest.contains("[[test]]") && !caps.iter().any(|cap| cap == "is-executable") && !caps.iter().any(|cap| cap == "is-library") {
        caps.push("is-test-only".to_string());
    }
    caps
}

fn prefixed(project: &CargoProject, suffix: &str) -> String {
    if project.relative_prefix.is_empty() {
        suffix.to_string()
    } else {
        format!("{}/{}", project.relative_prefix, suffix)
    }
}

fn normalize_relative_prefix(value: &str) -> String {
    value
        .trim()
        .trim_matches('/')
        .replace('\\', "/")
}

fn join_path(root: &str, child: &str) -> String {
    if root.ends_with('/') {
        format!("{root}{child}")
    } else {
        format!("{root}/{child}")
    }
}

fn workspace_name(root: &str) -> String {
    let trimmed = root.trim_end_matches('/');
    trimmed
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or("workspace")
        .to_string()
}

fn sanitize_id(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return "workspace".to_string();
    }
    trimmed
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect()
}


fn escape_json(input: &str) -> String {
    let mut out = String::with_capacity(input.len() + 8);
    for ch in input.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c < ' ' => {
                let n = c as u32;
                out.push_str("\\u");
                out.push(hex(((n >> 12) & 0xf) as u8));
                out.push(hex(((n >> 8) & 0xf) as u8));
                out.push(hex(((n >> 4) & 0xf) as u8));
                out.push(hex((n & 0xf) as u8));
            }
            c => out.push(c),
        }
    }
    out
}

fn hex(n: u8) -> char {
    match n {
        0..=9 => (b'0' + n) as char,
        _ => (b'A' + (n - 10)) as char,
    }
}

fn pack_output(bytes: Vec<u8>) -> i64 {
    if bytes.is_empty() {
        return 0;
    }
    let mut boxed = bytes.into_boxed_slice();
    let ptr = boxed.as_mut_ptr() as u64;
    let len = boxed.len() as u64;
    mem::forget(boxed);
    ((ptr << 32) | len) as i64
}
