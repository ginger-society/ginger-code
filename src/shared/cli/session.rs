use IAMService::apis::configuration::Configuration as IAMConfiguration;
use IAMService::apis::default_api::identity_validate_api_token;
use MetadataService::apis::configuration::Configuration as MetadataConfiguration;
use crate::shared::tui::fetch_metadata_and_process;

// The outer fn main() is sync; this bridges into async for the one
// command that needs it.
#[tokio::main]
pub async fn check_session_guard(
    iam_config:      &IAMConfiguration,
    metadata_config: &MetadataConfiguration,
) {
    match identity_validate_api_token(iam_config).await {
        Ok(session_details) => {
            fetch_metadata_and_process(metadata_config, &session_details.sub).await;
        }
        Err(e) => {
            eprintln!("Token validation failed: {:?}", e);
            std::process::exit(1);
        }
    }
}