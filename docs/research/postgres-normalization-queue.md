# PostgreSQL normalization queue: bounded work without page cursors

Scope: one `raw_posts` row per Telegram message, with immutable `payload`, `message_id` PK and nullable `processed_at`; the ingester commits historical messages in descending ID order while polling new messages in ascending ID order. The normalizer is initially one process. **Design inference:** `processed_at IS NULL` is the durable work predicate; no normalization page number, OFFSET, or monotonic message-ID cursor is required. Repeatedly take the first **N currently pending** rows in a deterministic `message_id` order. That order bounds and prioritizes work; it does *not* imply arrival order. PostgreSQL defines `LIMIT` as a maximum returned count and `OFFSET` as skipping qualifying rows; an `ORDER BY` with a unique key determines which rows are selected. [PostgreSQL SELECT: LIMIT/OFFSET](https://www.postgresql.org/docs/18/sql-select.html#SQL-LIMIT).

```sql
-- Sketch: one transaction per small batch; default READ COMMITTED isolation.
BEGIN;
SELECT message_id, payload
FROM raw_posts
WHERE processed_at IS NULL
ORDER BY message_id
LIMIT $1
FOR UPDATE;
-- For each distinct grouped_id in selected payloads, read *all* stored
-- raw_posts with that ID, including processed rows. Use the actual
-- serializer's JSON shape when extracting grouped_id; do not assume
-- the field is at the payload root.
-- Reconcile the affected posts/articles (including deletion or relinking
-- when a caption becomes ambiguous), then mark ONLY selected message IDs:
UPDATE raw_posts SET processed_at = clock_timestamp()
WHERE message_id = ANY($1::bigint[]) AND processed_at IS NULL;
COMMIT;
-- No selected rows: end this pass, but poll again after new ingester commits.
```

Each statement binds its own `$1`: positive batch size, one group's ID, then the captured array of *selected* message IDs. **Design inference:** never mark every row matching `grouped_id` merely because it matches at `UPDATE` time: an insert committed after the group read could then become processed without its caption being counted. Keep the fetch, all derived writes and selected-row marks in the *same* transaction; on error roll back the whole transaction. `ROLLBACK` discards its updates, and transaction-end releases row locks, so selected IDs remain eligible for a later attempt. [ROLLBACK](https://www.postgresql.org/docs/18/sql-rollback.html); [row locks](https://www.postgresql.org/docs/18/explicit-locking.html#LOCKING-ROWS).

**Design inference from PostgreSQL snapshot semantics:** each `READ COMMITTED` statement sees committed data as of its start, while the next statement can see subsequent commits. A historical row inserted *below* the last normalized ID or a live row inserted *above* it remains pending and is found on a future selection regardless of ID. A row committed during the current SELECT may be invisible to that selection, but is eligible on the next one. Do not interpret a single empty selection as permanent completion. With one normalizer and a PK on `message_id`, the pending predicate plus atomic derived writes/marking avoids losing committed raw rows and avoids repeating a **committed** normalization of the same row; an aborted attempt may run again, and group-derived output can intentionally be revised when new members arrive. This does not assert exactly-once execution or coverage of messages never ingested. [READ COMMITTED](https://www.postgresql.org/docs/18/transaction-iso.html#XACT-READ-COMMITTED); [ROLLBACK](https://www.postgresql.org/docs/18/sql-rollback.html).

**Design inference:** this is a *safety* argument, not a deadline or fairness guarantee. If lower IDs keep arriving faster than the normalizer can drain them, `ORDER BY message_id ASC LIMIT N` can indefinitely delay higher pending IDs; adjust scheduling/capacity if this workload occurs. In the finite-backlog case, repeated successful batches eventually drain pending rows.

**Why not OFFSET?** Suppose pending IDs 1–20 and page size 10: after processing 1–10, the pending set starts at 11; `OFFSET 10 LIMIT 10` skips 11–20. Sorting does not fix the shifting filter. Conversely, a new historical insert can change offsets mid-scan. `OFFSET` also skips rows before returning the batch and with `FOR UPDATE` those skipped rows are locked too. `LIMIT N` with **no OFFSET** always rechecks the durable predicate. [PostgreSQL SELECT: LIMIT/OFFSET and locking](https://www.postgresql.org/docs/18/sql-select.html#SQL-FOR-UPDATE-SHARE).

**Albums:** [Telegram defines photo-album shared-caption display only when exactly one photo has a caption](https://core.telegram.org/api/files#albums-grouped-media). **Design inference:** when any pending `grouped_id` member is selected, reread *every currently committed* raw member with that ID, including already processed ones; recompute the caption count and reconcile the one post and its affected article(s) transactionally. A newly inserted group member stays pending and retriggers this even if every older member has `processed_at`. One caption may provisionally yield a post; a later second caption must retract it. Neither PostgreSQL row locking nor `processed_at` proves an album is source-complete: a later Telegram page, an uncommitted ingester transaction, or an unavailable source member may still change the count. Permanent completeness requires an independently justified source-group completion signal/barrier; the proposed workflow only guarantees convergence relative to **currently saved** members (assuming every new member is eventually ingested and normalized). [READ COMMITTED snapshots](https://www.postgresql.org/docs/18/transaction-iso.html#XACT-READ-COMMITTED).

**If multiple normalizers are introduced:** keep `FOR UPDATE` across the full processing transaction and optionally add `SKIP LOCKED` to the selection, so workers can claim other pending rows instead of waiting. PostgreSQL expressly describes `SKIP LOCKED` as suitable for queue-like multiple consumers, while warning that it gives an inconsistent view; an empty result can mean all visible candidates are locked, not drained. Locks held through commit/rollback prevent two workers from committing processing of the *same raw row* simultaneously. **Design inference:** row claims alone do not serialize two *different* rows from the same album, nor separate rows that alter the same article; coordinate affected logical groups/articles (e.g. a transaction-scoped group lock or one normalizer), and order multi-object lock acquisition consistently to avoid deadlocks. `SKIP LOCKED` is optional, unnecessary for the planned single worker, and does not solve group completeness. [PostgreSQL SELECT: locking clause](https://www.postgresql.org/docs/18/sql-select.html#SQL-FOR-UPDATE-SHARE); [explicit locking and deadlocks](https://www.postgresql.org/docs/18/explicit-locking.html#LOCKING-DEADLOCKS).

Optional performance choice after measuring backlog: `CREATE INDEX raw_posts_pending_id ON raw_posts (message_id) WHERE processed_at IS NULL;` targets pending scans without indexing finished rows. This is not required for queue correctness. [PostgreSQL partial indexes](https://www.postgresql.org/docs/18/indexes-partial.html).
