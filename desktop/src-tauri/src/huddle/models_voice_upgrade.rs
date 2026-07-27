use super::*;

pub(super) const POCKET_MARIUS_WAV: &[u8] =
    include_bytes!("../../resources/pocket-voices/marius.wav");
const PRE_MARIUS_TTS_MODEL_VERSION: &str = "3";

/// Add Marius to an otherwise-ready v3 install without re-downloading models.
///
/// The manifest is written last, so interruption leaves v3 intact and the next
/// launch retries.
pub(super) fn install_marius_into_v3_model(models_dir: &Path) -> Result<(), String> {
    let model_dir = models_dir.join(TTS_MODEL_DIR_NAME);
    let manifest_path = model_dir.join(MANIFEST_FILENAME);
    let version = match std::fs::read_to_string(&manifest_path) {
        Ok(version) => version,
        Err(_) => return Ok(()),
    };
    if version.trim() != PRE_MARIUS_TTS_MODEL_VERSION {
        return Ok(());
    }
    if !TTS_EXPECTED_FILES
        .iter()
        .filter(|filename| **filename != "marius.wav")
        .all(|filename| model_dir.join(filename).is_file())
    {
        return Ok(());
    }

    std::fs::write(model_dir.join("marius.wav"), POCKET_MARIUS_WAV)
        .map_err(|error| format!("write Marius voice: {error}"))?;
    std::fs::write(model_dir.join(TTS_LICENSE_FILE_NAME), TTS_LICENSE_TEXT)
        .map_err(|error| format!("update Pocket voice notice: {error}"))?;
    std::fs::write(manifest_path, TTS_MODEL_VERSION)
        .map_err(|error| format!("update Pocket model manifest: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v3_install_adds_marius_without_redownloading_models() {
        let temp = tempfile::tempdir().expect("tempdir");
        let model_dir = temp.path().join(TTS_MODEL_DIR_NAME);
        std::fs::create_dir_all(&model_dir).expect("create model dir");
        for file in TTS_EXPECTED_FILES
            .iter()
            .filter(|filename| **filename != "marius.wav")
        {
            std::fs::write(model_dir.join(file), b"existing").expect("write prior file");
        }
        std::fs::write(
            model_dir.join(MANIFEST_FILENAME),
            PRE_MARIUS_TTS_MODEL_VERSION,
        )
        .expect("write prior manifest");

        install_marius_into_v3_model(temp.path()).expect("in-place upgrade");

        assert_eq!(
            std::fs::read(model_dir.join("marius.wav")).expect("Marius installed"),
            POCKET_MARIUS_WAV
        );
        assert_eq!(
            std::fs::read_to_string(model_dir.join(MANIFEST_FILENAME)).expect("updated manifest"),
            TTS_MODEL_VERSION
        );
        assert!(
            ModelSlot::new(TTS_MODEL_DIR_NAME, TTS_EXPECTED_FILES, TTS_MODEL_VERSION)
                .is_ready(temp.path())
        );
    }
}
