use std::{
    collections::{HashMap, HashSet}, fs, path::Path, process::{Command, exit}, time::Duration
};

use IAMService::apis::configuration::Configuration as IAMConfiguration;


use MetadataService::{
    apis::{
        configuration::Configuration as MetadataConfiguration,
        default_api::{
            metadata_get_services_and_envs, MetadataGetServicesAndEnvsParams
        },
    },
   
};
use ginger_shared_rs::{LANG, read_service_config_file, utils::get_package_json_info};
use toml::Value;

// TODO: move this to ginger-shared-rs as it is duplicating with ginger-connector
pub fn get_cargo_toml_info() -> Option<(String, String, String, String, Vec<String>)> {
    let cargo_toml_content = fs::read_to_string("Cargo.toml").expect("Failed to read Cargo.toml");
    let cargo_toml: Value =
        toml::from_str(&cargo_toml_content).expect("Failed to parse Cargo.toml");

    if let Some(package) = cargo_toml.get("package") {
        let name = package.get("name")?.as_str()?.to_string();
        let version = package.get("version")?.as_str()?.to_string();
        let description = package.get("description")?.as_str()?.to_string();
        let mut internal_dependencies = Vec::new();

        let metadata = cargo_toml
            .get("package")
            .and_then(|pkg| pkg.get("metadata"))
            .expect("there is no metadata field in your cargo.toml");
        let organization = metadata.get("organization")?.as_str()?.to_string();

        // Extract dependencies
        let dependencies = cargo_toml
            .get("dependencies")
            .expect("there is no dependencies field in your Cargo.toml");

        if let Some(deps) = dependencies.as_table() {
            for (key, value) in deps {
                if let Some(dep_table) = value.as_table() {
                    // Check if the dependency has an organization field
                    if let Some(dep_org) = dep_table.get("organization") {
                        if dep_org.as_str()? == organization {
                            let dep_format = format!("@{}/{}", organization, key);
                            internal_dependencies.push(dep_format);
                        }
                    }
                }
            }
        }

        Some((
            name,
            version,
            description,
            organization,
            internal_dependencies,
        ))
    } else {
        None
    }
}

// TODO: move this to ginger-shared-rs as it is duplicating with ginger-connector
pub fn get_pyproject_toml_info() -> Option<(String, String, String, String, Vec<String>)> {
    // Read and parse pyproject.toml
    let pyproject_toml_content =
        fs::read_to_string("pyproject.toml").expect("Failed to read pyproject.toml");
    let pyproject_toml: Value =
        toml::from_str(&pyproject_toml_content).expect("Failed to parse pyproject.toml");

    let name = pyproject_toml.get("name")?.as_str()?.to_string();
    let version = pyproject_toml.get("version")?.as_str()?.to_string();
    let description = pyproject_toml.get("description")?.as_str()?.to_string();
    let organization = pyproject_toml.get("organization")?.as_str()?.to_string();

    // Read and process requirements.txt
    let requirements_path = Path::new("requirements.txt");
    let mut dependencies = Vec::new();

    if requirements_path.exists() {
        let requirements_content =
            fs::read_to_string(requirements_path).expect("Failed to read requirements.txt");

        for line in requirements_content.lines() {
            let trimmed_line = line.trim();

            if trimmed_line.is_empty() {
                continue; // Skip empty lines
            }

            // If the line starts with '#', treat it as an internal dependency
            if trimmed_line.starts_with('#') {
                let internal_dependency = trimmed_line.trim_start_matches('#').trim();
                dependencies.push(internal_dependency.to_string());
                continue;
            }

            // Split the line on '#', if any
            let parts: Vec<&str> = trimmed_line.split('#').collect();
            let mut dependency = parts[0].trim().to_string();

            // Remove ==version if present
            if let Some((dep_name, _version)) = dependency.split_once("==") {
                dependency = dep_name.to_string();
            }

            if parts.len() > 1 {
                let org = parts[1].trim();

                // Check if the organization matches the one from pyproject.toml
                if org == organization {
                    dependencies.push(format!("@{}/{}", org, dependency));
                }
            }
        }
    }

    Some((name, version, description, organization, dependencies))
}


pub async fn fetch_metadata_and_process(
    config_path: &Path,
    iam_config: &IAMConfiguration,
    metadata_config: &MetadataConfiguration,
) {
    let mut config = read_service_config_file(config_path).unwrap();
    match metadata_get_services_and_envs(
        metadata_config,
        MetadataGetServicesAndEnvsParams {
            page_number: Some("1".to_string()),
            page_size: Some("50".to_string()),
            org_id: config.organization_id.clone(),
        },
    )
    .await
    {
        Ok(services) => {
            let (
                mut current_package_name,
                version,
                description,
                organization,
                internal_dependencies,
            ) = match config.lang {
                LANG::TS => get_package_json_info().unwrap_or_else(|| {
                    eprintln!("Failed to get name and version from package.json");
                    exit(1);
                }),
                LANG::Rust => get_cargo_toml_info().unwrap_or_else(|| {
                    eprintln!("Failed to get name and version from Cargo.toml");
                    exit(1);
                }),
                LANG::Python => get_pyproject_toml_info().unwrap_or_else(|| {
                    eprintln!("Failed to get name and version from pyproject.toml");
                    exit(1);
                }),
                LANG::Shell => todo!(),
            };

            if config.override_name.is_some() {
                current_package_name = config.override_name.clone().unwrap()
            }

            println!("Package name: {}", current_package_name);
            println!("Package version: {}", version);
            println!("Package organization: {}", organization);
            println!("Package description: {}", description);

            let service_names: Vec<String> = services
                .iter()
                .filter(|s| s.identifier != current_package_name)
                .map(|s| format!("@{}/{}", s.organization_id.clone(), s.identifier.clone()))
                .collect();

            println!("\nAvailable services:");
            for service_name in service_names.iter() {
                println!("  {}", service_name);
            }
        }
        Err(e) => {
            println!("{:?}", e);
            println!("Unable to get the metadata for this template");
            exit(1)
        }
    };
}

