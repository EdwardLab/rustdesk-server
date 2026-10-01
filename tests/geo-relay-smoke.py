#!/usr/bin/env python3
"""Exercise real hbbs/hbbr processes without touching an installed client."""
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]
BIN = Path(os.environ.get("RUSTDESK_TEST_BIN", ROOT / "target/debug"))
FIXTURE = ROOT / "tests/data/GeoIP2-City-Test.mmdb"


def free_port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def stop(process):
    if process.poll() is None:
        process.terminate()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=5)


def console(port, command):
    with socket.create_connection(("127.0.0.1", port - 1), timeout=1) as sock:
        sock.sendall(command.encode())
        response = b""
        while chunk := sock.recv(4096):
            response += chunk
        return response.decode().strip()


def wait_for(predicate, description):
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline:
        try:
            if predicate():
                return
        except (OSError, ValueError):
            pass
        time.sleep(0.1)
    raise AssertionError(f"Timed out: {description}")


def run():
    # Each server also binds adjacent ports; reserve the entire set first.
    for _ in range(20):
        port = free_port()
        first, second = port + 10, port + 20
        if second + 2 > 65535:
            continue
        ports = [port - 1, port, port + 2, first, first + 2, second, second + 2]
        reservations = []
        try:
            for candidate in ports:
                sock = socket.socket()
                reservations.append(sock)
                sock.bind(("127.0.0.1", candidate))
        except OSError:
            continue
        finally:
            for sock in reservations:
                sock.close()
        break
    else:
        raise AssertionError("Could not allocate local server ports")

    hosts = [f"127.0.0.1:{first}", f"127.0.0.1:{second}"]
    processes = []
    logs = []
    with tempfile.TemporaryDirectory(prefix="rustdesk-geo-") as directory:
        work = Path(directory)
        locations = work / "locations.json"
        london, tokyo = [51.5, -0.1], [35.7, 139.7]
        locations.write_text(json.dumps(dict(zip(hosts, [london, tokyo]))))
        env = dict(os.environ, RUST_LOG="info", GEOIP_DB=str(FIXTURE), RELAY_LOCATIONS=str(locations))

        def launch(name, arguments, suffix, environment=env):
            cwd = work / suffix
            cwd.mkdir(exist_ok=True)
            log = (cwd / "process.log").open("w")
            logs.append(log)
            process = subprocess.Popen(
                [str(BIN / name), *arguments], cwd=cwd, env=environment,
                stdout=log, stderr=subprocess.STDOUT,
            )
            processes.append(process)
            return process

        def chosen(ip):
            return json.loads(console(port, f"test-geo {ip} {ip}"))

        try:
            relay_a = launch("hbbr", ["-p", str(first)], "relay-a")
            relay_b = launch("hbbr", ["-p", str(second)], "relay-b")
            server = launch("hbbs", ["-p", str(port), "-r", ",".join(hosts)], "hbbs")
            wait_for(lambda: set(console(port, "rs").splitlines()) == set(hosts), "both relays online")
            assert chosen("81.2.69.160") == hosts[0]
            assert chosen("::ffff:81.2.69.160") == hosts[0]
            assert chosen("2001:218::") == hosts[1]

            # Unknown IPs use round-robin; peers are never sent to an offline node.
            assert {chosen("127.0.0.1"), chosen("127.0.0.1")} == set(hosts)
            stop(relay_b)
            wait_for(lambda: console(port, "rs") == hosts[0], "failed Tokyo relay removed")
            assert chosen("2001:218::") == hosts[0]
            stop(relay_a)
            wait_for(lambda: console(port, "rs") == "", "all failed relays removed")
            assert chosen("81.2.69.160") == ""

            launch("hbbr", ["-p", str(first)], "relay-a")
            launch("hbbr", ["-p", str(second)], "relay-b")
            wait_for(lambda: set(console(port, "rs").splitlines()) == set(hosts), "relays recovered")
            assert chosen("81.2.69.160") == hosts[0]

            # A bad reload leaves the current routing active; a valid one is applied.
            locations.write_text('{"bad": [100, 0]}')
            console(port, "reload-geo")
            wait_for(lambda: "Invalid latitude/longitude" in (work / "hbbs/process.log").read_text(), "invalid reload logged")
            assert chosen("81.2.69.160") == hosts[0]
            locations.write_text(json.dumps(dict(zip(hosts, [tokyo, london]))))
            console(port, "reload-geo")
            wait_for(lambda: chosen("81.2.69.160") == hosts[1], "valid reload applied")
            assert server.poll() is None

            # Invalid startup configuration fails rather than using round-robin.
            locations.write_text(json.dumps({hosts[0]: london}))
            invalid = launch("hbbs", ["-p", str(free_port()), "-r", ",".join(hosts)], "invalid")
            assert invalid.wait(timeout=5) != 0
            assert "Missing geographic location" in (work / "invalid/process.log").read_text()
            print("PASS: GeoIP selection, IPv6, fallback, failure/recovery, reload and startup validation")
        except Exception:
            for log_path in work.glob("*/process.log"):
                print(f"--- {log_path.parent.name} ---\n{log_path.read_text()[-4000:]}")
            raise
        finally:
            for process in reversed(processes):
                stop(process)
            for log in logs:
                log.close()


if __name__ == "__main__":
    run()
