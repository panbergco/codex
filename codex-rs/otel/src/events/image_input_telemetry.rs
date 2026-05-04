use codex_protocol::models::ContentItem;
use codex_protocol::models::FunctionCallOutputContentItem;
use codex_protocol::models::ResponseItem;
use serde::Serialize;
use std::collections::BTreeSet;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ImageInputTelemetry {
    pub(crate) details_json: String,
    pub(crate) image_types: Vec<String>,
    pub(crate) mime_types: Vec<String>,
    pub(crate) user_message_image_count: usize,
    pub(crate) tool_output_image_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct ImageInputDetail {
    source: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    image_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mime_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    height: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    byte_length: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    extension: Option<String>,
}

pub(crate) fn model_input_image_telemetry(items: &[ResponseItem]) -> ImageInputTelemetry {
    let mut details: Vec<ImageInputDetail> = Vec::new();
    let mut user_message_image_count = 0;
    let mut tool_output_image_count = 0;

    for item in items {
        match item {
            ResponseItem::Message { content, .. } => {
                for content_item in content {
                    if let ContentItem::InputImage { image_url, .. } = content_item {
                        user_message_image_count += 1;
                        details.push(image_url_detail("message", image_url));
                    }
                }
            }
            ResponseItem::FunctionCallOutput { output, .. } => {
                if let Some(content_items) = output.content_items() {
                    tool_output_image_count += collect_tool_output_image_details(
                        "tool_output",
                        content_items,
                        &mut details,
                    );
                }
            }
            ResponseItem::CustomToolCallOutput { output, .. } => {
                if let Some(content_items) = output.content_items() {
                    tool_output_image_count += collect_tool_output_image_details(
                        "custom_tool_output",
                        content_items,
                        &mut details,
                    );
                }
            }
            _ => {}
        }
    }

    let image_types = collect_unique_strings(
        details
            .iter()
            .filter_map(|detail| detail.image_type.as_deref()),
    );
    let mime_types = collect_unique_strings(
        details
            .iter()
            .filter_map(|detail| detail.mime_type.as_deref()),
    );
    let details_json = if details.is_empty() {
        String::new()
    } else {
        serde_json::to_string(&details).unwrap_or_default()
    };

    ImageInputTelemetry {
        details_json,
        image_types,
        mime_types,
        user_message_image_count,
        tool_output_image_count,
    }
}

fn collect_tool_output_image_details(
    source: &'static str,
    content_items: &[FunctionCallOutputContentItem],
    details: &mut Vec<ImageInputDetail>,
) -> usize {
    let mut image_count = 0;
    for content_item in content_items {
        if let FunctionCallOutputContentItem::InputImage { image_url, .. } = content_item {
            details.push(image_url_detail(source, image_url));
            image_count += 1;
        }
    }
    image_count
}

fn image_url_detail(source: &'static str, image_url: &str) -> ImageInputDetail {
    if let Some(detail) = data_url_detail(source, image_url) {
        return detail;
    }

    let extension = safe_image_extension_from_url(image_url);
    let mime_type = extension.as_deref().and_then(mime_type_from_extension);
    ImageInputDetail {
        source,
        image_type: mime_type.as_deref().and_then(image_type_from_mime),
        mime_type,
        width: None,
        height: None,
        byte_length: None,
        extension,
    }
}

fn data_url_detail(source: &'static str, image_url: &str) -> Option<ImageInputDetail> {
    let image_url = strip_data_scheme(image_url)?;
    let (metadata, payload) = image_url.split_once(',')?;
    let mut metadata_parts = metadata.split(';');
    let mime_type = normalize_known_image_mime(metadata_parts.next()?)?;
    let is_base64 = metadata_parts.any(|part| part.eq_ignore_ascii_case("base64"));

    Some(ImageInputDetail {
        source,
        image_type: image_type_from_mime(&mime_type),
        mime_type: Some(mime_type),
        width: None,
        height: None,
        byte_length: is_base64.then(|| base64_payload_byte_len(payload)),
        extension: None,
    })
}

fn strip_data_scheme(image_url: &str) -> Option<&str> {
    let scheme = image_url.get(..5)?;
    if !scheme.eq_ignore_ascii_case("data:") {
        return None;
    }
    image_url.get(5..)
}

fn safe_image_extension_from_url(url: &str) -> Option<String> {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let extension = Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)?;
    mime_type_from_extension(&extension)?;
    Some(extension)
}

fn normalize_known_image_mime(mime_type: &str) -> Option<String> {
    match mime_type.trim().to_ascii_lowercase().as_str() {
        "image/png" => Some("image/png".to_string()),
        "image/jpeg" | "image/jpg" => Some("image/jpeg".to_string()),
        "image/gif" => Some("image/gif".to_string()),
        "image/webp" => Some("image/webp".to_string()),
        "image/bmp" => Some("image/bmp".to_string()),
        "image/heic" => Some("image/heic".to_string()),
        "image/heif" => Some("image/heif".to_string()),
        "image/tiff" => Some("image/tiff".to_string()),
        "image/svg+xml" => Some("image/svg+xml".to_string()),
        _ => None,
    }
}

fn mime_type_from_extension(extension: &str) -> Option<String> {
    match extension {
        "png" => Some("image/png".to_string()),
        "jpg" | "jpeg" => Some("image/jpeg".to_string()),
        "gif" => Some("image/gif".to_string()),
        "webp" => Some("image/webp".to_string()),
        "bmp" => Some("image/bmp".to_string()),
        "heic" => Some("image/heic".to_string()),
        "heif" => Some("image/heif".to_string()),
        "tif" | "tiff" => Some("image/tiff".to_string()),
        "svg" => Some("image/svg+xml".to_string()),
        _ => None,
    }
}

fn image_type_from_mime(mime_type: &str) -> Option<String> {
    mime_type
        .strip_prefix("image/")
        .map(str::to_ascii_lowercase)
}

fn base64_payload_byte_len(payload: &str) -> u64 {
    let trimmed = payload.trim_end_matches('=');
    ((trimmed.len() * 3) / 4) as u64
}

fn collect_unique_strings<'a>(values: impl Iterator<Item = &'a str>) -> Vec<String> {
    values
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(str::to_string)
        .collect()
}
