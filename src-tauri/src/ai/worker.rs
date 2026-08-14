use crate::ai::arcface::ArcFace;
use crate::ai::object_detection;
use crate::ai::FaceDetector;
use crate::database::Database;
use image::GenericImageView;
use std::sync::Arc;
use std::time::{Duration as StdDuration, Instant};
use tokio::sync::Mutex;
use tokio::time::{sleep, Duration};
use tokio_util::sync::CancellationToken;

pub struct AiWorker {
    db: Arc<Database>,
    detector: Option<Arc<Mutex<FaceDetector>>>,
    arcface: Arc<Mutex<Option<ArcFace>>>, // Lazy load or load at startup
    models_dir: std::path::PathBuf,
}

impl AiWorker {
    pub fn new(
        db: Arc<Database>,
        detector: Option<Arc<Mutex<FaceDetector>>>,
        models_dir: std::path::PathBuf,
    ) -> Self {
        Self {
            db,
            detector,
            arcface: Arc::new(Mutex::new(None)),
            models_dir,
        }
    }

    fn config_enabled(&self, key: &str) -> bool {
        matches!(self.db.get_config(key), Ok(Some(value)) if value.eq_ignore_ascii_case("true"))
    }

    fn start_arcface_initialization(&self) {
        let arcface_clone = self.arcface.clone();
        let models_dir_clone = self.models_dir.clone();

        log::info!("Spawning the ArcFace initialization thread");
        std::thread::spawn(move || {
            log::debug!("ArcFace background thread started");

            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();

            let dl_result = rt.block_on(async {
                let models_dir_path = models_dir_clone.as_path();
                log::debug!("Checking for the ArcFace model in {:?}", models_dir_path);
                crate::ai::download_arcface_model(models_dir_path, |_, current, total| {
                    if current == 0 {
                        log::info!("Downloading the ArcFace model");
                    }
                    if current % (10 * 1024 * 1024) == 0 {
                        log::debug!("Downloading ArcFace: {}/{} bytes", current, total);
                    }
                })
                .await
            });

            if let Err(e) = dl_result {
                log::error!("Failed to download the ArcFace model: {}", e);
            } else {
                log::info!("ArcFace model downloaded");
            }

            log::debug!("Loading the ArcFace model");
            match ArcFace::new(&models_dir_clone) {
                Ok(model) => {
                    log::info!("ArcFace model loaded");
                    *arcface_clone.blocking_lock() = Some(model);
                }
                Err(e) => {
                    log::warn!(
                        "ArcFace model failed to load: {}. Face clustering will be skipped.",
                        e
                    );
                }
            }
        });
    }

    pub async fn run(&self, cancel: CancellationToken) {
        log::info!("AI worker started");

        let mut tags_model_ready = false;
        let mut arcface_init_started = false;
        let mut pending_tag_requeue = false;
        let mut last_tags_model_attempt: Option<Instant> = None;
        let mut last_face_enabled = false;
        let mut last_tags_enabled = false;

        log::debug!("AI worker entering its main loop");
        loop {
            if cancel.is_cancelled() {
                log::info!("AI Worker received shutdown signal");
                break;
            }

            let face_enabled = self.config_enabled("ai_face_enabled");
            let tags_enabled = self.config_enabled("ai_tags_enabled");

            let face_just_enabled = face_enabled && !last_face_enabled;
            let tags_just_enabled = tags_enabled && !last_tags_enabled;

            if face_just_enabled {
                match self.db.queue_pending_face_scans() {
                    Ok(count) => {
                        if count > 0 {
                            log::info!("Requeued {} image(s) for pending face scan", count);
                        }
                    }
                    Err(e) => log::warn!("Failed to requeue pending face scans: {}", e),
                }
            }

            if face_enabled && !arcface_init_started {
                if self.detector.is_some() {
                    self.start_arcface_initialization();
                    arcface_init_started = true;
                } else {
                    log::warn!(
                        "Face detection enabled in settings, but detector is unavailable on this device"
                    );
                }
            }

            if tags_just_enabled {
                pending_tag_requeue = true;
            }

            if tags_enabled && !tags_model_ready {
                let should_attempt_download = last_tags_model_attempt
                    .map(|last| last.elapsed() >= StdDuration::from_secs(30))
                    .unwrap_or(true);

                if should_attempt_download {
                    last_tags_model_attempt = Some(Instant::now());

                    tags_model_ready =
                        object_detection::ensure_model_loaded(&self.models_dir).is_ok();

                    if !tags_model_ready {
                        log::info!(
                            "Object detection model missing; downloading because AI tags are enabled"
                        );
                        match object_detection::download_model(
                            &self.models_dir,
                            |_file, _current, _total| {},
                        )
                        .await
                        {
                            Ok(_) => {
                                tags_model_ready =
                                    object_detection::ensure_model_loaded(&self.models_dir).is_ok();
                            }
                            Err(e) => {
                                log::warn!("Object detection model download failed: {}", e);
                            }
                        }
                    }
                }
            }

            if tags_model_ready && pending_tag_requeue {
                match self.db.queue_pending_tag_scans() {
                    Ok(count) => {
                        if count > 0 {
                            log::info!("Requeued {} image(s) for pending tag scan", count);
                        }
                    }
                    Err(e) => log::warn!("Failed to requeue pending tag scans: {}", e),
                }
                pending_tag_requeue = false;
            }

            last_face_enabled = face_enabled;
            last_tags_enabled = tags_enabled;

            if !face_enabled && !tags_enabled {
                sleep(Duration::from_secs(2)).await;
                continue;
            }

            let item_opt = match self.db.get_next_item_to_scan() {
                Ok(opt) => opt,
                Err(e) => {
                    log::error!("Error fetching next item to scan: {}", e);
                    None
                }
            };

            if let Some(item) = item_opt {
                // Logged by row id: the file path is the user's own naming of people,
                // places and events, and this line runs for every item in the library.
                log::debug!("AI processing media {}", item.id);

                let path = std::path::PathBuf::from(&item.file_path);
                if !path.exists() {
                    log::warn!("File missing for AI scan, media {}", item.id);
                    let _ = self.db.mark_media_scan_failed(item.id);
                    continue;
                }

                let is_image = item
                    .mime_type
                    .as_deref()
                    .map(|m| m.starts_with("image/"))
                    .unwrap_or(false);

                if !is_image {
                    log::debug!("Skipping non-image media {}", item.id);
                    let _ = self.db.mark_media_scanned(item.id);
                    continue;
                }

                if face_enabled {
                    if let Some(detector) = &self.detector {
                        let detector = detector.clone();
                        let path_clone = path.clone();

                        let result = tokio::task::spawn_blocking(move || {
                            let detector = detector.blocking_lock();
                            detector.detect(&path_clone)
                        })
                        .await;

                        match result {
                            Ok(detect_res) => match detect_res {
                                Ok(faces) => {
                                    log::debug!("Found {} faces in media {}", faces.len(), item.id);
                                    if let Err(e) = self.db.add_faces(item.id, &faces) {
                                        log::error!("Failed to save faces: {}", e);
                                    }

                                    match image::open(&item.file_path) {
                                        Ok(img) => {
                                            if let Ok(db_faces) =
                                                self.db.get_all_faces_for_media(item.id)
                                            {
                                                let arcface_clone = self.arcface.clone();
                                                let img_clone = img.clone();
                                                let db_clone = self.db.clone();
                                                let db_faces_clone = db_faces.clone();

                                                let _ = tokio::task::spawn_blocking(move || {
                                                    let arcface_guard = arcface_clone.blocking_lock();
                                                    if let Some(arcface) = arcface_guard.as_ref() {
                                                        for (face_id, face_data) in db_faces_clone {
                                                            let (w, h) = img_clone.dimensions();
                                                            let x = face_data.x.max(0.0) as u32;
                                                            let y = face_data.y.max(0.0) as u32;
                                                            let width = face_data
                                                                .width
                                                                .min(w as f32 - x as f32)
                                                                as u32;
                                                            let height = face_data
                                                                .height
                                                                .min(h as f32 - y as f32)
                                                                as u32;

                                                            if width > 10 && height > 10 {
                                                                let crop = img_clone
                                                                    .crop_imm(x, y, width, height)
                                                                    .to_rgb8();
                                                                match arcface.get_embedding(
                                                                    &image::DynamicImage::ImageRgb8(
                                                                        crop,
                                                                    ),
                                                                ) {
                                                                    Ok(embedding) => {
                                                                        if let Err(e) = db_clone
                                                                            .store_face_embedding(
                                                                                face_id, &embedding,
                                                                            )
                                                                        {
                                                                            log::error!(
                                                                                "Failed to store/cluster face {}: {}",
                                                                                face_id,
                                                                                e
                                                                            );
                                                                        }
                                                                    }
                                                                    Err(e) => log::warn!(
                                                                        "Failed to embed face {}: {}",
                                                                        face_id,
                                                                        e
                                                                    ),
                                                                }
                                                            }
                                                        }
                                                    }
                                                })
                                                .await;
                                            }
                                        }
                                        Err(e) => {
                                            log::warn!(
                                                "Failed to reopen image for embedding: {}",
                                                e
                                            )
                                        }
                                    }
                                }
                                Err(e) => {
                                    log::error!(
                                        "Face detection failed for {}: {}",
                                        item.file_path,
                                        e
                                    );
                                    let _ = self.db.mark_media_scan_failed(item.id);
                                }
                            },
                            Err(e) => {
                                log::error!("Join error in AI worker: {}", e);
                                let _ = self.db.mark_media_scan_failed(item.id);
                            }
                        }
                    }
                }

                if tags_enabled && tags_model_ready {
                    let path_for_tags = path.clone();
                    let models_dir = self.models_dir.clone();

                    let tag_result = tokio::task::spawn_blocking(move || {
                        if object_detection::model_available(&models_dir) {
                            object_detection::classify_image(&path_for_tags, 5)
                        } else {
                            Err("Object detection model not available".to_string())
                        }
                    })
                    .await;

                    match tag_result {
                        Ok(Ok(tags)) => {
                            if !tags.is_empty() {
                                log::info!(
                                    "Found {} tags in {}: {:?}",
                                    tags.len(),
                                    item.file_path,
                                    tags.iter().map(|(t, _)| t).collect::<Vec<_>>()
                                );
                            }
                            let tags_f64: Vec<(String, f64)> =
                                tags.into_iter().map(|(t, c)| (t, c as f64)).collect();
                            if let Err(e) = self.db.add_tags(item.id, &tags_f64) {
                                log::error!("Failed to save tags to DB: {}", e);
                            }
                        }
                        Ok(Err(e)) => {
                            log::debug!("Object detection skipped for {}: {}", item.file_path, e);
                        }
                        Err(e) => {
                            log::error!("Join error in object detection: {}", e);
                        }
                    }
                }

                if let Err(e) = self.db.mark_media_scanned(item.id) {
                    log::error!("Failed to mark item {} as scanned: {}", item.id, e);
                }

                sleep(Duration::from_millis(100)).await;
            } else {
                sleep(Duration::from_secs(5)).await;
            }
        }
    }
}
