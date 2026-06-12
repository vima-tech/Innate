use super::{
    events::{event_id_for_line, parse_log_event},
    state::init_state_db,
    watch::process_log_file,
    DAEMON_SCHEMA,
};
use tempfile::NamedTempFile;

#[test]
fn classifies_session_end_events() {
    assert_eq!(
        parse_log_event("Session ended").map(|event| event.kind),
        Some("end")
    );
    assert_eq!(
        parse_log_event(r#"{"event_type":"session_end"}"#).map(|event| event.kind),
        Some("end")
    );
}

#[test]
fn daemon_state_schema_tracks_errors() {
    let file = NamedTempFile::new().unwrap();
    init_state_db(file.path()).unwrap();
    let conn = rusqlite::Connection::open(file.path()).unwrap();
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master
                 WHERE type='table' AND name='daemon_errors'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn parses_structured_hook_payload() {
    let event = parse_log_event(
        r#"{
                "event_id":"evt-1",
                "event_type":"user_feedback",
                "trace_id":"trace-1",
                "query":"retry task",
                "output_summary":"bounded retry worked",
                "used":["chunk-1","chunk-2"],
                "feedback":"up",
                "nomination":"keep this approach",
                "priority":7
            }"#,
    )
    .unwrap();

    assert_eq!(event.kind, "feedback");
    assert_eq!(event.event_id.as_deref(), Some("evt-1"));
    assert_eq!(event.trace_id.as_deref(), Some("trace-1"));
    assert_eq!(
        event.output_summary.as_deref(),
        Some("bounded retry worked")
    );
    assert_eq!(
        event.used,
        Some(vec!["chunk-1".to_string(), "chunk-2".to_string()])
    );
    assert_eq!(event.feedback.as_deref(), Some("up"));
    assert_eq!(event.nomination.as_deref(), Some("keep this approach"));
    assert_eq!(event.priority, 7);
}

#[test]
fn generated_event_id_changes_after_log_rotation() {
    let before = event_id_for_line("/tmp/agent.log", "inode-1", 42, "Tests passed");
    let after = event_id_for_line("/tmp/agent.log", "inode-2", 42, "Tests passed");
    assert_ne!(before, after);
}

fn open_state_db(path: &std::path::Path) -> rusqlite::Connection {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch(DAEMON_SCHEMA).unwrap();
    conn
}

fn saved_offset(state_db: &rusqlite::Connection, path: &std::path::Path) -> i64 {
    state_db
        .query_row(
            "SELECT last_processed_offset FROM watch_state WHERE watch_path=?",
            rusqlite::params![path.to_string_lossy().as_ref()],
            |r| r.get(0),
        )
        .unwrap_or(0)
}

// A partial last line (no trailing newline) must not advance the saved
// offset past the last complete line.  If the writer later completes the
// line, the daemon re-reads from the correct position.
#[test]
fn partial_last_line_does_not_advance_offset() {
    let log_file = NamedTempFile::new().unwrap();
    let state_file = NamedTempFile::new().unwrap();
    let state_db = open_state_db(state_file.path());
    let mut sink = std::io::sink();

    let complete = b"not-an-event\nnot-an-event\n";
    let partial = b"incomplete";
    std::fs::write(
        log_file.path(),
        [complete.as_ref(), partial.as_ref()].concat(),
    )
    .unwrap();

    process_log_file(log_file.path(), &state_db, "/dev/null", &mut sink);

    assert_eq!(
        saved_offset(&state_db, log_file.path()),
        complete.len() as i64
    );
}

// After a partial last line is completed, the daemon re-reads from the
// saved offset and correctly processes the now-complete line.
#[test]
fn completed_partial_line_is_processed_on_next_poll() {
    let log_file = NamedTempFile::new().unwrap();
    let state_file = NamedTempFile::new().unwrap();
    let state_db = open_state_db(state_file.path());
    let mut sink = std::io::sink();

    std::fs::write(log_file.path(), b"not-an-event\nincomplete").unwrap();
    process_log_file(log_file.path(), &state_db, "/dev/null", &mut sink);
    assert_eq!(
        saved_offset(&state_db, log_file.path()),
        b"not-an-event\n".len() as i64
    );

    std::fs::write(log_file.path(), b"not-an-event\nincomplete-now-done\n").unwrap();
    process_log_file(log_file.path(), &state_db, "/dev/null", &mut sink);
    assert_eq!(
        saved_offset(&state_db, log_file.path()),
        b"not-an-event\nincomplete-now-done\n".len() as i64
    );
}
