"""Check actual model resolution without starting a turn or contacting a provider."""

import json
import os
import re
import sqlite3
import subprocess
import tempfile
import threading
from pathlib import Path

CONTROL = "release-contract/unknown-metadata-control"


def rpc(process, request_id, method, params):
    message = {"id": request_id, "method": method, "params": params}
    process.stdin.write(json.dumps(message) + "\n")
    process.stdin.flush()
    for line in process.stdout:
        reply = json.loads(line)
        if reply.get("id") != request_id:
            continue
        if "error" in reply:
            raise ValueError(f"{method} failed: {reply['error']}")
        return reply["result"]
    raise RuntimeError(f"app-server exited during {method}")


def fallback_warnings(home):
    text = (home / "stderr").read_text()
    for db in home.glob("logs*.sqlite"):
        with sqlite3.connect(db) as conn:
            columns = {row[1] for row in conn.execute("pragma table_info(logs)")}
            for col in ("feedback_log_body", "message"):
                if col in columns:
                    rows = conn.execute(
                        f"select {col} from logs where {col} like '%fallback model metadata%'"
                    )
                    text += "\n" + "\n".join(row[0] for row in rows)
    return sorted(set(re.findall(r"Unknown model (\S+) is used", text)))


def verify_threads(binary, catalog, routes):
    args = [
        str(binary),
        "-c",
        "model_catalog_json=" + json.dumps(str(catalog.resolve())),
        "-c",
        'model_provider="releaseprobe"',
        "-c",
        'model_providers.releaseprobe={name="releaseprobe",base_url="http://127.0.0.1:9/v1",wire_api="responses"}',
        "app-server",
    ]
    with tempfile.TemporaryDirectory() as working_dir:
        home = Path(working_dir)
        env = {
            **os.environ,
            "CODEX_HOME": working_dir,
            "RUST_LOG": "codex_models_manager=warn",
        }
        with (
            (home / "stderr").open("wb") as log,
            subprocess.Popen(
                args,
                cwd=working_dir,
                env=env,
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=log,
                text=True,
            ) as process,
        ):
            timer = threading.Timer(60, process.kill)
            timer.start()
            try:
                rpc(
                    process,
                    1,
                    "initialize",
                    {
                        "clientInfo": {"name": "catalog-release", "version": "1"},
                        "capabilities": {"experimentalApi": True},
                    },
                )
                for index, route in enumerate([*routes, CONTROL]):
                    reply = rpc(
                        process,
                        2 * index + 2,
                        "thread/start",
                        {
                            "model": route,
                            "cwd": working_dir,
                            "ephemeral": True,
                            "approvalPolicy": "never",
                            "sandbox": "read-only",
                        },
                    )
                    if reply["model"] != route:
                        raise ValueError(
                            f"{route} silently changed to {reply['model']}"
                        )
                    rpc(
                        process,
                        2 * index + 3,
                        "thread/unsubscribe",
                        {"threadId": reply["thread"]["id"]},
                    )
            finally:
                process.stdin.close()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
                timer.cancel()
        warnings = fallback_warnings(home)
        if warnings != [CONTROL]:
            raise ValueError(
                f"fallback detector expected only {CONTROL}, observed {warnings}"
            )
        return {
            "started": len(routes),
            "fallbackRoutes": [],
            "negativeControl": CONTROL,
        }
