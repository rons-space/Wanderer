use crate::model_integrity::ARCFACE;
use image::GenericImageView;
use ndarray::{Array4, Axis, Ix2};
use std::io::Cursor;
use std::path::Path;
use tract_onnx::prelude::*;

pub mod arcface;
pub mod object_detection;
pub mod worker;

// Embed the model to simplify distribution
const MODEL_BYTES: &[u8] = include_bytes!("version-RFB-320_simplified.onnx");

pub type TractModel =
    RunnableModel<TypedFact, Box<dyn TypedOp>, Graph<TypedFact, Box<dyn TypedOp>>>;

pub struct FaceDetector {
    model: TractModel,
    priors: Vec<[f32; 4]>, // [cx, cy, w, h] normalized
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Face {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub score: f32,
}

impl FaceDetector {
    pub fn new() -> TractResult<Self> {
        let mut reader = Cursor::new(MODEL_BYTES);
        let model = tract_onnx::onnx()
            .model_for_read(&mut reader)?
            .with_input_fact(0, f32::fact([1, 3, 240, 320]).into())?
            .into_optimized()?
            .into_runnable()?;

        let priors = Self::generate_priors();
        log::debug!("FaceDetector: generated {} priors/anchors", priors.len());

        Ok(Self { model, priors })
    }

    fn generate_priors() -> Vec<[f32; 4]> {
        let input_w = 320.0;
        let input_h = 240.0;

        let feature_maps = [[40, 30], [20, 15], [10, 8], [5, 4]];
        let strides = [8.0, 16.0, 32.0, 64.0];
        let min_sizes = [
            vec![10.0, 16.0, 24.0],    // Stride 8 (3 scales) -> 40*30*3 = 3600
            vec![32.0, 48.0],          // Stride 16 (2 scales) -> 20*15*2 = 600
            vec![64.0, 96.0],          // Stride 32 (2 scales) -> 10*8*2 = 160
            vec![128.0, 176.0, 256.0], // Stride 64 (3 scales) -> 5*4*3 = 60
        ]; // Total: 4420

        let mut priors = Vec::with_capacity(4420);

        for (idx, &stride) in strides.iter().enumerate() {
            let (feat_w, feat_h) = (feature_maps[idx][0], feature_maps[idx][1]);
            for y in 0..feat_h {
                for x in 0..feat_w {
                    for &min_size in &min_sizes[idx] {
                        let cx = (x as f32 + 0.5) * stride / input_w;
                        let cy = (y as f32 + 0.5) * stride / input_h;
                        let w = min_size / input_w;
                        let h = min_size / input_h;
                        priors.push([cx, cy, w, h]);
                    }
                }
            }
        }
        priors
    }

    pub fn detect(&self, image_path: &Path) -> TractResult<Vec<Face>> {
        let image =
            image::open(image_path).map_err(|e| anyhow::anyhow!("Failed to open image: {}", e))?;
        let (width, height) = image.dimensions();
        let image_rgb = image.to_rgb8();

        // Resize to 320x240
        let resized =
            image::imageops::resize(&image_rgb, 320, 240, image::imageops::FilterType::Triangle);

        // Preprocess: (x - 127) / 128
        let tensor: Tensor = Array4::from_shape_fn((1, 3, 240, 320), |(_, c, y, x)| {
            let pixel = resized.get_pixel(x as u32, y as u32);
            let val = pixel[c] as f32;
            (val - 127.0) / 128.0
        })
        .into();

        let result = self.model.run(tvec!(tensor.into()))?;

        // Index 0: Confidences (1, N, 2)
        // Index 1: Boxes (1, N, 4)
        let scores_tensor = &result[0];
        let boxes_tensor = &result[1];

        let scores_view = scores_tensor.to_array_view::<f32>()?;
        let boxes_view = boxes_tensor.to_array_view::<f32>()?;

        // (N, 2) and (N, 4)
        let scores = scores_view
            .index_axis(Axis(0), 0)
            .into_dimensionality::<Ix2>()?;
        let boxes = boxes_view
            .index_axis(Axis(0), 0)
            .into_dimensionality::<Ix2>()?;

        let mut faces = Vec::new();
        let prob_threshold = 0.7;
        let iou_threshold = 0.3;

        let center_variance = 0.1;
        let size_variance = 0.2;

        let image_width = width as f32;
        let image_height = height as f32;

        for i in 0..scores.shape()[0] {
            let score = scores[(i, 1)]; // Class 1 is face

            if score > prob_threshold {
                let box_enc = boxes.index_axis(Axis(0), i);
                let prior = &self.priors[i];

                // Decode
                // box_enc = [d_cx, d_cy, d_w, d_h]
                // cx = prior_cx + d_cx * variance * prior_w
                // cy = prior_cy + d_cy * variance * prior_h
                // w = prior_w * exp(d_w * variance)
                // h = prior_h * exp(d_h * variance)

                let cx = prior[0] + box_enc[0] * center_variance * prior[2];
                let cy = prior[1] + box_enc[1] * center_variance * prior[3];
                let w = prior[2] * (box_enc[2] * size_variance).exp();
                let h = prior[3] * (box_enc[3] * size_variance).exp();

                // Convert center-size to top-left-size
                let x1 = cx - w / 2.0;
                let y1 = cy - h / 2.0;

                // Scale to original image
                let final_x = x1 * image_width;
                let final_y = y1 * image_height;
                let final_w = w * image_width;
                let final_h = h * image_height;

                let face = Face {
                    x: final_x,
                    y: final_y,
                    width: final_w,
                    height: final_h,
                    score,
                };

                faces.push(face);
            }
        }

        // NMS
        faces.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let mut keep = Vec::new();
        let mut suppressed = vec![false; faces.len()];

        for i in 0..faces.len() {
            if suppressed[i] {
                continue;
            }
            keep.push(faces[i].clone());

            for j in (i + 1)..faces.len() {
                if suppressed[j] {
                    continue;
                }

                let iou = compute_iou(&faces[i], &faces[j]);
                if iou > iou_threshold {
                    suppressed[j] = true;
                }
            }
        }

        Ok(keep)
    }
}

/// Download the ArcFace model
/// Download the ArcFace model with fallback URLs
pub async fn download_arcface_model<F>(
    models_dir: &Path,
    progress_callback: F,
) -> Result<(), String>
where
    F: Fn(&str, u64, u64) + Send + 'static + Clone,
{
    // List of mirrors to try.
    // The buffalo_l one seems to return 401 sometimes or is gated.
    // We can try other known hosting locations or mirrors.
    let urls = [
        // Mirror 1: yakhyo GitHub Release (v0.0.1) - Verified public asset
        "https://github.com/yakhyo/face-reidentification/releases/download/v0.0.1/w600k_r50.onnx",
        // Mirror 2: "maze" HuggingFace (seems public/ungated)
        "https://huggingface.co/maze/faceX/resolve/main/w600k_r50.onnx",
        // Mirror 3: "Aitrepreneur" HuggingFace
        "https://huggingface.co/Aitrepreneur/insightface/resolve/main/models/buffalo_l/w600k_r50.onnx",
        // Mirror 4: Original (just in case)
        "https://huggingface.co/buffalo_l/insightface_onnx/resolve/main/w600k_r50.onnx",
    ];

    let model_path = models_dir.join("w600k_r50.onnx");

    if model_path.exists() {
        // A size threshold used to stand in for a real check here, which accepted any
        // file over 100 MB. Verify the pin instead, and re-download anything that fails
        // rather than trusting what is on disk.
        match ARCFACE.verify_or_remove(&model_path) {
            Ok(()) => {
                log::info!("ArcFace model already present and verified");
                return Ok(());
            }
            Err(e) => log::warn!("Discarding the existing ArcFace model: {}", e),
        }
    }

    std::fs::create_dir_all(models_dir)
        .map_err(|e| format!("Failed to create models dir: {}", e))?;

    let client = reqwest::Client::new();
    let mut last_error = String::new();

    for (i, url) in urls.iter().enumerate() {
        log::info!(
            "Attempting to download ArcFace from URL {} ({})...",
            i + 1,
            url
        );

        match download_from_url(&client, url, &model_path, progress_callback.clone()).await {
            Ok(_) => match ARCFACE.verify_or_remove(&model_path) {
                Ok(()) => {
                    log::info!("ArcFace download complete and verified from URL {}", i + 1);
                    return Ok(());
                }
                Err(e) => {
                    // Treated like any other mirror failure: a mirror that no longer
                    // serves the pinned bytes is a mirror we move on from.
                    log::warn!("URL {} served an unexpected ArcFace model: {}", i + 1, e);
                    last_error = e;
                    continue;
                }
            },
            Err(e) => {
                log::warn!("Failed to download from URL {}: {}", i + 1, e);
                last_error = e;
                // Clean up partial file if it exists
                let temp_path = model_path.with_extension("tmp");
                if temp_path.exists() {
                    let _ = std::fs::remove_file(&temp_path);
                }
            }
        }
    }

    Err(format!(
        "All download attempts failed. Last error: {}",
        last_error
    ))
}

async fn download_from_url<F>(
    client: &reqwest::Client,
    url: &str,
    model_path: &Path,
    progress_callback: F,
) -> Result<(), String>
where
    F: Fn(&str, u64, u64) + Send + 'static,
{
    use futures_util::StreamExt;
    use std::io::Write;

    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("Request failed: {}", e))?;

    if !response.status().is_success() {
        return Err(format!("HTTP error: {}", response.status()));
    }

    let total_size = response.content_length().unwrap_or(175_000_000);
    let mut downloaded: u64 = 0;

    let temp_path = model_path.with_extension("tmp");
    let mut file = std::fs::File::create(&temp_path)
        .map_err(|e| format!("Failed to create temp file: {}", e))?;

    let mut stream = response.bytes_stream();

    // Initial progress
    progress_callback("w600k_r50.onnx", 0, total_size);

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("Stream error: {}", e))?;
        file.write_all(&chunk)
            .map_err(|e| format!("Write error: {}", e))?;
        downloaded += chunk.len() as u64;
        progress_callback("w600k_r50.onnx", downloaded, total_size);
    }

    std::fs::rename(&temp_path, model_path).map_err(|e| format!("Rename failed: {}", e))?;

    Ok(())
}

fn compute_iou(a: &Face, b: &Face) -> f32 {
    let x1 = a.x.max(b.x);
    let y1 = a.y.max(b.y);
    let x2 = (a.x + a.width).min(b.x + b.width);
    let y2 = (a.y + a.height).min(b.y + b.height);

    if x2 < x1 || y2 < y1 {
        return 0.0;
    }

    let intersection = (x2 - x1) * (y2 - y1);
    let area_a = a.width * a.height;
    let area_b = b.width * b.height;

    intersection / (area_a + area_b - intersection)
}
