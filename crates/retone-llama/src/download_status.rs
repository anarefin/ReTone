//! Active local-model downloads as the renderer sees them. Every change bumps a shared sequence so the
//! renderer can drop stale events.

use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadStatus {
    pub model_type: String,
    pub model_id: String,
    pub phase: String,
    pub progress: f64,
    pub downloaded_bytes: u64,
    pub total_bytes: u64,
    pub sequence: u64,
}

/// Fields a progress update may change.
#[derive(Debug, Clone, Default)]
pub struct StatusUpdate {
    pub phase: Option<String>,
    pub progress: Option<f64>,
    pub downloaded_bytes: Option<u64>,
    pub total_bytes: Option<u64>,
}

/// Insertion-ordered, like the JS `Map`.
#[derive(Debug, Default)]
pub struct LocalModelDownloadStatus {
    downloads: Vec<DownloadStatus>,
    sequence: u64,
}

impl LocalModelDownloadStatus {
    pub fn new() -> Self {
        Self::default()
    }

    fn index(&self, model_type: &str, model_id: &str) -> Option<usize> {
        self.downloads
            .iter()
            .position(|d| d.model_type == model_type && d.model_id == model_id)
    }

    /// Claims a download; an existing one is returned unchanged.
    pub fn start(&mut self, model_type: &str, model_id: &str) -> DownloadStatus {
        if let Some(i) = self.index(model_type, model_id) {
            return self.downloads[i].clone();
        }
        self.sequence += 1;
        let status = DownloadStatus {
            model_type: model_type.into(),
            model_id: model_id.into(),
            phase: "downloading".into(),
            progress: 0.0,
            downloaded_bytes: 0,
            total_bytes: 0,
            sequence: self.sequence,
        };
        self.downloads.push(status.clone());
        status
    }

    pub fn has(&self, model_type: &str, model_id: &str) -> bool {
        self.index(model_type, model_id).is_some()
    }

    pub fn update(
        &mut self,
        model_type: &str,
        model_id: &str,
        update: StatusUpdate,
    ) -> DownloadStatus {
        let i = match self.index(model_type, model_id) {
            Some(i) => i,
            None => {
                self.start(model_type, model_id);
                self.downloads.len() - 1
            }
        };
        self.sequence += 1;
        let status = &mut self.downloads[i];
        if let Some(phase) = update.phase {
            status.phase = phase;
        }
        if let Some(progress) = update.progress {
            status.progress = progress;
        }
        if let Some(bytes) = update.downloaded_bytes {
            status.downloaded_bytes = bytes;
        }
        if let Some(bytes) = update.total_bytes {
            status.total_bytes = bytes;
        }
        status.sequence = self.sequence;
        status.clone()
    }

    /// Removes a download, returning its last state with a fresh sequence.
    pub fn finish(&mut self, model_type: &str, model_id: &str) -> Option<DownloadStatus> {
        let i = self.index(model_type, model_id)?;
        let mut status = self.downloads.remove(i);
        self.sequence += 1;
        status.sequence = self.sequence;
        Some(status)
    }

    pub fn active_downloads(&self) -> Vec<DownloadStatus> {
        self.downloads.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn progress(p: f64, done: u64, total: u64) -> StatusUpdate {
        StatusUpdate {
            progress: Some(p),
            downloaded_bytes: Some(done),
            total_bytes: Some(total),
            ..StatusUpdate::default()
        }
    }

    #[test]
    fn tracks_independent_downloads_with_normalized_progress() {
        let mut status = LocalModelDownloadStatus::new();
        let whisper = status.start("whisper", "base");
        let llm = status.start("llm", "qwen-2.5-7b");
        let installing = status.update(
            "parakeet",
            "parakeet-tdt-0.6b-v3",
            StatusUpdate {
                phase: Some("installing".into()),
                progress: Some(100.0),
                ..StatusUpdate::default()
            },
        );
        let progressed = status.update("whisper", "base", progress(42.0, 42, 100));

        assert_eq!(whisper.phase, "downloading");
        assert_eq!(llm.progress, 0.0);
        assert_eq!(installing.phase, "installing");
        assert!(progressed.sequence > installing.sequence);
        assert_eq!(status.active_downloads(), vec![progressed, llm, installing]);
    }

    #[test]
    fn does_not_reset_an_existing_download_and_removes_it_only_on_finish() {
        let mut status = LocalModelDownloadStatus::new();
        status.start("llm", "qwen-2.5-7b");
        let progressed = status.update("llm", "qwen-2.5-7b", progress(55.0, 55, 100));
        let duplicate = status.start("llm", "qwen-2.5-7b");
        assert_eq!(duplicate, progressed);
        assert!(status.has("llm", "qwen-2.5-7b"));

        let finished = status.finish("llm", "qwen-2.5-7b").unwrap();
        assert_eq!(finished.progress, 55.0);
        assert!(finished.sequence > progressed.sequence);
        assert!(status.active_downloads().is_empty());
        assert!(status.finish("llm", "qwen-2.5-7b").is_none());
    }

    #[test]
    fn starts_with_zero_progress() {
        let mut status = LocalModelDownloadStatus::new();
        let started = status.start("whisper", "small");
        assert_eq!(started.model_type, "whisper");
        assert_eq!(started.phase, "downloading");
        assert_eq!(started.progress, 0.0);
        assert_eq!(started.downloaded_bytes, 0);
        assert_eq!(started.total_bytes, 0);
    }

    #[test]
    fn serializes_in_the_renderer_shape() {
        let mut status = LocalModelDownloadStatus::new();
        let value = serde_json::to_value(status.start("llm", "m")).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "modelType": "llm", "modelId": "m", "phase": "downloading",
                "progress": 0.0, "downloadedBytes": 0, "totalBytes": 0, "sequence": 1
            })
        );
    }
}
