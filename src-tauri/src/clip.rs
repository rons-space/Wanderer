//! CLIP Semantic Search Module
//!
//! This module implements natural language image search using OpenAI's CLIP model.
//! CLIP (Contrastive Language-Image Pre-training) embeds both images and text into
//! a shared 512-dimensional vector space, enabling semantic similarity search.
//!
//! ## Architecture
//! - Uses ONNX models (ViT-B/32 vision + text encoder)
//! - Images are embedded during background indexing
//! - Text queries are embedded at search time
//! - Cosine similarity finds most similar images
//!
//! ## Model Requirements
//! Models must be placed in the `models/` directory:
//! - `clip-vit-b32-vision.onnx` (~350MB)
//! - `clip-vit-b32-text-int8.onnx` (~65MB)
//! - `tokenizer.json`

use crate::model_integrity::{PinnedArtifact, CLIP_TEXTUAL, CLIP_TOKENIZER, CLIP_VISUAL};
use std::path::Path;
use std::sync::OnceLock;
use tokenizers::Tokenizer;
use tract_onnx::tract_hir::internal::*;

/// CLIP embedding dimension (ViT-B/32)
// Nothing validates embedding length against this today; kept as the documented shape of
// what `clip_embedding` rows contain.
#[allow(dead_code)]
pub const EMBEDDING_DIM: usize = 512;

/// Model filenames
pub const VISUAL_MODEL_NAME: &str = "clip-vit-b32-vision.onnx";
pub const TEXTUAL_MODEL_NAME: &str = "clip-vit-b32-text-int8.onnx";
pub const TOKENIZER_FILENAME: &str = "tokenizer.json";
const LEGACY_VISUAL_MODEL_NAME: &str = "clip-vit-b32-vision-int8.onnx";
const LEGACY_TEXTUAL_MODEL_NAME: &str = "clip-vit-b32-text.onnx";
const VISUAL_MODEL_CANDIDATES: &[&str] = &[VISUAL_MODEL_NAME, LEGACY_VISUAL_MODEL_NAME];
const TEXTUAL_MODEL_CANDIDATES: &[&str] = &[TEXTUAL_MODEL_NAME, LEGACY_TEXTUAL_MODEL_NAME];

type RunnableModel = tract_onnx::prelude::SimplePlan<
    tract_onnx::prelude::TypedFact,
    Box<dyn tract_onnx::prelude::TypedOp>,
    tract_onnx::prelude::Graph<
        tract_onnx::prelude::TypedFact,
        Box<dyn tract_onnx::prelude::TypedOp>,
    >,
>;

/// Static storage for loaded models
static VISUAL_MODEL: OnceLock<Option<RunnableModel>> = OnceLock::new();
static TEXTUAL_MODEL: OnceLock<Option<RunnableModel>> = OnceLock::new();

static TOKENIZER: OnceLock<Tokenizer> = OnceLock::new();

#[derive(Debug, Default, Clone, Hash)]
struct ClipRange;

impl tract_onnx::tract_hir::ops::expandable::Expansion for ClipRange {
    fn name(&self) -> std::borrow::Cow<'_, str> {
        "ClipRange".into()
    }

    fn rules<'r, 'p: 'r, 's: 'r>(
        &'s self,
        s: &mut Solver<'r>,
        inputs: &'p [TensorProxy],
        outputs: &'p [TensorProxy],
    ) -> InferenceResult {
        check_input_arity(inputs, 3)?;
        check_output_arity(outputs, 1)?;

        s.given_3(
            &inputs[0].datum_type,
            &inputs[1].datum_type,
            &inputs[2].datum_type,
            move |s, dt0, dt1, dt2| {
                let mut dt =
                    DatumType::super_type_for([dt0, dt1, dt2]).context("No supertype found")?;
                // CLIP textual ONNX uses Shape(TDim) + I64 literals. Force Range output to I64
                // to match ONNX graph expectations and avoid TDim/I64 unification failure.
                if dt == DatumType::TDim {
                    dt = DatumType::I64;
                }
                s.equals(dt, &outputs[0].datum_type)
            },
        )?;
        s.equals(&inputs[0].rank, 0)?;
        s.equals(&inputs[1].rank, 0)?;
        s.equals(&inputs[2].rank, 0)?;
        s.equals(&outputs[0].rank, 1)?;
        s.given_3(
            &inputs[0].value,
            &inputs[1].value,
            &inputs[2].value,
            move |s, v0, v1, v2| {
                let v0 = v0.cast_to::<TDim>()?;
                let v1 = v1.cast_to::<TDim>()?;
                let v2 = v2.cast_to::<i64>()?;
                let out = (v1.to_scalar::<TDim>()?.clone() - v0.to_scalar::<TDim>()?)
                    .divceil(*v2.to_scalar::<i64>()? as _);
                s.equals(&outputs[0].shape[0], out)
            },
        )?;
        Ok(())
    }

    fn wire(
        &self,
        prefix: &str,
        model: &mut TypedModel,
        inputs: &[OutletId],
    ) -> TractResult<TVec<OutletId>> {
        let mut dt: DatumType = DatumType::super_type_for(
            inputs
                .iter()
                .map(|o| model.outlet_fact(*o).expect("valid outlet").datum_type),
        )
        .context("No supertype for inputs")?;
        if dt == DatumType::TDim {
            dt = DatumType::I64;
        }
        let casted = tract_onnx::tract_core::ops::cast::wire_cast(prefix, model, inputs, dt)?;
        let len = model.symbols.new_with_prefix("range");
        model.wire_node(
            prefix,
            tract_onnx::tract_core::ops::array::Range::new(len.into()),
            &casted,
        )
    }
}

fn clip_onnx() -> tract_onnx::Onnx {
    let mut onnx = tract_onnx::onnx();
    onnx.op_register.insert("Range", |_, _| {
        Ok((
            tract_onnx::tract_hir::ops::expandable::expand(ClipRange),
            vec![],
        ))
    });
    onnx
}

/// The pin describing a CLIP file, when this version is the one that fetched it.
fn pin_for(filename: &str) -> Option<PinnedArtifact> {
    match filename {
        VISUAL_MODEL_NAME => Some(CLIP_VISUAL),
        TEXTUAL_MODEL_NAME => Some(CLIP_TEXTUAL),
        TOKENIZER_FILENAME => Some(CLIP_TOKENIZER),
        _ => None,
    }
}

fn has_any_model(models_dir: &Path, candidates: &[&str]) -> bool {
    candidates.iter().any(|name| models_dir.join(name).exists())
}

fn load_model_with_fallback(
    models_dir: &Path,
    role: &str,
    candidates: &[&str],
) -> Result<RunnableModel, String> {
    use tract_onnx::prelude::*;

    let mut errors: Vec<String> = Vec::new();

    for filename in candidates {
        let model_path = models_dir.join(filename);
        if !model_path.exists() {
            continue;
        }

        // The pins cover the files this version downloads. The legacy names are
        // whatever an older build left behind, at a quantization these digests do not
        // describe, so they are loaded with a warning rather than rejected: refusing
        // them would break CLIP for anyone who installed it before verification
        // existed. Re-running the model download replaces them with verified files.
        match pin_for(filename) {
            Some(pin) => pin.verify(&model_path)?,
            None => log::warn!(
                "Loading {} without an integrity check: it predates pinned models.                  Re-download the CLIP models to replace it.",
                filename
            ),
        }

        log::info!("Loading CLIP {} model from {:?}", role, model_path);

        let optimized_attempt = clip_onnx()
            .model_for_path(&model_path)
            .map_err(|e| format!("load error: {}", e))
            .and_then(|model| apply_input_facts(model, role))
            .and_then(|model| {
                model
                    .into_optimized()
                    .map_err(|e| format!("optimize error: {}", e))
            })
            .and_then(|model| {
                model
                    .into_runnable()
                    .map_err(|e| format!("runnable error: {}", e))
            });

        match optimized_attempt {
            Ok(model) => return Ok(model),
            Err(opt_err) => {
                // Fallback: some ONNX graphs fail tract optimizations but can run typed.
                let unoptimized_attempt = clip_onnx()
                    .model_for_path(&model_path)
                    .map_err(|e| format!("load error: {}", e))
                    .and_then(|model| apply_input_facts(model, role))
                    .and_then(|model| {
                        model
                            .into_typed()
                            .map_err(|e| format!("typed error: {}", e))
                    })
                    .and_then(|model| {
                        model
                            .into_runnable()
                            .map_err(|e| format!("runnable error: {}", e))
                    });

                match unoptimized_attempt {
                    Ok(model) => {
                        log::warn!(
                            "Loaded CLIP {} model without optimization from {:?}",
                            role,
                            model_path
                        );
                        return Ok(model);
                    }
                    Err(typed_err) => {
                        log::warn!(
                            "Failed to initialize CLIP {} model from {:?}: optimized={} | typed={}",
                            role,
                            model_path,
                            opt_err,
                            typed_err
                        );
                        errors.push(format!(
                            "{} -> optimize: {} | typed: {}",
                            filename, opt_err, typed_err
                        ));
                    }
                }
            }
        };
    }

    if errors.is_empty() {
        return Err(format!(
            "No {} model file found (expected one of: {})",
            role,
            candidates.join(", ")
        ));
    }

    Err(format!(
        "All {} model files failed to initialize: {}. Re-download CLIP models from Settings.",
        role,
        errors.join(" | ")
    ))
}

fn apply_input_facts(
    mut model: tract_onnx::prelude::InferenceModel,
    role: &str,
) -> Result<tract_onnx::prelude::InferenceModel, String> {
    use tract_onnx::prelude::*;

    match role {
        "visual" => model
            .with_input_fact(0, f32::fact([1, 3, 224, 224]).into())
            .map_err(|e| format!("Failed to set visual input fact: {}", e)),
        "textual" => {
            let input_count = model
                .input_outlets()
                .map_err(|e| format!("Failed to inspect textual inputs: {}", e))?
                .len();

            for ix in 0..input_count {
                model = model
                    .with_input_fact(ix, i64::fact([1, 77]).into())
                    .map_err(|e| format!("Failed to set textual input fact #{}: {}", ix, e))?;
            }
            Ok(model)
        }
        _ => Ok(model),
    }
}

/// Check if CLIP models are available
pub fn models_available(models_dir: &Path) -> bool {
    models_dir.join(TOKENIZER_FILENAME).exists()
        && has_any_model(models_dir, VISUAL_MODEL_CANDIDATES)
        && has_any_model(models_dir, TEXTUAL_MODEL_CANDIDATES)
}

/// Initialize CLIP models (lazy loading)
/// Returns true if models were loaded successfully
pub fn ensure_models_loaded(models_dir: &Path) -> Result<(), String> {
    // Check if already loaded
    if VISUAL_MODEL.get().is_some() && TEXTUAL_MODEL.get().is_some() && TOKENIZER.get().is_some() {
        return Ok(());
    }

    let tokenizer_path = models_dir.join(TOKENIZER_FILENAME);

    if !has_any_model(models_dir, VISUAL_MODEL_CANDIDATES) {
        return Err(format!(
            "Visual model not found. Expected one of: {}",
            VISUAL_MODEL_CANDIDATES.join(", ")
        ));
    }
    if !has_any_model(models_dir, TEXTUAL_MODEL_CANDIDATES) {
        return Err(format!(
            "Textual model not found. Expected one of: {}",
            TEXTUAL_MODEL_CANDIDATES.join(", ")
        ));
    }
    if !tokenizer_path.exists() {
        return Err(format!("Tokenizer not found at {:?}", tokenizer_path));
    }

    // Load Tokenizer
    if TOKENIZER.get().is_none() {
        CLIP_TOKENIZER.verify(&tokenizer_path)?;
        log::info!("Loading tokenizer from {:?}", tokenizer_path);
        let tokenizer = Tokenizer::from_file(&tokenizer_path)
            .map_err(|e| format!("Failed to load tokenizer: {}", e))?;
        let _ = TOKENIZER.set(tokenizer);
    }

    // Load Visual Model
    if VISUAL_MODEL.get().is_none() {
        let visual_model = load_model_with_fallback(models_dir, "visual", VISUAL_MODEL_CANDIDATES)?;
        let _ = VISUAL_MODEL.set(Some(visual_model));
    }

    // Load Textual Model
    if TEXTUAL_MODEL.get().is_none() {
        let textual_model =
            load_model_with_fallback(models_dir, "textual", TEXTUAL_MODEL_CANDIDATES)?;
        let _ = TEXTUAL_MODEL.set(Some(textual_model));
    }

    log::info!("CLIP models loaded successfully");
    Ok(())
}

/// Generate embedding for an image
pub fn encode_image(image_path: &Path) -> Result<Vec<f32>, String> {
    use image::GenericImageView;
    use tract_onnx::prelude::*;

    let model = VISUAL_MODEL
        .get()
        .ok_or("Visual model not loaded. Call ensure_models_loaded first.")?
        .as_ref()
        .ok_or("Visual model initialization failed")?;

    // Open image
    let img = image::open(image_path).map_err(|e| format!("Failed to open image: {}", e))?;

    // Resize to 224x224
    let resized = img.resize_exact(224, 224, image::imageops::FilterType::Triangle);

    // Normalize and convert to NCHW
    // CLIP Mean and Std for normalization
    // The published CLIP constants, kept as written upstream rather than truncated to the
    // nearest `f32`, so they stay recognisable against the reference implementation.
    #[allow(clippy::excessive_precision)]
    let mean = [0.48145466, 0.4578275, 0.40821073];
    #[allow(clippy::excessive_precision)]
    let std = [0.26862954, 0.26130258, 0.27577711];

    let image_tensor: Tensor =
        tract_ndarray::Array4::from_shape_fn((1, 3, 224, 224), |(_, c, y, x)| {
            let pixel = resized.get_pixel(x as u32, y as u32);
            let val = pixel[c] as f32 / 255.0;
            (val - mean[c]) / std[c]
        })
        .into();

    // Run inference
    let result = model
        .run(tvec!(image_tensor.into()))
        .map_err(|e| format!("Inference failed: {}", e))?;

    // Extract embedding (output 0)
    let embedding_tensor = &result[0];
    let embedding_vec: Vec<f32> = embedding_tensor
        .to_array_view::<f32>()
        .map_err(|e| e.to_string())?
        .as_slice()
        .ok_or("Failed to convert tensor to slice")?
        .to_vec();

    let mut final_embedding = embedding_vec;
    normalize_embedding(&mut final_embedding);

    Ok(final_embedding)
}

/// Generate embedding for a text query
/// Returns a 512-dimensional normalized vector
pub fn encode_text(query: &str) -> Result<Vec<f32>, String> {
    use tract_onnx::prelude::*;

    let tokenizer = TOKENIZER
        .get()
        .ok_or("Tokenizer not loaded. Call ensure_models_loaded first.")?;

    let model = TEXTUAL_MODEL
        .get()
        .ok_or("Textual model not loaded. Call ensure_models_loaded first.")?
        .as_ref()
        .ok_or("Textual model initialization failed")?;

    // Tokenize
    let encoding = tokenizer
        .encode(query, true)
        .map_err(|e| format!("Tokenization failed: {}", e))?;

    // CLIP expects fixed-length input (77 tokens)
    let ids = encoding.get_ids();
    let mut final_ids = vec![0i64; 77];

    // Copy tokens, truncating if necessary
    let len = ids.len().min(77);
    for i in 0..len {
        final_ids[i] = ids[i] as i64;
    }

    // Create tensor
    let input_ids = tract_ndarray::Array2::from_shape_vec((1, 77), final_ids)
        .map_err(|e| e.to_string())?
        .into_tensor();

    // Run inference
    let result = model
        .run(tvec!(input_ids.into()))
        .map_err(|e| format!("Inference failed: {}", e))?;

    // Extract embedding (usually the first output)
    let embedding_tensor = &result[0];
    let embedding_vec: Vec<f32> = embedding_tensor
        .to_array_view::<f32>()
        .map_err(|e| e.to_string())?
        .as_slice()
        .ok_or("Failed to convert tensor to slice")?
        .to_vec();

    let mut final_embedding = embedding_vec;
    normalize_embedding(&mut final_embedding);

    Ok(final_embedding)
}

/// Compute cosine similarity between two embeddings
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }

    let dot_product: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();

    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }

    dot_product / (norm_a * norm_b)
}

/// Normalize an embedding vector to unit length
pub fn normalize_embedding(embedding: &mut [f32]) {
    let norm: f32 = embedding.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in embedding.iter_mut() {
            *x /= norm;
        }
    }
}

/// Download CLIP models from HuggingFace
/// progress_callback: (model_name, current_bytes, total_bytes)
pub async fn download_models<F>(models_dir: &Path, progress_callback: F) -> Result<(), String>
where
    F: Fn(String, u64, u64) + Send + Sync + 'static + Clone,
{
    use futures_util::StreamExt;
    use std::io::Write;

    if !models_dir.exists() {
        std::fs::create_dir_all(models_dir).map_err(|e| e.to_string())?;
    }

    // URLs for ViT-B/32 ONNX.
    // We use non-quantized vision for tract compatibility and quantized text to save space.
    // Source: Xenova/clip-vit-base-patch32, pinned to a commit rather than `main` so the
    // bytes cannot change underneath the digests in `model_integrity`.
    const CLIP_REVISION: &str = "d15189d7028b43f1d3e65039190477f6af591c2a";
    let base = format!(
        "https://huggingface.co/Xenova/clip-vit-base-patch32/resolve/{}",
        CLIP_REVISION
    );

    let downloads = vec![
        (
            format!("{}/onnx/vision_model.onnx", base),
            VISUAL_MODEL_NAME,
            CLIP_VISUAL,
        ),
        (
            format!("{}/onnx/text_model_quantized.onnx", base),
            TEXTUAL_MODEL_NAME,
            CLIP_TEXTUAL,
        ),
        (
            format!("{}/tokenizer.json", base),
            TOKENIZER_FILENAME,
            CLIP_TOKENIZER,
        ),
    ];

    let client = reqwest::Client::new();

    for (url, filename, pin) in downloads {
        let dest_path = models_dir.join(filename);
        if dest_path.exists() {
            match pin.verify_or_remove(&dest_path) {
                Ok(()) => {
                    log::info!("Model {} already present and verified", filename);
                    progress_callback(filename.to_string(), 100, 100); // 100%
                    continue;
                }
                Err(e) => log::warn!("Re-downloading {}: {}", filename, e),
            }
        }

        log::info!("Downloading {} from {}", filename, url);
        progress_callback(filename.to_string(), 0, 0);

        let res = client
            .get(url)
            .send()
            .await
            .map_err(|e| format!("Failed to make request: {}", e))?;

        let total_size = res.content_length().unwrap_or(0);
        let mut stream = res.bytes_stream();
        let mut file = std::fs::File::create(&dest_path).map_err(|e| e.to_string())?;
        let mut downloaded: u64 = 0;

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| format!("Error downloading chunk: {}", e))?;
            file.write_all(&chunk).map_err(|e| e.to_string())?;
            downloaded += chunk.len() as u64;
            progress_callback(filename.to_string(), downloaded, total_size);
        }

        // Drop the handle before hashing so the last write is on disk.
        drop(file);

        pin.verify_or_remove(&dest_path)?;
        log::info!("Successfully downloaded and verified {}", filename);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cosine_similarity() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![1.0, 0.0, 0.0];
        assert!((cosine_similarity(&a, &b) - 1.0).abs() < 0.0001);

        let c = vec![0.0, 1.0, 0.0];
        assert!((cosine_similarity(&a, &c) - 0.0).abs() < 0.0001);
    }
}
