use super::*;

#[test]
fn startup_waits_for_delayed_first_frame_and_preserves_it() {
    let (tx, rx) = mpsc::channel();
    let state = Arc::new(AtomicU8::new(AudioState::Starting as u8));
    let worker_state = state.clone();
    let worker = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(30));
        worker_state.store(AudioState::Listening as u8, Ordering::SeqCst);
        tx.send(Arc::<[f32]>::from(vec![0.25; 1280])).unwrap();
    });
    let first = wait_for_first_frame(
        || rx.recv_timeout(Duration::from_millis(5)).ok(),
        || if state.load(Ordering::SeqCst) == AudioState::Starting as u8 { AudioState::Starting } else { AudioState::Listening },
        || false,
        Duration::from_secs(1),
    ).unwrap();
    worker.join().unwrap();
    assert_eq!(first.len(), 1280);
    assert_eq!(first[0], 0.25);
}

#[test]
fn startup_timeout_and_denied_state_never_claim_recording() {
    let result = wait_for_first_frame(
        || { std::thread::sleep(Duration::from_millis(2)); None },
        || AudioState::Starting,
        || false,
        Duration::from_millis(8),
    );
    assert!(result.unwrap_err().contains("не начал передавать"));
    let result = wait_for_first_frame(
        || panic!("denied startup must not wait for audio"),
        || AudioState::Denied,
        || false,
        Duration::from_secs(1),
    );
    assert!(result.unwrap_err().contains("Нет доступа"));
}

#[test]
fn cancelling_during_first_receive_does_not_report_ready() {
    let cancelled = std::cell::Cell::new(false);
    let result = wait_for_first_frame(
        || { cancelled.set(true); Some(Arc::<[f32]>::from(vec![0.25; 1280])) },
        || AudioState::Listening,
        || cancelled.get(),
        Duration::from_secs(1),
    );
    assert!(result.unwrap_err().contains("отменён"));
}

pub(super) struct TestDir(pub(super) PathBuf);
impl TestDir {
    pub(super) fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "jarvis-meetings-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        create_private_dir(&dir).unwrap();
        Self(dir)
    }
}
impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn fixture(id: &str, audio_path: &Path, status: &str) -> Meeting {
    Meeting {
        id: id.into(),
        title: "Еженедельная встреча".into(),
        started_at: 1000,
        ended_at: None,
        duration_ms: 0,
        status: status.into(),
        source: "microphone".into(),
        transcript: "Уже сохранено".into(),
        segments: vec![],
        audio_path: audio_path.to_string_lossy().into_owned(),
        error: None,
        warning: None,
    }
}

#[test]
fn long_recording_streams_to_wav_and_transcribes_without_the_dictation_buffer_cap() {
    let dir = TestDir::new();
    let path = dir.0.join("long.wav");
    let mut writer = AudioWriter::create(&path).unwrap();
    // Eleven minutes exceed AudioHub's 5.5 minute subscriber queue. The writer
    // drains every frame; only a frame and <=30 seconds of ASR audio live in RAM.
    let frame = vec![0.1; DST_RATE as usize];
    for second in 0..660 {
        writer.push(&frame).unwrap();
        if second % 60 == 0 {
            writer.flush().unwrap();
        }
    }
    assert_eq!(writer.duration_ms(), 660_000);
    writer.finish().unwrap();
    let mut total = 0usize;
    let mut chunks = 0;
    let mut segments = Vec::new();
    transcribe_file(
        &path,
        |pcm| {
            assert!(pcm.len() <= CHUNK_SAMPLES);
            total += pcm.len();
            chunks += 1;
            Ok(format!("Фрагмент {chunks}"))
        },
        |segment| {
            segments.push(segment);
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(total, 660 * DST_RATE as usize);
    assert_eq!(chunks, 22);
    assert_eq!(segments.first().unwrap().start_ms, 0);
    assert_eq!(segments.last().unwrap().end_ms, 660_000);
    assert!(segments.windows(2).all(|w| w[0].end_ms == w[1].start_ms));
}

#[test]
fn quiet_chunk_boundary_preserves_every_sample_and_final_partial_chunk() {
    let dir = TestDir::new();
    let path = dir.0.join("boundary.wav");
    let mut pcm = vec![0.15; CHUNK_SAMPLES + DST_RATE as usize / 2];
    pcm[DST_RATE as usize * 28..DST_RATE as usize * 29].fill(0.0);
    let mut writer = AudioWriter::create(&path).unwrap();
    writer.push(&pcm).unwrap();
    writer.finish().unwrap();
    let mut lengths = Vec::new();
    let mut segments = Vec::new();
    transcribe_file(
        &path,
        |chunk| {
            lengths.push(chunk.len());
            Ok("Речь".into())
        },
        |segment| {
            segments.push(segment);
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(lengths.iter().sum::<usize>(), pcm.len());
    assert!(
        lengths[0] < CHUNK_SAMPLES,
        "split should choose the quiet interval"
    );
    assert_eq!(segments.last().unwrap().end_ms, 30_500);
    assert_eq!(segments[0].end_ms, segments[1].start_ms);
}

#[test]
fn digital_silence_never_calls_asr_and_keeps_audio() {
    let dir = TestDir::new();
    let path = dir.0.join("silence.wav");
    let mut writer = AudioWriter::create(&path).unwrap();
    writer.push(&vec![0.0; DST_RATE as usize * 31]).unwrap();
    writer.finish().unwrap();
    transcribe_file(
        &path,
        |_| panic!("silence reached ASR"),
        |_| panic!("silence produced text"),
    )
    .unwrap();
    assert!(path.is_file());
}

#[test]
fn failed_chunk_preserves_audio_and_already_emitted_transcript() {
    let dir = TestDir::new();
    let path = dir.0.join("retry.wav");
    let mut writer = AudioWriter::create(&path).unwrap();
    writer.push(&vec![0.2; CHUNK_SAMPLES * 2]).unwrap();
    writer.finish().unwrap();
    let mut calls = 0;
    let mut saved = Vec::new();
    let result = transcribe_file(
        &path,
        |_| {
            calls += 1;
            if calls == 2 {
                Err("model unavailable".into())
            } else {
                Ok("Первый фрагмент".into())
            }
        },
        |segment| {
            saved.push(segment);
            Ok(())
        },
    );
    assert_eq!(result.unwrap_err(), "model unavailable");
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].text, "Первый фрагмент");
    assert_eq!(
        hound::WavReader::open(&path).unwrap().duration(),
        (CHUNK_SAMPLES * 2) as u32
    );
}

#[test]
fn restart_recovers_recording_as_interrupted_using_flushed_wav_duration() {
    let dir = TestDir::new();
    let id = "1000-1-0";
    let meeting_dir = dir.0.join(id);
    create_private_dir(&meeting_dir).unwrap();
    let path = meeting_dir.join("audio.wav");
    let mut writer = AudioWriter::create(&path).unwrap();
    writer.push(&vec![0.1; DST_RATE as usize * 3]).unwrap();
    writer.flush().unwrap(); // crash-like reload before finalize
    let record = fixture(id, &path, "recording");
    crate::stt::transcripts::write_private_atomic(
        &meeting_dir.join("meeting.json"),
        &serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();
    let service = Meetings::load_from(dir.0.clone());
    let restored = service.get(id).unwrap();
    assert_eq!(restored.status, "interrupted");
    assert_eq!(restored.duration_ms, 3000);
    assert_eq!(restored.ended_at, Some(4000));
    assert_eq!(restored.transcript, "Уже сохранено");
    assert!(restored.error.unwrap().contains("повторно"));
    assert!(!service.is_recording());
    assert!(service.status().is_none());
    writer.finish().unwrap();
}

#[test]
fn online_meeting_crash_before_mix_recovers_both_saved_tracks() {
    let dir = TestDir::new();
    let id = "2000-1-0";
    let meeting_dir = dir.0.join(id);
    create_private_dir(&meeting_dir).unwrap();
    for (name, seconds) in [("microphone.wav", 3), ("system.wav", 5)] {
        let mut writer = AudioWriter::create(&meeting_dir.join(name)).unwrap();
        writer
            .push(&vec![0.1; DST_RATE as usize * seconds])
            .unwrap();
        writer.finish().unwrap();
    }
    let mut record = fixture(id, &meeting_dir.join("audio.wav"), "recording");
    record.source = "microphone-system".into();
    crate::stt::transcripts::write_private_atomic(
        &meeting_dir.join("meeting.json"),
        &serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();
    assert!(!meeting_dir.join("audio.wav").exists());
    let service = Meetings::load_from(dir.0.clone());
    let restored = service.get(id).unwrap();
    assert_eq!(restored.status, "interrupted");
    assert_eq!(restored.duration_ms, 5000);
    assert_eq!(super::system_audio::mix_tracks(&meeting_dir).unwrap(), 5000);
    assert_eq!(
        hound::WavReader::open(meeting_dir.join("audio.wav"))
            .unwrap()
            .duration(),
        DST_RATE * 5
    );
}

#[test]
fn archive_does_not_trust_external_audio_paths_or_mismatched_ids() {
    let dir = TestDir::new();
    let meeting_dir = dir.0.join("valid-id");
    create_private_dir(&meeting_dir).unwrap();
    let record = fixture("valid-id", Path::new("/external/private.wav"), "ready");
    crate::stt::transcripts::write_private_atomic(
        &meeting_dir.join("meeting.json"),
        &serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();
    let service = Meetings::load_from(dir.0.clone());
    assert_eq!(
        service.get("valid-id").unwrap().audio_path,
        meeting_dir.join("audio.wav").to_string_lossy()
    );
    assert!(service.get("../valid-id").is_err());
    assert!(!valid_id("../valid-id"));
    assert!(!valid_id("/etc/passwd"));
}

#[test]
fn meeting_capture_and_transcription_reservations_reject_overlapping_starts() {
    let mut state = State::default();
    assert!(ensure_idle(&state).is_ok());
    state.active = Some(ActiveRecording {
        id: "meeting".into(),
        stop: Arc::new(AtomicU8::new(0)),
        finished: Arc::new((Mutex::new(false), Condvar::new())),
    });
    assert!(ensure_idle(&state).unwrap_err().contains("остановите"));
    state.active = None;
    state.processing.insert("meeting".into());
    assert!(ensure_idle(&state).unwrap_err().contains("расшифровки"));
}

#[test]
fn finished_status_is_not_exposed_as_active_during_worker_cleanup() {
    let dir = TestDir::new();
    let service = Meetings::load_from(dir.0.clone());
    let record = fixture("done", &dir.0.join("audio.wav"), "ready");
    {
        let mut state = service.state.lock().unwrap();
        state.items.insert(record.id.clone(), record);
        state.processing.insert("done".into());
    }
    // The completion event can trigger a UI pull before the worker removes its
    // reservation. A ready result must never leave the start button disabled.
    assert!(service.status().is_none());
}

#[cfg(unix)]
#[test]
fn local_recordings_and_metadata_are_private() {
    use std::os::unix::fs::PermissionsExt;
    let dir = TestDir::new();
    let path = dir.0.join("private.wav");
    let mut writer = AudioWriter::create(&path).unwrap();
    writer.push(&[f32::NAN, f32::INFINITY, 2.0, -2.0]).unwrap();
    writer.finish().unwrap();
    assert_eq!(
        std::fs::metadata(&dir.0).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let metadata = dir.0.join("meeting.json");
    crate::stt::transcripts::write_private_atomic(&metadata, b"{}").unwrap();
    assert_eq!(
        std::fs::metadata(&metadata).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let pcm: Vec<i16> = hound::WavReader::open(&path)
        .unwrap()
        .samples()
        .map(Result::unwrap)
        .collect();
    assert_eq!(pcm, [0, 0, i16::MAX, -i16::MAX]);
}

#[test]
fn title_validation_handles_unicode_and_empty_names() {
    assert_eq!(
        clean_title(Some("  Планёрка  ".into())).unwrap(),
        "Планёрка"
    );
    assert_eq!(clean_title(None).unwrap(), "Новая встреча");
    assert!(clean_title(Some("я".repeat(200))).is_ok());
    assert!(clean_title(Some("я".repeat(201))).is_err());
    assert!(clean_title(Some("Встреча\nКоманда".into())).is_err());
}
