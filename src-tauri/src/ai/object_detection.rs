//! Object Detection / Image Classification Module
//!
//! This module provides automatic tagging for images using MobileNet V2.
//! Instead of bounding-box detection (SSD), we use image classification
//! to generate semantic tags like "dog", "beach", "mountain", etc.
//!
//! The model is downloaded on first use from public model mirrors.

use crate::model_integrity::MOBILENET;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tract_onnx::prelude::*;

/// A loaded, runnable ONNX graph. Named for the same reason `clip.rs` names its own:
/// the inferred type is four lines of `tract` generics.
type RunnableModel = tract_onnx::prelude::SimplePlan<
    tract_onnx::prelude::TypedFact,
    Box<dyn tract_onnx::prelude::TypedOp>,
    tract_onnx::prelude::Graph<
        tract_onnx::prelude::TypedFact,
        Box<dyn tract_onnx::prelude::TypedOp>,
    >,
>;

/// Model singleton
static CLASSIFIER: OnceLock<Option<RunnableModel>> = OnceLock::new();

const PRIMARY_MODEL_NAME: &str = "mobilenet_v2.onnx";
const MODEL_ALIASES: &[&str] = &["MobileNetV2.onnx", "mobilenetv2.onnx", "mobilenet-v2.onnx"];

/// ImageNet class labels (top 100 most useful for photo tagging)
// Unread: `map_class_to_tag` below returns tags from hard-coded class ranges instead of
// indexing this table. Kept because it is the vocabulary that mapping approximates, and
// the replacement for that mapping needs it.
#[allow(dead_code)]
const IMAGENET_LABELS: &[&str] = &[
    "person",
    "bicycle",
    "car",
    "motorcycle",
    "airplane",
    "bus",
    "train",
    "truck",
    "boat",
    "bird",
    "cat",
    "dog",
    "horse",
    "sheep",
    "cow",
    "elephant",
    "bear",
    "zebra",
    "giraffe",
    "backpack",
    "umbrella",
    "handbag",
    "suitcase",
    "frisbee",
    "skis",
    "snowboard",
    "sports ball",
    "kite",
    "baseball bat",
    "baseball glove",
    "skateboard",
    "surfboard",
    "tennis racket",
    "bottle",
    "wine glass",
    "cup",
    "fork",
    "knife",
    "spoon",
    "bowl",
    "banana",
    "apple",
    "sandwich",
    "orange",
    "broccoli",
    "carrot",
    "hot dog",
    "pizza",
    "donut",
    "cake",
    "chair",
    "couch",
    "potted plant",
    "bed",
    "dining table",
    "toilet",
    "tv",
    "laptop",
    "mouse",
    "remote",
    "keyboard",
    "cell phone",
    "microwave",
    "oven",
    "toaster",
    "sink",
    "refrigerator",
    "book",
    "clock",
    "vase",
    "scissors",
    "teddy bear",
    "hair drier",
    "toothbrush",
    // Scene/Nature tags (common in photo apps)
    "beach",
    "mountain",
    "sunset",
    "sunrise",
    "sky",
    "water",
    "ocean",
    "lake",
    "river",
    "forest",
    "tree",
    "flower",
    "grass",
    "snow",
    "city",
    "building",
    "street",
    "bridge",
    // Food and drinks
    "food",
    "coffee",
    "wine",
    "beer",
    // Events
    "wedding",
    "party",
    "concert",
    "sports",
];

/// Check if the classification model is available
pub fn model_available(models_dir: &Path) -> bool {
    find_existing_model_path(models_dir).is_some()
}

/// Load the classification model (lazy initialization)
pub fn ensure_model_loaded(models_dir: &Path) -> Result<(), String> {
    if CLASSIFIER.get().is_some() {
        return Ok(());
    }

    let model_path = ensure_primary_model_file(models_dir)?;

    // Last point before the bytes reach the ONNX parser.
    MOBILENET.verify(&model_path)?;

    log::info!("Loading MobileNet V2 model from {:?}", model_path);

    let model = tract_onnx::onnx()
        .model_for_path(&model_path)
        .map_err(|e| format!("Failed to load model: {}", e))?
        .with_input_fact(0, f32::fact([1, 3, 224, 224]).into())
        .map_err(|e| format!("Failed to set input: {}", e))?
        .into_optimized()
        .map_err(|e| format!("Failed to optimize: {}", e))?
        .into_runnable()
        .map_err(|e| format!("Failed to make runnable: {}", e))?;

    let _ = CLASSIFIER.set(Some(model));
    log::info!("MobileNet V2 model loaded successfully");
    Ok(())
}

/// Classify an image and return top tags with confidence scores
/// Returns: Vec<(tag_name, confidence)> sorted by confidence descending
pub fn classify_image(image_path: &Path, top_k: usize) -> Result<Vec<(String, f32)>, String> {
    let model = CLASSIFIER
        .get()
        .ok_or("Model not loaded")?
        .as_ref()
        .ok_or("Model initialization failed")?;

    // Load and preprocess image
    let img = image::open(image_path).map_err(|e| format!("Failed to open image: {}", e))?;

    let resized = img.resize_exact(224, 224, image::imageops::FilterType::Triangle);
    let rgb = resized.to_rgb8();

    // ImageNet normalization: (x / 255 - mean) / std
    let mean = [0.485f32, 0.456, 0.406];
    let std = [0.229f32, 0.224, 0.225];

    let tensor: Tensor = tract_ndarray::Array4::from_shape_fn((1, 3, 224, 224), |(_, c, y, x)| {
        let pixel = rgb.get_pixel(x as u32, y as u32);
        let val = pixel[c] as f32 / 255.0;
        (val - mean[c]) / std[c]
    })
    .into();

    // Run inference
    let result = model
        .run(tvec!(tensor.into()))
        .map_err(|e| format!("Inference failed: {}", e))?;

    // Get predictions
    let predictions = result[0]
        .to_array_view::<f32>()
        .map_err(|e| format!("Failed to get output: {}", e))?;

    // Apply softmax and get top-k
    let preds = predictions.as_slice().ok_or("Failed to get slice")?;

    // Find max for softmax stability
    let max_val = preds.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
    let exp_sum: f32 = preds.iter().map(|x| (x - max_val).exp()).sum();

    let mut scored: Vec<(usize, f32)> = preds
        .iter()
        .enumerate()
        .map(|(i, &x)| (i, (x - max_val).exp() / exp_sum))
        .collect();

    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    // Map to labels (only keep those we have labels for, with threshold)
    let threshold = 0.05; // 5% minimum confidence
    let mut tags: Vec<(String, f32)> = Vec::new();

    for (idx, score) in scored.iter().take(top_k * 2) {
        if *score < threshold {
            break;
        }

        // Map ImageNet class index to our simplified label set
        if let Some(label) = map_class_to_tag(*idx) {
            // Avoid duplicates
            if !tags.iter().any(|(t, _)| t == &label) {
                tags.push((label, *score));
                if tags.len() >= top_k {
                    break;
                }
            }
        }
    }

    Ok(tags)
}

/// Map ImageNet class index to a simplified tag
/// ImageNet has 1000 classes, we map to ~100 useful photo tags
fn map_class_to_tag(class_idx: usize) -> Option<String> {
    // This is a simplified mapping - in production, you'd use a full lookup table
    // ImageNet class ranges (approximate):
    // 0-151: Animals
    // 152-297: Plants, fungi
    // 298-397: Objects
    // 398-491: Food
    // 492-591: Vehicles
    // 592-991: Other objects, scenes

    let tag = match class_idx {
        // Dogs (many ImageNet dog breeds map to "dog")
        151..=268 => "dog",
        // Cats
        281..=285 => "cat",
        // Birds
        7..=23 => "bird",
        // Cars/vehicles (non-overlapping ranges)
        407..=428 => "car",
        429..=435 | 469..=471 => "car", // Corrected to avoid overlap
        // Planes
        404..=406 => "airplane",
        // Boats
        472..=485 => "boat", // Adjusted to not overlap with music
        // Bicycles (removed - was inside car range)
        // Motorcycles
        670..=671 => "motorcycle",
        // Food items
        924..=969 => "food",
        // Flowers
        985..=987 => "flower",
        // People indicators (clothing, accessories)
        818..=837 => "person",
        // Sports equipment
        768..=800 => "sports",
        // Musical instruments
        514..=530 => "music", // Adjusted to no longer overlap
        // Furniture
        546..=570 => "furniture", // Adjusted to no longer overlap with car
        // Electronics
        531..=545 => "electronics", // Adjusted to no longer overlap with music
        // Nature scenes
        970..=980 => "nature",
        _ => return None,
    };

    Some(tag.to_string())
}

/// Download the MobileNet V2 model
pub async fn download_model<F>(models_dir: &Path, progress_callback: F) -> Result<(), String>
where
    F: Fn(&str, u64, u64) + Send + 'static + Clone,
{
    let model_path = ensure_primary_model_file(models_dir);
    if model_path.is_ok() {
        log::info!("MobileNet model already exists");
        return Ok(());
    }

    std::fs::create_dir_all(models_dir)
        .map_err(|e| format!("Failed to create models dir: {}", e))?;

    // Pinned to a commit rather than `main`: a branch ref means the bytes behind this
    // URL can change without the application noticing, and the SHA-256 below would then
    // reject a legitimate upstream update in a way nobody could diagnose from the error.
    // Pinning both together makes the two agree by construction.
    const ONNX_MODELS_COMMIT: &str = "4c46cd00fbdb7cd30b6c1c17ab54f2e1f4f7b177";
    let urls = [
        format!(
            "https://media.githubusercontent.com/media/onnx/models/{}/validated/vision/classification/mobilenet/model/mobilenetv2-7.onnx",
            ONNX_MODELS_COMMIT
        ),
        format!(
            "https://github.com/onnx/models/raw/{}/validated/vision/classification/mobilenet/model/mobilenetv2-7.onnx",
            ONNX_MODELS_COMMIT
        ),
    ];

    let client = reqwest::Client::new();
    let target_path = models_dir.join(PRIMARY_MODEL_NAME);
    let mut last_error = String::new();

    for (idx, url) in urls.iter().enumerate() {
        log::info!("Downloading MobileNet V2 from mirror {}: {}", idx + 1, url);

        match download_from_url(
            &client,
            url.as_str(),
            &target_path,
            progress_callback.clone(),
            PRIMARY_MODEL_NAME,
        )
        .await
        {
            Ok(_) => match MOBILENET.verify_or_remove(&target_path) {
                Ok(()) => {
                    log::info!(
                        "MobileNet V2 download complete and verified from mirror {}",
                        idx + 1
                    );
                    return Ok(());
                }
                Err(e) => {
                    log::warn!(
                        "Mirror {} served an unexpected MobileNet model: {}",
                        idx + 1,
                        e
                    );
                    last_error = e;
                }
            },
            Err(e) => {
                last_error = e.clone();
                log::warn!("Failed MobileNet download from mirror {}: {}", idx + 1, e);
            }
        }
    }

    Err(format!(
        "All MobileNet download attempts failed. Last error: {}",
        last_error
    ))
}

fn find_existing_model_path(models_dir: &Path) -> Option<PathBuf> {
    let primary = models_dir.join(PRIMARY_MODEL_NAME);
    if primary.exists() {
        return Some(primary);
    }

    MODEL_ALIASES
        .iter()
        .map(|name| models_dir.join(name))
        .find(|candidate| candidate.exists())
}

fn ensure_primary_model_file(models_dir: &Path) -> Result<PathBuf, String> {
    let primary = models_dir.join(PRIMARY_MODEL_NAME);
    if primary.exists() {
        return Ok(primary);
    }

    if let Some(existing) = find_existing_model_path(models_dir) {
        if existing != primary {
            std::fs::copy(&existing, &primary).map_err(|e| {
                format!(
                    "Failed to copy existing MobileNet model from {:?} to {:?}: {}",
                    existing, primary, e
                )
            })?;
            log::info!(
                "Copied existing MobileNet model from {:?} to {:?}",
                existing,
                primary
            );
        }
        return Ok(primary);
    }

    Err("MobileNet model not found. Please download it first.".to_string())
}

async fn download_from_url<F>(
    client: &reqwest::Client,
    url: &str,
    model_path: &Path,
    progress_callback: F,
    model_name: &str,
) -> Result<(), String>
where
    F: Fn(&str, u64, u64) + Send + 'static,
{
    use futures_util::StreamExt;
    use std::io::Write;

    progress_callback(model_name, 0, 14_000_000);

    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("Download request failed: {}", e))?;

    if !response.status().is_success() {
        return Err(format!("Download failed with status {}", response.status()));
    }

    let total_size = response.content_length().unwrap_or(14_000_000);
    let mut downloaded: u64 = 0;
    let temp_path = model_path.with_extension("tmp");

    let mut file =
        std::fs::File::create(&temp_path).map_err(|e| format!("Failed to create file: {}", e))?;
    let mut stream = response.bytes_stream();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("Download stream error: {}", e))?;
        file.write_all(&chunk)
            .map_err(|e| format!("Failed to write: {}", e))?;
        downloaded += chunk.len() as u64;
        progress_callback(model_name, downloaded, total_size);
    }

    if downloaded < 1_000_000 {
        let _ = std::fs::remove_file(&temp_path);
        return Err(format!(
            "Downloaded file too small ({} bytes), refusing to use it",
            downloaded
        ));
    }

    std::fs::rename(&temp_path, model_path).map_err(|e| format!("Failed to rename file: {}", e))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_map_class_to_tag() {
        assert_eq!(map_class_to_tag(207), Some("dog".to_string()));
        assert_eq!(map_class_to_tag(281), Some("cat".to_string()));
        assert_eq!(map_class_to_tag(7), Some("bird".to_string()));
    }
}
