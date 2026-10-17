-- Identity belongs to the native conversation, independently of transcript/settings.
ALTER TABLE react_session ADD COLUMN claude_session TEXT;
CREATE TABLE react_claude_reconnect (
    session_id TEXT NOT NULL REFERENCES react_session(id) ON DELETE CASCADE,
    message_id TEXT NOT NULL,
    request BLOB NOT NULL,
    encoding_version INTEGER NOT NULL DEFAULT 1 CHECK (encoding_version = 1),
    PRIMARY KEY (session_id, message_id)
);
