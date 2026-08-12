use std::{
    env,
    error::Error,
    ffi::OsStr,
    fs, io,
    path::{Path, PathBuf},
    process::Command,
};

const LEMONADE_VERSION: &str = "11.5.2";
const INSTALL_REVISION: &str = "ubuntu-x64-minimal-v1";
const PATCH_REVISION: &str = "gemma4-reasoning-llamacpp-11.5.1-v1";
const CHECKSUM_FILE: &str = "../../packaging/lemonade-embeddable.sha256";
const SKIP_ENV: &str = "UFORGE_SKIP_EMBEDDED_LEMONADE";
const REQUIRE_ENV: &str = "UFORGE_REQUIRE_EMBEDDED_LEMONADE";
// Keep the 11.5.2 control plane while rolling only its llama.cpp downloads
// back to the pins in Lemonade's v11.5.1 backend_versions.json.
const LLAMACPP_11_5_1_PINS: [(&str, &str); 6] = [
    ("vulkan", "b9747"),
    ("rocm-stable", "b9752"),
    ("rocm-nightly", "b1292"),
    ("cuda", "b9851"),
    ("metal", "b9747"),
    ("cpu", "b9747"),
];

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={CHECKSUM_FILE}");
    println!("cargo:rerun-if-env-changed={SKIP_ENV}");
    println!("cargo:rerun-if-env-changed={REQUIRE_ENV}");

    if let Err(error) = stage_u_forge_defaults() {
        panic!("failed to stage u-forge defaults: {error}");
    }

    if env_flag(SKIP_ENV) {
        println!("cargo:warning=skipping Embeddable Lemonade bootstrap ({SKIP_ENV} is set)");
        return;
    }

    let required = env_flag(REQUIRE_ENV);
    if let Err(error) = bootstrap() {
        if required {
            panic!("Embeddable Lemonade bootstrap is required: {error}");
        }
        println!(
            "cargo:warning=Embeddable Lemonade is unavailable; u-forge will remain graph-only: {error}"
        );
    }
}

fn bootstrap() -> Result<(), Box<dyn Error>> {
    ensure_supported_target()?;

    let manifest_dir = PathBuf::from(
        env::var_os("CARGO_MANIFEST_DIR")
            .ok_or("Cargo did not provide CARGO_MANIFEST_DIR to the Lemonade bootstrap")?,
    );
    let profile_dir = profile_dir()?;
    let install_dir = profile_dir.join("lemonade");
    let binary = install_dir.join("lemond");
    let model_manifest = install_dir.join("resources/server_models.json");
    let backend_versions = install_dir.join("resources/backend_versions.json");
    let defaults = install_dir.join("resources/defaults.json");
    let license = install_dir.join("LICENSE");
    let marker = install_dir.join(".u-forge-embedded-version");

    // Cargo notices manual removal of the provisioned artifact on the next command.
    println!("cargo:rerun-if-changed={}", marker.display());
    println!("cargo:rerun-if-changed={}", binary.display());
    println!("cargo:rerun-if-changed={}", model_manifest.display());
    println!("cargo:rerun-if-changed={}", backend_versions.display());
    println!("cargo:rerun-if-changed={}", defaults.display());
    println!("cargo:rerun-if-changed={}", license.display());

    let checksum_path = manifest_dir.join(CHECKSUM_FILE);
    let (expected_checksum, asset_name) = read_checksum_spec(&checksum_path)?;
    let expected_marker = format!(
        "version={LEMONADE_VERSION}\nasset={asset_name}\nsha256={expected_checksum}\ninstall={INSTALL_REVISION}\npatch={PATCH_REVISION}\n"
    );
    if artifact_is_current(
        &install_dir,
        [
            binary.as_path(),
            model_manifest.as_path(),
            backend_versions.as_path(),
            defaults.as_path(),
            license.as_path(),
        ],
        &model_manifest,
        &backend_versions,
        &marker,
        &expected_marker,
    ) {
        return Ok(());
    }

    let target_root = profile_dir
        .parent()
        .ok_or("target profile directory has no parent")?;
    let cache_dir = target_root.join("lemonade-cache");
    fs::create_dir_all(&cache_dir)?;
    let archive = cache_dir.join(&asset_name);
    ensure_archive(&archive, &asset_name, &expected_checksum)?;

    let staging = profile_dir.join(format!(".lemonade-bootstrap-{}", std::process::id()));
    remove_staging_dir(&staging)?;
    fs::create_dir_all(&staging)?;
    let result = install_archive(
        &archive,
        &asset_name,
        &staging,
        &install_dir,
        &expected_marker,
    );
    let _ = remove_staging_dir(&staging);
    result
}

fn profile_dir() -> Result<PathBuf, Box<dyn Error>> {
    let out_dir = PathBuf::from(
        env::var_os("OUT_DIR").ok_or("Cargo did not provide OUT_DIR to the build script")?,
    );
    out_dir
        .ancestors()
        .nth(3)
        .map(Path::to_path_buf)
        .ok_or_else(|| "Cargo OUT_DIR did not contain the expected target profile directory".into())
}

fn stage_u_forge_defaults() -> Result<(), Box<dyn Error>> {
    let manifest_dir = PathBuf::from(
        env::var_os("CARGO_MANIFEST_DIR")
            .ok_or("Cargo did not provide CARGO_MANIFEST_DIR to the build script")?,
    );
    let source = manifest_dir.join("../../defaults");
    emit_rerun_tree(&source)?;
    let destination = profile_dir()?.join("defaults");
    let staging = destination.with_extension(format!("stage-{}", std::process::id()));
    remove_staging_dir(&staging)?;
    copy_tree(&source, &staging)?;
    if destination.exists() {
        fs::remove_dir_all(&destination)?;
    }
    fs::rename(staging, destination)?;
    Ok(())
}

fn emit_rerun_tree(path: &Path) -> Result<(), Box<dyn Error>> {
    println!("cargo:rerun-if-changed={}", path.display());
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            emit_rerun_tree(&entry.path())?;
        } else {
            println!("cargo:rerun-if-changed={}", entry.path().display());
        }
    }
    Ok(())
}

fn copy_tree(source: &Path, destination: &Path) -> Result<(), Box<dyn Error>> {
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let target = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            copy_file(&entry.path(), &target)?;
        }
    }
    Ok(())
}

fn ensure_supported_target() -> Result<(), Box<dyn Error>> {
    let os = env::var("CARGO_CFG_TARGET_OS")?;
    let arch = env::var("CARGO_CFG_TARGET_ARCH")?;
    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if os == "linux" && arch == "x86_64" && target_env == "gnu" {
        return Ok(());
    }
    Err(format!(
        "automatic provisioning currently supports linux-x86_64-gnu, not {os}-{arch}-{target_env}"
    )
    .into())
}

fn read_checksum_spec(path: &Path) -> Result<(String, String), Box<dyn Error>> {
    let contents = fs::read_to_string(path)?;
    let mut fields = contents.split_whitespace();
    let checksum = fields.next().ok_or("Lemonade checksum file is empty")?;
    let asset = fields
        .next()
        .ok_or("Lemonade checksum file does not name an asset")?;
    if fields.next().is_some() || checksum.len() != 64 {
        return Err("Lemonade checksum file must contain one SHA-256 and asset name".into());
    }
    let expected_asset = format!("lemonade-embeddable-{LEMONADE_VERSION}-ubuntu-x64.tar.gz");
    if asset != expected_asset {
        return Err(format!(
            "Lemonade checksum names {asset}, expected pinned asset {expected_asset}"
        )
        .into());
    }
    Ok((checksum.to_owned(), asset.to_owned()))
}

fn artifact_is_current(
    install_dir: &Path,
    required_files: [&Path; 5],
    model_manifest: &Path,
    backend_versions: &Path,
    marker: &Path,
    expected_marker: &str,
) -> bool {
    if !install_dir.is_dir()
        || required_files.iter().any(|path| !path.is_file())
        || fs::read_to_string(marker).ok().as_deref() != Some(expected_marker)
    {
        return false;
    }
    verify_reasoning_patch(model_manifest).is_ok()
        && verify_llamacpp_11_5_1_pins(backend_versions).is_ok()
}

fn ensure_archive(
    archive: &Path,
    asset_name: &str,
    expected_checksum: &str,
) -> Result<(), Box<dyn Error>> {
    if archive.is_file() && sha256(archive)? == expected_checksum {
        return Ok(());
    }

    let temporary = archive.with_extension(format!("download-{}", std::process::id()));
    let _ = fs::remove_file(&temporary);
    let url = format!(
        "https://github.com/lemonade-sdk/lemonade/releases/download/v{LEMONADE_VERSION}/{asset_name}"
    );
    let status = Command::new("curl")
        .args([
            "--fail",
            "--location",
            "--retry",
            "3",
            "--connect-timeout",
            "10",
            "--max-time",
            "300",
            "--silent",
            "--show-error",
            "--output",
        ])
        .arg(&temporary)
        .arg(url)
        .status()
        .map_err(|error| format!("failed to run curl: {error}"))?;
    if !status.success() {
        let _ = fs::remove_file(&temporary);
        return Err(format!("curl exited with {status}").into());
    }
    let actual = sha256(&temporary)?;
    if actual != expected_checksum {
        let _ = fs::remove_file(&temporary);
        return Err(format!(
            "downloaded Lemonade checksum mismatch: expected {expected_checksum}, got {actual}"
        )
        .into());
    }
    fs::rename(&temporary, archive)?;
    Ok(())
}

fn sha256(path: &Path) -> Result<String, Box<dyn Error>> {
    let output = Command::new("sha256sum")
        .arg(path)
        .output()
        .map_err(|error| format!("failed to run sha256sum: {error}"))?;
    if !output.status.success() {
        return Err(format!("sha256sum exited with {}", output.status).into());
    }
    let stdout = String::from_utf8(output.stdout)?;
    stdout
        .split_whitespace()
        .next()
        .map(str::to_owned)
        .ok_or_else(|| "sha256sum returned no checksum".into())
}

fn install_archive(
    archive: &Path,
    asset_name: &str,
    staging: &Path,
    install_dir: &Path,
    marker_contents: &str,
) -> Result<(), Box<dyn Error>> {
    let status = Command::new("tar")
        .args([OsStr::new("-xzf")])
        .arg(archive)
        .arg("-C")
        .arg(staging)
        .status()
        .map_err(|error| format!("failed to run tar: {error}"))?;
    if !status.success() {
        return Err(format!("tar exited with {status}").into());
    }

    let root_name = asset_name
        .strip_suffix(".tar.gz")
        .ok_or("Lemonade asset is not a .tar.gz archive")?;
    let extracted = staging.join(root_name);
    let binary = extracted.join("lemond");
    let license = extracted.join("LICENSE");
    let model_manifest = extracted.join("resources/server_models.json");
    let backend_versions = extracted.join("resources/backend_versions.json");
    let defaults = extracted.join("resources/defaults.json");
    for required in [
        binary.as_path(),
        license.as_path(),
        model_manifest.as_path(),
        backend_versions.as_path(),
        defaults.as_path(),
    ] {
        if !required.is_file() {
            return Err(format!("Lemonade archive is missing {}", required.display()).into());
        }
    }

    let patched = patch_reasoning_labels(&model_manifest)?;
    verify_reasoning_patch(&model_manifest)?;
    patch_llamacpp_11_5_1_pins(&backend_versions)?;
    verify_llamacpp_11_5_1_pins(&backend_versions)?;

    let prepared = staging.join("lemonade");
    fs::create_dir_all(prepared.join("resources"))?;
    copy_file(&binary, &prepared.join("lemond"))?;
    copy_file(&license, &prepared.join("LICENSE"))?;
    for (source, name) in [
        (&model_manifest, "server_models.json"),
        (&backend_versions, "backend_versions.json"),
        (&defaults, "defaults.json"),
    ] {
        copy_file(source, &prepared.join("resources").join(name))?;
    }
    fs::write(prepared.join(".u-forge-embedded-version"), marker_contents)?;

    if install_dir.exists() {
        fs::remove_dir_all(install_dir)?;
    }
    fs::rename(&prepared, install_dir)?;
    println!(
        "cargo:warning=provisioned Embeddable Lemonade {LEMONADE_VERSION}; added reasoning to {patched} Gemma 4 GGUF models and pinned llama.cpp backends to Lemonade 11.5.1"
    );
    Ok(())
}

fn copy_file(source: &Path, destination: &Path) -> Result<(), Box<dyn Error>> {
    fs::copy(source, destination)?;
    Ok(())
}

fn patch_reasoning_labels(path: &Path) -> Result<usize, Box<dyn Error>> {
    let mut manifest: serde_json::Value = serde_json::from_slice(&fs::read(path)?)?;
    let models = manifest
        .as_object_mut()
        .ok_or("Lemonade server_models.json must contain an object")?;
    let mut matched = 0;
    for (name, model) in models {
        if !is_gemma4_gguf(name) {
            continue;
        }
        matched += 1;
        let model = model
            .as_object_mut()
            .ok_or_else(|| format!("Lemonade model {name} must contain an object"))?;
        let labels = model
            .entry("labels")
            .or_insert_with(|| serde_json::Value::Array(Vec::new()))
            .as_array_mut()
            .ok_or_else(|| format!("Lemonade model {name} labels must contain an array"))?;
        if !labels
            .iter()
            .any(|label| label.as_str() == Some("reasoning"))
        {
            labels.push(serde_json::Value::String("reasoning".to_owned()));
        }
    }
    if matched == 0 {
        return Err("Lemonade manifest contained no Gemma 4 GGUF models to patch".into());
    }
    let mut bytes = serde_json::to_vec_pretty(&manifest)?;
    bytes.push(b'\n');
    fs::write(path, bytes)?;
    Ok(matched)
}

fn verify_reasoning_patch(path: &Path) -> Result<(), Box<dyn Error>> {
    let manifest: serde_json::Value = serde_json::from_slice(&fs::read(path)?)?;
    let models = manifest
        .as_object()
        .ok_or("Lemonade server_models.json must contain an object")?;
    let matching = models
        .iter()
        .filter(|(name, _)| is_gemma4_gguf(name))
        .collect::<Vec<_>>();
    if matching.is_empty() {
        return Err("Lemonade manifest contained no Gemma 4 GGUF models".into());
    }
    for (name, model) in matching {
        let has_reasoning = model
            .get("labels")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|labels| {
                labels
                    .iter()
                    .any(|label| label.as_str() == Some("reasoning"))
            });
        if !has_reasoning {
            return Err(format!("Lemonade model {name} is missing reasoning").into());
        }
    }
    Ok(())
}

fn patch_llamacpp_11_5_1_pins(path: &Path) -> Result<(), Box<dyn Error>> {
    let mut manifest: serde_json::Value = serde_json::from_slice(&fs::read(path)?)?;
    let llamacpp = manifest
        .get_mut("llamacpp")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or("Lemonade backend_versions.json must contain a llamacpp object")?;
    for (backend, version) in LLAMACPP_11_5_1_PINS {
        llamacpp.insert(
            backend.to_owned(),
            serde_json::Value::String(version.to_owned()),
        );
    }
    let mut bytes = serde_json::to_vec_pretty(&manifest)?;
    bytes.push(b'\n');
    fs::write(path, bytes)?;
    Ok(())
}

fn verify_llamacpp_11_5_1_pins(path: &Path) -> Result<(), Box<dyn Error>> {
    let manifest: serde_json::Value = serde_json::from_slice(&fs::read(path)?)?;
    let llamacpp = manifest
        .get("llamacpp")
        .and_then(serde_json::Value::as_object)
        .ok_or("Lemonade backend_versions.json must contain a llamacpp object")?;
    for (backend, expected) in LLAMACPP_11_5_1_PINS {
        let actual = llamacpp.get(backend).and_then(serde_json::Value::as_str);
        if actual != Some(expected) {
            return Err(format!(
                "Lemonade llama.cpp backend {backend} is pinned to {actual:?}, expected {expected} from 11.5.1"
            )
            .into());
        }
    }
    Ok(())
}

fn is_gemma4_gguf(name: &str) -> bool {
    name.starts_with("Gemma-4-") && name.ends_with("-GGUF")
}

fn remove_staging_dir(path: &Path) -> io::Result<()> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn env_flag(name: &str) -> bool {
    env::var_os(name).is_some_and(|value| !value.is_empty() && value != "0")
}
