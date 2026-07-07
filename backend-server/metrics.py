"""Usage metrics for the tokopt backend server — one row per request, plus a
summary aggregation for GET /usage. SQLite, WAL mode for concurrent readers/
writers from the ThreadingHTTPServer's worker threads.
"""
from __future__ import annotations
import sqlite3
import threading
import time


class Metrics:
    def __init__(self, db_path: str):
        self.db_path = db_path
        self._lock = threading.Lock()
        conn = self._connect()
        conn.execute("""
            CREATE TABLE IF NOT EXISTS requests (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                ts INTEGER NOT NULL,
                endpoint TEXT NOT NULL,
                model TEXT,
                status TEXT NOT NULL,
                latency_ms INTEGER NOT NULL,
                chars_in INTEGER NOT NULL,
                chars_out INTEGER NOT NULL
            )
        """)
        conn.execute("CREATE INDEX IF NOT EXISTS idx_requests_ts ON requests(ts)")
        conn.execute("CREATE INDEX IF NOT EXISTS idx_requests_endpoint ON requests(endpoint)")
        conn.commit()
        conn.close()

    def _connect(self) -> sqlite3.Connection:
        conn = sqlite3.connect(self.db_path, timeout=10)
        conn.execute("PRAGMA journal_mode=WAL")
        return conn

    def record(self, endpoint: str, *, model: str, status: str, latency_ms: int,
               chars_in: int, chars_out: int) -> None:
        """Fail-silent — a metrics write must never break a request."""
        try:
            with self._lock:
                conn = self._connect()
                conn.execute(
                    "INSERT INTO requests (ts, endpoint, model, status, latency_ms, chars_in, chars_out) "
                    "VALUES (?, ?, ?, ?, ?, ?, ?)",
                    (int(time.time()), endpoint, model, status, latency_ms, chars_in, chars_out),
                )
                conn.commit()
                conn.close()
        except Exception:
            pass

    def summary(self) -> dict:
        now = int(time.time())
        conn = self._connect()
        try:
            def window(seconds: int) -> dict:
                cutoff = now - seconds
                rows = conn.execute(
                    "SELECT endpoint, status, COUNT(*), AVG(latency_ms), SUM(chars_in), SUM(chars_out) "
                    "FROM requests WHERE ts > ? GROUP BY endpoint, status",
                    (cutoff,),
                ).fetchall()
                by_endpoint: dict = {}
                for endpoint, status, count, avg_latency, sum_in, sum_out in rows:
                    e = by_endpoint.setdefault(endpoint, {"total": 0, "by_status": {}})
                    e["total"] += count
                    e["by_status"][status] = count
                    if endpoint == "compress" and status == "ok":
                        e["chars_in"] = (e.get("chars_in") or 0) + (sum_in or 0)
                        e["chars_out"] = (e.get("chars_out") or 0) + (sum_out or 0)
                    e["avg_latency_ms"] = round(avg_latency or 0, 1)
                return by_endpoint

            total_requests = conn.execute("SELECT COUNT(*) FROM requests").fetchone()[0]
            return {
                "total_requests_all_time": total_requests,
                "last_1h": window(3600),
                "last_24h": window(86400),
                "last_7d": window(7 * 86400),
            }
        finally:
            conn.close()
