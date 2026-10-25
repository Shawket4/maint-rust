-- ai_audit_log: one row per write that arrives through the AI advisor.
--
-- The advisor (apex-rust) never touches this database. A human with
-- permission >= 4 approves a suggestion and the advisor replays it against
-- this API with the APPROVER's JWT plus `X-Via-AI` / `X-AI-Suggestion-Id`.
-- The owner's rule: such a change records both the human and that it came
-- through the AI chat. FalconGo keeps the same trail in ai_audit_logs.
--
-- The row is written BEFORE the handler runs (status 0 = in flight) and
-- updated with the response status after, so an AI write can never apply
-- without a row: if the insert fails the request is refused with a 500.
-- A row still at status 0 means the handler ran but the status update failed
-- (or the process died mid-request) — treat it as "may have applied".

CREATE TABLE ai_audit_log (
    id              BIGSERIAL PRIMARY KEY,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- Always the verified JWT user, never anything asserted in a header.
    user_id         BIGINT NOT NULL,
    permission      INT NOT NULL,
    -- The X-Via-AI value (e.g. 'advisor').
    via             TEXT NOT NULL,
    -- X-AI-Suggestion-Id; NULL when absent or not a UUID.
    suggestion_id   UUID,
    method          TEXT NOT NULL,
    path            TEXT NOT NULL,
    status          INT NOT NULL DEFAULT 0,
    -- Request body (sensitive keys redacted); a marker object when the body
    -- is over 64 KB or not JSON; NULL when empty.
    request         JSONB,
    -- The JSON response (e.g. /sync/push per-op results) when it is JSON and
    -- at most 64 KB; NULL otherwise.
    results         JSONB
);

CREATE INDEX idx_ai_audit_created    ON ai_audit_log(created_at);
CREATE INDEX idx_ai_audit_suggestion ON ai_audit_log(suggestion_id);
