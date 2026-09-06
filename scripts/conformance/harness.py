"""Standard-library-only harness for Nebula's public Runtime API."""

from __future__ import annotations

import ctypes
from ctypes import wintypes
import fnmatch
import glob
import json
import os
import re
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import zipfile
from pathlib import Path
from typing import Any, Callable

PROTOCOL_NAME = "nebula.runtime"
PROTOCOL_VERSION = 1
MAX_RESPONSE_BYTES = 32 * 1024 * 1024
DEFAULT_STARTUP_TIMEOUT = 20.0

# The Windows gate cannot start the app until its parent assigns the exact
# process HANDLE to a job. -I -S disables user/site startup hooks in this gate.
_WINDOWS_LAUNCH_GATE = (
    "import subprocess, sys\n"
    "if sys.stdin.buffer.read(1) != b'G':\n"
    "    raise SystemExit(125)\n"
    "raise SystemExit(subprocess.call(sys.argv[1:], stdin=subprocess.DEVNULL))\n"
)


class ConformanceError(RuntimeError):
    """A test precondition or behavioral assertion failed."""


class ApiFailure(ConformanceError):
    def __init__(self, method: str, error: dict[str, Any]) -> None:
        self.method = method
        self.error = error
        code = error.get("code", "unknown_error")
        message = error.get("message", "runtime request failed")
        super().__init__(f"{method}: {code}: {message}")


class SkipCase(RuntimeError):
    """The environment cannot exercise an optional conformance case."""


def require(condition: bool, message: str) -> None:
    if not condition:
        raise ConformanceError(message)


def normalize_text(value: str) -> str:
    lines = value.replace("\r\n", "\n").replace("\r", "\n").split("\n")
    return "\n".join(line.rstrip() for line in lines).rstrip()


def find_last_line_match(pattern: re.Pattern[str], text: str) -> re.Match[str] | None:
    for line in reversed(text.splitlines()):
        if match := pattern.search(line.strip()):
            return match
    return None


def normalize_path(value: str) -> str:
    value = value.replace("\\", "/")
    match = re.match(r"^([A-Za-z]):(?:/|$)", value)
    if match:
        drive = match.group(1).lower()
        value = f"/{drive}/{value[match.end():]}"
    value = re.sub(r"/{2,}", "/", value)
    return value.rstrip("/") or "/"


def path_shape(value: str) -> str:
    normalized = normalize_path(value)
    if re.match(r"^/[a-z](?:/|$)", normalized):
        return "drive_absolute"
    if normalized.startswith("/"):
        return "posix_absolute"
    return "relative"


def shell_category(value: str) -> str | None:
    token = value.strip().strip("-").replace("\\", "/").rsplit("/", 1)[-1].lower()
    if token.endswith(".exe"):
        token = token[:-4]
    if "powershell" in token:
        return "powershell"
    if token in {"pwsh", "cmd", "bash", "zsh", "fish", "nu", "sh", "dash"}:
        return token
    if "nushell" in token:
        return "nu"
    return None


def flatten(value: Any, prefix: str = "") -> dict[str, Any]:
    result: dict[str, Any] = {}
    if isinstance(value, dict):
        for key in sorted(value):
            path = f"{prefix}.{key}" if prefix else key
            result.update(flatten(value[key], path))
    elif isinstance(value, list):
        for index, item in enumerate(value):
            path = f"{prefix}.{index}" if prefix else str(index)
            result.update(flatten(item, path))
    else:
        result[prefix] = value
    return result


def filter_flat(values: dict[str, Any], patterns: list[str]) -> dict[str, Any]:
    return {
        path: value
        for path, value in values.items()
        if not any(fnmatch.fnmatchcase(path, pattern) for pattern in patterns)
    }


def compare_flat(
    expected: dict[str, Any], actual: dict[str, Any], label: str
) -> list[str]:
    errors: list[str] = []
    for path in sorted(expected.keys() - actual.keys()):
        errors.append(f"{label}: missing field {path}")
    for path in sorted(actual.keys() - expected.keys()):
        errors.append(f"{label}: unexpected field {path}={actual[path]!r}")
    for path in sorted(expected.keys() & actual.keys()):
        if expected[path] != actual[path]:
            errors.append(
                f"{label}: {path}: expected {expected[path]!r}, got {actual[path]!r}"
            )
    return errors


class ResolvedApp:
    """Resolve packaged and unpackaged application paths to one executable."""

    def __init__(self, source: str | Path) -> None:
        matches = sorted(glob.glob(os.fspath(source)))
        if len(matches) != 1:
            raise ConformanceError(
                f"--app must resolve to exactly one path, got {len(matches)}: {source}"
            )
        self.source = Path(matches[0]).resolve()
        self._temporary: tempfile.TemporaryDirectory[str] | None = None
        self.executable = self._resolve(self.source)

    def _resolve(self, source: Path) -> Path:
        if source.is_dir() and source.suffix.lower() == ".app":
            executable = source / "Contents" / "MacOS" / "nebula"
            require(executable.is_file(), f"app bundle has no executable: {executable}")
            return executable
        if source.is_file() and source.suffix.lower() == ".zip":
            self._temporary = tempfile.TemporaryDirectory(prefix="nebula-conformance-app-")
            root = Path(self._temporary.name).resolve()
            try:
                with zipfile.ZipFile(source) as archive:
                    for member in archive.infolist():
                        target = (root / member.filename).resolve()
                        require(
                            target == root or root in target.parents,
                            f"archive member escapes extraction root: {member.filename}",
                        )
                    archive.extractall(root)
                candidates = [
                    path
                    for path in root.rglob("*")
                    if path.is_file() and path.name.lower() in {"nebula", "nebula.exe"}
                ]
                require(
                    len(candidates) == 1,
                    f"archive contains {len(candidates)} Nebula executables",
                )
                return candidates[0]
            except Exception:
                self.close()
                raise
        if source.is_dir():
            candidates = [
                path
                for path in source.rglob("*")
                if path.is_file() and path.name.lower() in {"nebula", "nebula.exe"}
            ]
            require(len(candidates) == 1, f"directory contains {len(candidates)} Nebula executables")
            return candidates[0]
        require(source.is_file(), f"application path does not exist: {source}")
        return source

    def close(self) -> None:
        if self._temporary is not None:
            self._temporary.cleanup()
            self._temporary = None


class RuntimeClient:
    def __init__(self, port: int, token: str, timeout: float = 5.0) -> None:
        self.port = port
        self.token = token
        self.timeout = timeout
        self.sequence = 0

    @classmethod
    def from_port_file(cls, path: Path, timeout: float = 5.0) -> RuntimeClient:
        parts = path.read_text(encoding="utf-8").split()
        require(len(parts) >= 2, f"invalid runtime discovery record: {path}")
        try:
            port = int(parts[0])
        except ValueError as error:
            raise ConformanceError(f"invalid runtime port in {path}: {parts[0]!r}") from error
        require(0 < port <= 65535, f"runtime port is out of range: {port}")
        return cls(port, parts[1], timeout)

    def request(
        self,
        method: str,
        params: dict[str, Any] | None = None,
        *,
        timeout: float | None = None,
        allow_error: bool = False,
    ) -> dict[str, Any]:
        self.sequence += 1
        request = {
            "protocol": PROTOCOL_NAME,
            "version": PROTOCOL_VERSION,
            "id": f"conformance-{os.getpid()}-{self.sequence}",
            "token": self.token,
            "method": method,
            "params": params or {},
        }
        payload = json.dumps(request, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
        request_timeout = timeout if timeout is not None else self.timeout
        with socket.create_connection(("127.0.0.1", self.port), request_timeout) as stream:
            stream.settimeout(request_timeout)
            stream.sendall(payload + b"\n")
            stream.shutdown(socket.SHUT_WR)
            reader = stream.makefile("rb")
            line = reader.readline(MAX_RESPONSE_BYTES + 1)
        require(line, f"{method}: runtime closed the connection without a response")
        require(len(line) <= MAX_RESPONSE_BYTES, f"{method}: response exceeds byte limit")
        try:
            response = json.loads(line.decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError) as error:
            raise ConformanceError(f"{method}: invalid UTF-8 JSON response") from error
        require(response.get("protocol") == PROTOCOL_NAME, f"{method}: wrong response protocol")
        require(response.get("version") == PROTOCOL_VERSION, f"{method}: wrong protocol version")
        require(response.get("id") == request["id"], f"{method}: response id mismatch")
        if not response.get("ok") and not allow_error:
            raise ApiFailure(method, response.get("error") or {})
        return response


class _WindowsJob:
    """Own one harness launch and its descendants, including orphaned shells.

    Killing only Nebula can leave ConPTY children holding the test directory.
    Job membership is established before the gated app launch, inherited by
    descendants, and retained after the parent exits. No machine-wide process
    enumeration or process-name matching is used. The job kills its members if the
    harness itself exits unexpectedly and Windows closes its handle.
    """

    def __init__(self) -> None:
        class BasicLimits(ctypes.Structure):
            _fields_ = [
                ("PerProcessUserTimeLimit", ctypes.c_int64),
                ("PerJobUserTimeLimit", ctypes.c_int64),
                ("LimitFlags", wintypes.DWORD),
                ("MinimumWorkingSetSize", ctypes.c_size_t),
                ("MaximumWorkingSetSize", ctypes.c_size_t),
                ("ActiveProcessLimit", wintypes.DWORD),
                ("Affinity", ctypes.c_size_t),
                ("PriorityClass", wintypes.DWORD),
                ("SchedulingClass", wintypes.DWORD),
            ]

        class ExtendedLimits(ctypes.Structure):
            _fields_ = [
                ("BasicLimitInformation", BasicLimits),
                ("IoInfo", ctypes.c_uint64 * 6),
                ("ProcessMemoryLimit", ctypes.c_size_t),
                ("JobMemoryLimit", ctypes.c_size_t),
                ("PeakProcessMemoryUsed", ctypes.c_size_t),
                ("PeakJobMemoryUsed", ctypes.c_size_t),
            ]

        class Accounting(ctypes.Structure):
            _fields_ = [
                ("TotalUserTime", ctypes.c_int64),
                ("TotalKernelTime", ctypes.c_int64),
                ("ThisPeriodTotalUserTime", ctypes.c_int64),
                ("ThisPeriodTotalKernelTime", ctypes.c_int64),
                ("TotalPageFaultCount", wintypes.DWORD),
                ("TotalProcesses", wintypes.DWORD),
                ("ActiveProcesses", wintypes.DWORD),
                ("TotalTerminatedProcesses", wintypes.DWORD),
            ]

        self._accounting = Accounting
        self._api = ctypes.WinDLL("kernel32", use_last_error=True)
        self._api.CreateJobObjectW.argtypes = (ctypes.c_void_p, wintypes.LPCWSTR)
        self._api.CreateJobObjectW.restype = wintypes.HANDLE
        self._api.SetInformationJobObject.argtypes = (
            wintypes.HANDLE, ctypes.c_int, ctypes.c_void_p, wintypes.DWORD,
        )
        self._api.SetInformationJobObject.restype = wintypes.BOOL
        self._api.AssignProcessToJobObject.argtypes = (wintypes.HANDLE, wintypes.HANDLE)
        self._api.AssignProcessToJobObject.restype = wintypes.BOOL
        self._api.TerminateJobObject.argtypes = (wintypes.HANDLE, wintypes.UINT)
        self._api.TerminateJobObject.restype = wintypes.BOOL
        self._api.QueryInformationJobObject.argtypes = (
            wintypes.HANDLE, ctypes.c_int, ctypes.c_void_p, wintypes.DWORD,
            ctypes.POINTER(wintypes.DWORD),
        )
        self._api.QueryInformationJobObject.restype = wintypes.BOOL
        self._api.OpenProcess.argtypes = (wintypes.DWORD, wintypes.BOOL, wintypes.DWORD)
        self._api.OpenProcess.restype = wintypes.HANDLE
        self._api.IsProcessInJob.argtypes = (
            wintypes.HANDLE, wintypes.HANDLE, ctypes.POINTER(wintypes.BOOL),
        )
        self._api.IsProcessInJob.restype = wintypes.BOOL
        self._api.WaitForSingleObject.argtypes = (wintypes.HANDLE, wintypes.DWORD)
        self._api.WaitForSingleObject.restype = wintypes.DWORD
        self._api.CloseHandle.argtypes = (wintypes.HANDLE,)
        self._api.CloseHandle.restype = wintypes.BOOL
        self._process_handles: list[int] = []
        self._handle = self._api.CreateJobObjectW(None, None)
        if not self._handle:
            raise ctypes.WinError(ctypes.get_last_error())
        limits = ExtendedLimits()
        limits.BasicLimitInformation.LimitFlags = 0x2000  # KILL_ON_JOB_CLOSE
        if not self._api.SetInformationJobObject(
            self._handle, 9, ctypes.byref(limits), ctypes.sizeof(limits)
        ):
            error = ctypes.WinError(ctypes.get_last_error())
            self.close()
            raise error

    def assign(self, process: subprocess.Popen[bytes]) -> None:
        # CPython retains the native process handle on Windows. Using that
        # handle avoids opening a PID that could have been recycled.
        if not self._api.AssignProcessToJobObject(self._handle, int(process._handle)):
            raise ctypes.WinError(ctypes.get_last_error())

    def terminate(self, timeout: float = 5.0) -> None:
        deadline = time.monotonic() + timeout
        self._capture_process_handles()
        if not self._api.TerminateJobObject(self._handle, 1):
            raise ctypes.WinError(ctypes.get_last_error())
        self._capture_process_handles()
        while True:
            accounting = self._accounting()
            if not self._api.QueryInformationJobObject(
                self._handle, 1, ctypes.byref(accounting), ctypes.sizeof(accounting), None
            ):
                raise ctypes.WinError(ctypes.get_last_error())
            if accounting.ActiveProcesses == 0:
                break
            if time.monotonic() >= deadline:
                raise ConformanceError(
                    f"Windows launch job still has {accounting.ActiveProcesses} "
                    f"processes after {timeout:.1f}s of cleanup"
                )
            time.sleep(0.01)
        # ActiveProcesses can reach zero before final process teardown closes
        # directory handles. Wait on the retained identities, not just that
        # accounting value, including when the launch parent exited earlier.
        for handle in self._process_handles:
            remaining_ms = max(0, int((deadline - time.monotonic()) * 1000))
            result = self._api.WaitForSingleObject(handle, remaining_ms)
            if result == 0xFFFFFFFF:
                raise ctypes.WinError(ctypes.get_last_error())
            if result != 0:
                raise ConformanceError(f"Windows job member did not exit within {timeout:.1f}s")

    def _capture_process_handles(self) -> None:
        capacity = 16
        while True:
            buffer = ctypes.create_string_buffer(8 + ctypes.sizeof(ctypes.c_size_t) * capacity)
            if self._api.QueryInformationJobObject(
                self._handle, 3, buffer, ctypes.sizeof(buffer), None
            ):
                count = wintypes.DWORD.from_buffer(buffer, 4).value
                pids = (ctypes.c_size_t * count).from_buffer(buffer, 8)
                break
            error = ctypes.get_last_error()
            if error != 234:  # ERROR_MORE_DATA: members grew beyond the buffer.
                raise ctypes.WinError(error)
            capacity = max(capacity * 2, wintypes.DWORD.from_buffer(buffer).value)
        for pid in pids:
            # These are only candidates from this job, never a machine-wide
            # process tree. Verify membership after opening to reject PID reuse.
            handle = self._api.OpenProcess(0x00101000, False, pid)
            if not handle:
                error = ctypes.get_last_error()
                if error == 87:  # The process exited before its handle was opened.
                    continue
                raise ctypes.WinError(error)
            member = wintypes.BOOL()
            if not self._api.IsProcessInJob(handle, self._handle, ctypes.byref(member)):
                error = ctypes.WinError(ctypes.get_last_error())
                self._api.CloseHandle(handle)
                raise error
            if member.value:
                self._process_handles.append(handle)
            elif not self._api.CloseHandle(handle):
                raise ctypes.WinError(ctypes.get_last_error())

    def close(self) -> None:
        if self._handle is not None:
            if not self._api.CloseHandle(self._handle):
                raise ctypes.WinError(ctypes.get_last_error())
            self._handle = None
        while self._process_handles:
            if not self._api.CloseHandle(self._process_handles[-1]):
                raise ctypes.WinError(ctypes.get_last_error())
            self._process_handles.pop()


class ConformanceContext:
    def __init__(
        self,
        app: ResolvedApp,
        platform: str,
        config_dir: Path,
        work_dir: Path,
        artifact_dir: Path,
        startup_timeout: float = DEFAULT_STARTUP_TIMEOUT,
    ) -> None:
        self.app = app
        self.platform = platform
        self.platform_family = platform.split("-", 1)[0]
        self.config_dir = config_dir.resolve()
        self.work_dir = work_dir.resolve()
        self.artifact_dir = artifact_dir.resolve()
        self.startup_timeout = startup_timeout
        self.process: subprocess.Popen[bytes] | None = None
        self._windows_job: _WindowsJob | None = None
        self.client: RuntimeClient | None = None
        self.description: dict[str, Any] = {}
        self.startup_ms = 0
        self.window_id = 0
        self.pane_id = 0
        self.shell = "unknown"
        self._launch_number = 0
        self._log_handle: Any = None
        self._marker_sequence = 0

    @property
    def port_file(self) -> Path:
        return self.config_dir / "runtime.port"

    @property
    def session_file(self) -> Path:
        return self.config_dir / "session.json"

    def prepare(self) -> None:
        self.config_dir.mkdir(parents=True, exist_ok=True)
        self.work_dir.mkdir(parents=True, exist_ok=True)
        self.artifact_dir.mkdir(parents=True, exist_ok=True)
        settings = "\n".join(
            [
                "keep_session=0",
                "tray=0",
                "restore_session=1",
                "resume_ai=0",
                "fetch=0",
                "auto_check_updates=0",
                "ai_hooks=0",
                "windowing_behavior=use_new",
                "",
            ]
        )
        (self.config_dir / "nebula_settings.txt").write_text(
            settings, encoding="utf-8", newline="\n"
        )

    def start(self, *, explicit_working_directory: bool = True) -> None:
        require(
            self.process is None and self._windows_job is None,
            "Nebula is already running in this context",
        )
        self._launch_number += 1
        log_path = self.artifact_dir / f"nebula-{self._launch_number}.log"
        self._log_handle = log_path.open("wb")
        env = os.environ.copy()
        env["NEBULA_CONFIG_DIR"] = os.fspath(self.config_dir)
        env["RUST_BACKTRACE"] = "1"
        if self.app.executable.suffix.lower() == ".appimage":
            env.setdefault("APPIMAGE_EXTRACT_AND_RUN", "1")
        command = [os.fspath(self.app.executable)]
        if explicit_working_directory:
            command.extend(["--working-directory", os.fspath(self.work_dir)])
        started = time.monotonic()
        try:
            self.process = self._spawn_process(command, env)
        except OSError as error:
            self._close_log()
            raise ConformanceError(f"could not launch {self.app.executable}: {error}") from error

        deadline = started + self.startup_timeout
        last_error = "runtime.port has not appeared"
        while time.monotonic() < deadline:
            if self.process.poll() is not None:
                code = self.process.returncode
                self.stop(force=True)
                raise ConformanceError(f"Nebula exited during startup with code {code}")
            if self.port_file.is_file():
                try:
                    candidate = RuntimeClient.from_port_file(self.port_file, timeout=5.0)
                    response = candidate.request("runtime.describe", timeout=1.0)
                    remaining = max(0.1, deadline - time.monotonic())
                    snapshot_response = candidate.request(
                        "runtime.snapshot", timeout=min(5.0, remaining)
                    )
                    snapshot = snapshot_response.get("result")
                    require(isinstance(snapshot, dict), "runtime snapshot is not an object")
                    windows = snapshot.get("windows") or []
                    ready = any(
                        any(
                            tab.get("kind") == "shell" and bool(tab.get("panes"))
                            for tab in window.get("tabs") or []
                        )
                        for window in windows
                    )
                    require(ready, "runtime snapshot has no ready shell pane")
                    self.client = candidate
                    self.description = response["result"]
                    self.startup_ms = round((time.monotonic() - started) * 1000)
                    return
                except (OSError, ConformanceError) as error:
                    last_error = str(error)
            time.sleep(0.05)
        self.stop(force=True)
        raise ConformanceError(f"Nebula runtime did not become ready: {last_error}")

    def _spawn_process(self, command: list[str], env: dict[str, str]) -> subprocess.Popen[bytes]:
        options = {
            "cwd": self.work_dir,
            "env": env,
            "stdout": self._log_handle,
            "stderr": subprocess.STDOUT,
        }
        if os.name != "nt":
            return subprocess.Popen(command, stdin=subprocess.DEVNULL, **options)
        job = _WindowsJob()
        process = None
        try:
            process = subprocess.Popen(
                [sys.executable, "-I", "-S", "-c", _WINDOWS_LAUNCH_GATE, *command],
                stdin=subprocess.PIPE, **options,
            )
            job.assign(process)
            assert process.stdin is not None
            with process.stdin:
                process.stdin.write(b"G")
        except BaseException:
            try:
                # Before assignment the gate has no child; after assignment
                # the job owns anything it could have started.
                if process is not None:
                    if process.poll() is None:
                        process.kill()
                    process.wait(timeout=5)
                    if process.stdin is not None:
                        process.stdin.close()
                job.terminate()
            finally:
                job.close()
            raise
        self._windows_job = job
        return process

    def stop(self, *, force: bool, timeout: float = 5.0) -> None:
        process = self.process
        try:
            if self._windows_job is not None:
                deadline = time.monotonic() + timeout
                self._windows_job.terminate(timeout)
                if process is not None:
                    process.wait(timeout=max(0, deadline - time.monotonic()))
                self._windows_job.close()
            elif process is not None and process.poll() is None:
                if force:
                    process.kill()
                else:
                    process.terminate()
                try:
                    process.wait(timeout=timeout)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=timeout)
        finally:
            self._close_log()
        # Preserve ownership if cleanup fails, so a caller can retry and the
        # failure is not replaced by a silent temporary-directory exclusion.
        self.process = None
        self._windows_job = None
        self.client = None

    def restart(self) -> int:
        self.stop(force=True)
        # An explicit --working-directory intentionally suppresses Nebula's
        # session restore. A cold-restore check must therefore be a plain
        # launch; the process cwd remains the isolated work directory.
        self.start(explicit_working_directory=False)
        return self.startup_ms

    def _close_log(self) -> None:
        if self._log_handle is not None:
            self._log_handle.close()
            self._log_handle = None

    def api(
        self,
        method: str,
        params: dict[str, Any] | None = None,
        *,
        timeout: float | None = None,
        allow_error: bool = False,
    ) -> Any:
        require(self.client is not None, "Nebula runtime is not connected")
        response = self.client.request(method, params, timeout=timeout, allow_error=allow_error)
        return response if allow_error else response.get("result")

    def best_effort_api(self, method: str, params: dict[str, Any]) -> None:
        """Run cleanup without replacing the case's original failure."""
        try:
            self.api(method, params)
        except (OSError, ConformanceError):
            pass

    def snapshot(self) -> dict[str, Any]:
        snapshot = self.api("runtime.snapshot")
        require(isinstance(snapshot, dict), "runtime.snapshot did not return an object")
        return snapshot

    def refresh_targets(self, snapshot: dict[str, Any] | None = None) -> None:
        snapshot = snapshot or self.snapshot()
        windows = snapshot.get("windows") or []
        require(windows, "runtime snapshot has no window")
        window = next((item for item in windows if item.get("focused")), windows[0])
        tabs = [item for item in window.get("tabs") or [] if item.get("kind") == "shell"]
        require(tabs, "runtime window has no shell tab")
        active_index = window.get("active_tab", 0)
        tab = next((item for item in tabs if item.get("index") == active_index), tabs[0])
        panes = tab.get("panes") or []
        require(panes, "runtime shell tab has no pane")
        focused = tab.get("focused_pane_id") or window.get("focused_pane_id")
        pane = next((item for item in panes if item.get("id") == focused), panes[0])
        self.window_id = int(window["id"])
        self.pane_id = int(pane["id"])

    def tab_for_pane(self, snapshot: dict[str, Any], pane_id: int) -> dict[str, Any]:
        for window in snapshot.get("windows") or []:
            for tab in window.get("tabs") or []:
                if any(pane.get("id") == pane_id for pane in tab.get("panes") or []):
                    return tab
        raise ConformanceError(f"pane {pane_id} is absent from the runtime snapshot")

    def read(self, pane_id: int | None = None, lines: int = 120) -> dict[str, Any]:
        result = self.api(
            "pane.read",
            {
                "window_id": self.window_id,
                "pane_id": pane_id or self.pane_id,
                "lines": lines,
            },
        )
        require(isinstance(result, dict), "pane.read did not return an object")
        result["text"] = normalize_text(result.get("text", ""))
        return result

    def poll(
        self,
        probe: Callable[[], Any],
        predicate: Callable[[Any], bool],
        message: str,
        timeout: float = 10.0,
    ) -> Any:
        deadline = time.monotonic() + timeout
        last: Any = None
        while time.monotonic() < deadline:
            last = probe()
            if predicate(last):
                return last
            time.sleep(0.1)
        raise ConformanceError(f"{message}; last observation: {last!r}")

    def prompt(self, text: str, pane_id: int | None = None, submit: bool = True) -> Any:
        params = {
            "window_id": self.window_id,
            "pane_id": pane_id or self.pane_id,
            "text": text,
            "submit": submit,
        }
        deadline = time.monotonic() + 3.0
        while True:
            response = self.api("pane.prompt", params, allow_error=True)
            if response.get("ok"):
                return response.get("result")
            error = response.get("error") or {}
            if error.get("code") != "input_in_progress" or time.monotonic() >= deadline:
                raise ApiFailure("pane.prompt", error)
            time.sleep(0.05)

    def paste(self, text: str, pane_id: int | None = None, submit: bool = False) -> Any:
        params = {
            "window_id": self.window_id,
            "pane_id": pane_id or self.pane_id,
            "text": text,
            "submit": submit,
        }
        deadline = time.monotonic() + 10.0
        while True:
            response = self.api("pane.paste", params, allow_error=True)
            if response.get("ok"):
                return response.get("result")
            error = response.get("error") or {}
            retryable = error.get("code") in {"input_in_progress", "unsafe_input_mode"}
            if not retryable or time.monotonic() >= deadline:
                raise ApiFailure("pane.paste", error)
            time.sleep(0.1)

    def send_key(
        self,
        key: str,
        pane_id: int | None = None,
        *,
        control: bool = False,
        alt: bool = False,
        shift: bool = False,
    ) -> Any:
        return self.api(
            "pane.send_key",
            {
                "window_id": self.window_id,
                "pane_id": pane_id or self.pane_id,
                "key": key,
                "modifiers": {"control": control, "alt": alt, "shift": shift},
                "repeat": 1,
            },
        )

    def wait_for_line(
        self, pattern: re.Pattern[str], pane_id: int | None = None, timeout: float = 10.0
    ) -> tuple[re.Match[str], dict[str, Any]]:
        target = pane_id or self.pane_id

        def find(read: dict[str, Any]) -> re.Match[str] | None:
            # Terminal output can begin on the same physical row as a wrapped
            # prompt or command echo. Callers construct markers so the echoed
            # source does not contain the complete expected token.
            return find_last_line_match(pattern, read["text"])

        read = self.poll(
            lambda: self.read(target, 160),
            lambda value: find(value) is not None,
            f"pane {target} never produced {pattern.pattern!r}",
            timeout,
        )
        match = find(read)
        assert match is not None
        return match, read

    def detect_shell(self) -> str:
        candidates: list[str] = []
        try:
            processes = self.api(
                "pane.procs", {"window_id": self.window_id, "pane_id": self.pane_id}
            )
            candidates.extend(
                process.get("executable", "")
                for process in processes.get("processes") or []
                if process.get("depth") == 0
            )
        except ApiFailure:
            pass
        snapshot = self.snapshot()
        tab = self.tab_for_pane(snapshot, self.pane_id)
        pane = next(item for item in tab["panes"] if item["id"] == self.pane_id)
        candidates.extend(
            [pane.get("running_program") or "", pane.get("title") or "", tab.get("label") or ""]
        )
        for candidate in candidates:
            category = shell_category(candidate)
            if category:
                self.shell = category
                return category
        fallback = {"windows": "powershell", "macos": "zsh", "linux": "sh"}
        self.shell = fallback.get(self.platform_family, "sh")
        return self.shell

    def marker_command(self, prefix: str, suffix: str) -> str:
        shell = self.shell
        if shell in {"powershell", "pwsh"}:
            return f"Write-Output ('{prefix}' + '{suffix}')"
        if shell == "cmd":
            return f"echo {prefix}^{suffix}"
        if shell == "nu":
            return f"print ('{prefix}' + '{suffix}')"
        return f"printf '%s%s\\n' '{prefix}' '{suffix}'"

    def columns_command(self, marker: str) -> str:
        shell = self.shell
        if shell in {"powershell", "pwsh"}:
            return f"Write-Output ('{marker}' + $Host.UI.RawUI.WindowSize.Width)"
        if shell == "cmd":
            return (
                "powershell -NoProfile -Command \"Write-Output "
                f"('{marker}' + $Host.UI.RawUI.WindowSize.Width)\""
            )
        if shell == "fish":
            return f"printf '{marker}%s\\n' (tput cols)"
        if shell == "nu":
            return f"print ('{marker}' + ((term size).columns | into string))"
        return f"printf '{marker}%s\\n' \"$(tput cols)\""

    def measure_columns(self, pane_id: int) -> int:
        self._marker_sequence += 1
        marker = f"NEBULA_COLS_{self._marker_sequence}_"
        self.prompt(self.columns_command(marker), pane_id)
        match, _ = self.wait_for_line(re.compile(re.escape(marker) + r"(\d+)"), pane_id)
        return int(match.group(1))

    def scrollback_command(self, lines: int, marker: str) -> str:
        shell = self.shell
        require(len(marker) >= 2, "completion marker must contain at least two characters")
        # The command is echoed before its output arrives. Only execution may
        # assemble the complete token that wait_for_line uses as completion.
        completion = self.marker_command(marker[:-1], marker[-1])
        if shell in {"powershell", "pwsh"}:
            return f"1..{lines} | ForEach-Object {{ 'NEBULA_SCROLL_' + $_ }}; {completion}"
        if shell == "cmd":
            return f"(for /L %i in (1,1,{lines}) do @echo NEBULA_SCROLL_%i) & {completion}"
        if shell == "fish":
            return f"for i in (seq 1 {lines}); echo NEBULA_SCROLL_$i; end; {completion}"
        if shell == "nu":
            return (
                f"1..{lines} | each {{ |i| print $'NEBULA_SCROLL_($i)' }}; "
                f"{completion}"
            )
        return (
            f"i=1; while [ \"$i\" -le {lines} ]; do printf 'NEBULA_SCROLL_%s\\n' \"$i\"; "
            f"i=$((i + 1)); done; {completion}"
        )

    def ensure_single_tab(self) -> None:
        snapshot = self.snapshot()
        window = next(item for item in snapshot["windows"] if item["id"] == self.window_id)
        keep = next((tab for tab in window["tabs"] if tab["index"] == 0), None)
        require(keep is not None, "runtime window has no tab at index zero")
        panes = keep.get("panes") or []
        require(panes, "tab zero has no pane to retain")
        keep_pane = keep.get("focused_pane_id") or panes[0]["id"]
        for tab in sorted(window["tabs"], key=lambda item: item["index"], reverse=True):
            if tab["index"] == 0:
                continue
            self.api(
                "tab.close", {"window_id": self.window_id, "tab_index": tab["index"]}
            )
        self.pane_id = int(keep_pane)
        self.api("window.focus", {"window_id": self.window_id, "pane_id": self.pane_id})

    def wait_for_session(self, predicate: Callable[[dict[str, Any]], bool]) -> dict[str, Any]:
        def read_session() -> dict[str, Any] | None:
            try:
                return json.loads(self.session_file.read_text(encoding="utf-8"))
            except (OSError, json.JSONDecodeError):
                return None

        return self.poll(
            read_session,
            lambda value: isinstance(value, dict) and predicate(value),
            "session.json did not converge",
            timeout=5.0,
        )

    def close_and_wait(self, timeout: float = 5.0) -> int:
        require(self.process is not None, "Nebula process is not running")
        process = self.process
        started = time.monotonic()
        deadline = started + timeout
        try:
            self.api("window.close", {"window_id": self.window_id})
        except ApiFailure:
            raise
        except (OSError, ConformanceError):
            # A successful close may tear down the listener before its final
            # response is observed. The owned process exit is authoritative.
            pass
        try:
            process.wait(timeout=max(0, deadline - time.monotonic()))
        except subprocess.TimeoutExpired as error:
            raise ConformanceError(f"window.close did not exit within {timeout:.1f}s") from error
        self.stop(force=True, timeout=max(0, deadline - time.monotonic()))
        if process.returncode != 0:
            raise ConformanceError(f"Nebula exited with code {process.returncode}")
        return round((time.monotonic() - started) * 1000)


def platform_family(platform: str) -> str:
    return platform.split("-", 1)[0]


def load_rules(golden_dir: Path) -> dict[str, Any]:
    path = golden_dir / "whitelist.json"
    return json.loads(path.read_text(encoding="utf-8"))


def stable_flat(report: dict[str, Any], golden_dir: Path) -> dict[str, Any]:
    rules = load_rules(golden_dir)
    return filter_flat(flatten(report), list(rules.get("volatile", {})))


def compare_platform_golden(report: dict[str, Any], golden_dir: Path) -> list[str]:
    family = platform_family(str(report["platform"]))
    path = golden_dir / f"{family}.json"
    if not path.is_file():
        return [f"platform golden is missing: {path}"]
    expected = json.loads(path.read_text(encoding="utf-8"))
    return compare_flat(expected, stable_flat(report, golden_dir), family)


def compare_reports(reports: list[dict[str, Any]], golden_dir: Path) -> list[str]:
    require(len(reports) >= 2, "--compare requires at least two reports")
    rules = load_rules(golden_dir)
    ignored = list(rules.get("volatile", {})) + list(rules.get("cross_platform", {}))
    flattened = [filter_flat(flatten(report), ignored) for report in reports]
    baseline = flattened[0]
    baseline_name = str(reports[0].get("platform", "report-1"))
    errors: list[str] = []
    for report, actual in zip(reports[1:], flattened[1:]):
        name = str(report.get("platform", "report"))
        errors.extend(compare_flat(baseline, actual, f"{baseline_name} vs {name}"))
    return errors


def validate_common(report: dict[str, Any], golden_dir: Path) -> list[str]:
    common = json.loads((golden_dir / "common.json").read_text(encoding="utf-8"))
    errors: list[str] = []
    if report.get("schema_version") != common.get("schema_version"):
        errors.append("report schema version does not match common.json")
    flat = flatten(report)
    for path, expected in common.get("required", {}).items():
        actual = flat.get(path)
        if actual != expected:
            errors.append(f"common: {path}: expected {expected!r}, got {actual!r}")
    return errors
