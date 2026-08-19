from __future__ import annotations

import sqlite3

from agentdictate.replacements import ReplacementMapping


class MappingStoreMixin:
    _lock: object
    conn: sqlite3.Connection

    def _validated_source_phrase(self, mapping: ReplacementMapping) -> str:
        source_phrase = mapping.source_phrase.strip()
        if not source_phrase:
            raise ValueError("source_phrase is required for replacement mappings")
        return source_phrase

    def list_mappings(self, search: str = "") -> list[ReplacementMapping]:
        with self._lock:
            if search:
                rows = self.conn.execute(
                    """
                    SELECT * FROM replacement_mappings
                    WHERE source_phrase LIKE ? OR replacement_phrase LIKE ?
                    ORDER BY source_phrase COLLATE NOCASE
                    """,
                    (f"%{search}%", f"%{search}%"),
                )
            else:
                rows = self.conn.execute(
                    "SELECT * FROM replacement_mappings ORDER BY source_phrase COLLATE NOCASE"
                )
            return [
                ReplacementMapping(
                    id=int(row["id"]),
                    source_phrase=str(row["source_phrase"]),
                    replacement_phrase=str(row["replacement_phrase"]),
                    enabled=bool(row["enabled"]),
                    case_sensitive=bool(row["case_sensitive"]),
                    whole_word_only=bool(row["whole_word_only"]),
                    created_at=str(row["created_at"]),
                    updated_at=str(row["updated_at"]),
                )
                for row in rows
            ]

    def add_mapping(self, mapping: ReplacementMapping) -> int:
        source_phrase = self._validated_source_phrase(mapping)
        now = ReplacementMapping.now_iso()
        created_at = mapping.created_at or now
        updated_at = mapping.updated_at or now
        with self._lock:
            with self.conn:
                cursor = self.conn.execute(
                    """
                    INSERT INTO replacement_mappings (
                        source_phrase, replacement_phrase, enabled, case_sensitive,
                        whole_word_only, created_at, updated_at
                    )
                    VALUES (?, ?, ?, ?, ?, ?, ?)
                    """,
                    (
                        source_phrase,
                        mapping.replacement_phrase,
                        int(mapping.enabled),
                        int(mapping.case_sensitive),
                        int(mapping.whole_word_only),
                        created_at,
                        updated_at,
                    ),
                )
            return int(cursor.lastrowid)

    def update_mapping(self, mapping: ReplacementMapping) -> None:
        if mapping.id is None:
            raise ValueError("mapping.id is required for update")
        source_phrase = self._validated_source_phrase(mapping)
        with self._lock:
            with self.conn:
                self.conn.execute(
                    """
                    UPDATE replacement_mappings
                    SET source_phrase = ?, replacement_phrase = ?, enabled = ?,
                        case_sensitive = ?, whole_word_only = ?, updated_at = ?
                    WHERE id = ?
                    """,
                    (
                        source_phrase,
                        mapping.replacement_phrase,
                        int(mapping.enabled),
                        int(mapping.case_sensitive),
                        int(mapping.whole_word_only),
                        ReplacementMapping.now_iso(),
                        mapping.id,
                    ),
                )

    def delete_mapping(self, mapping_id: int) -> None:
        with self._lock:
            with self.conn:
                self.conn.execute("DELETE FROM replacement_mappings WHERE id = ?", (mapping_id,))
