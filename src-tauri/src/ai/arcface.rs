use ndarray::Array4;
use tract_onnx::prelude::*;

// Embed model or load from file?
// For now, assume model file "w600k_r50.onnx" is in a known location or embedded.
// Since it's large (~100MB+ usually), we shouldn't embed it. We'll load from disk.

pub struct ArcFace {
    model: crate::ai::TractModel,
}

impl ArcFace {
    pub fn new(models_dir: &std::path::Path) -> anyhow::Result<Self> {
        let model_path = models_dir.join("w600k_r50.onnx");

        // Simple blocking download check for now (since this is in a blocking thread or we make it async)
        // Ideally we should handle this async, but ArcFace::new is blocking.
        // We'll trust the caller (AiWorker) to handle downloading or we modify this signature.
        // For this immediate fix, we will check if it exists, and if not, return an error
        // OR better: we make a helper in mod.rs that can be called before this.

        if !model_path.exists() {
            return Err(anyhow::anyhow!(
                "ArcFace model not found at {:?}",
                model_path
            ));
        }

        // Verified here rather than only at download time, because this is the last
        // point before the bytes reach the ONNX parser, and the file could have been
        // replaced on disk since it was fetched.
        crate::model_integrity::ARCFACE
            .verify(&model_path)
            .map_err(|e| anyhow::anyhow!(e))?;

        let model = tract_onnx::onnx()
            .model_for_path(&model_path)?
            // ArcFace input: 112x112
            .with_input_fact(0, f32::fact([1, 3, 112, 112]).into())?
            .into_optimized()?
            .into_runnable()?;

        Ok(Self { model })
    }

    pub fn get_embedding(&self, image: &image::DynamicImage) -> anyhow::Result<Vec<f32>> {
        // Resize to 112x112
        // Note: For best results, this should be an *aligned* face crop.
        // Our existing FaceDetector gives a bounding box. We'll just crop and resize for now.
        // In future: Use landmarks (5 points) to align.

        let resized =
            image::imageops::resize(image, 112, 112, image::imageops::FilterType::Triangle);

        // Preprocess: (x - 127.5) / 128.0 (Standard for many ArcFace models)
        // OR (x - 127.5) / 127.5?
        // InsightFace generally uses: (x - 127.5) / 128.0

        let tensor: Tensor = Array4::from_shape_fn((1, 3, 112, 112), |(_, c, y, x)| {
            let pixel = resized.get_pixel(x as u32, y as u32);
            let val = pixel[c] as f32;
            (val - 127.5) / 128.0
        })
        .into();

        let result = self.model.run(tvec!(tensor.into()))?;

        // Output is usually (1, 512)
        let embedding_tensor = &result[0];
        let embedding_view = embedding_tensor.to_array_view::<f32>()?;

        let embedding: Vec<f32> = embedding_view.iter().cloned().collect();

        // Normalize embedding to unit length (important for cosine similarity)
        let norm: f32 = embedding.iter().map(|v| v * v).sum::<f32>().sqrt();
        let normalized: Vec<f32> = embedding.iter().map(|v| v / norm).collect();

        Ok(normalized)
    }
}
