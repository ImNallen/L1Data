#!/usr/bin/env python3
import json
import os
from pathlib import Path
import secrets
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time


ROOT = Path(__file__).resolve().parent.parent
IMAGE = "mcr.microsoft.com/mssql/server:2022-latest"


def run(args, **kwargs):
    kwargs.setdefault("timeout", 30)
    result = subprocess.run(args, text=True, capture_output=True, **kwargs)
    if result.returncode:
        raise RuntimeError(f"{args[0]} exited {result.returncode}: {result.stdout}\n{result.stderr}")
    return result


def main():
    for tool in ("docker", "openssl", "cargo", "rustc"):
        if not shutil.which(tool):
            raise RuntimeError(f"required tool missing: {tool}")
    run(["docker", "image", "inspect", IMAGE])
    name = f"l1-data-tls-{secrets.token_hex(6)}"
    temporary = tempfile.TemporaryDirectory(prefix="l1-data-mssql-tls-")
    work = Path(temporary.name).resolve()
    created = False
    try:
        with socket.socket() as reservation:
            reservation.bind(("127.0.0.1", 0))
            port = reservation.getsockname()[1]
        assert port not in (1434, 5433, 7077, 5173)
        print(f"fixture {name} on 127.0.0.1:{port}", flush=True)
        tls = work / "tls"
        tls.mkdir(mode=0o755)
        empty = work / "empty-roots"
        empty.mkdir()
        for authority in ("ca", "unrelated"):
            run(["openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes",
                 "-keyout", str(work / f"{authority}.key"), "-out", str(work / f"{authority}.pem"),
                 "-days", "2", "-sha256", "-subj", f"/CN=L1 disposable {authority}",
                 "-addext", "basicConstraints=critical,CA:TRUE",
                 "-addext", "keyUsage=critical,keyCertSign,cRLSign"])
        run(["openssl", "req", "-new", "-newkey", "rsa:2048", "-nodes",
             "-keyout", str(tls / "server.key"), "-out", str(work / "server.csr"),
             "-subj", "/CN=localhost"])
        extensions = work / "extensions"
        extensions.write_text("basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\nsubjectAltName=DNS:localhost\n")
        run(["openssl", "x509", "-req", "-in", str(work / "server.csr"),
             "-CA", str(work / "ca.pem"), "-CAkey", str(work / "ca.key"),
             "-CAcreateserial", "-out", str(tls / "server.pem"), "-days", "2",
             "-sha256", "-extfile", str(extensions)])
        # The parent directory stays private on the host; SQL Server's container user must read the mounted key.
        (tls / "server.key").chmod(0o644)
        (tls / "mssql.conf").write_text("[network]\ntlscert = /tls/server.pem\ntlskey = /tls/server.key\ntlsprotocols = 1.2\nforceencryption = 1\n")
        password = "Tls1!" + secrets.token_hex(16)
        environment = work / "container.env"
        environment.write_text(f"ACCEPT_EULA=Y\nMSSQL_PID=Developer\nMSSQL_SA_PASSWORD={password}\n")
        environment.chmod(0o600)
        build = run(["cargo", "build", "--locked", "-p", "engine", "--message-format=json"], cwd=ROOT, timeout=600)
        artifacts = {}
        for line in build.stdout.splitlines():
            message = json.loads(line)
            if message.get("reason") == "compiler-artifact":
                for filename in message["filenames"]:
                    if filename.endswith(".rlib"):
                        artifacts[message["target"]["name"]] = filename
        probe = work / "probe"
        command = ["rustc", "--edition=2024", str(ROOT / "dev/mssql-tls-probe.rs"), "-o", str(probe)]
        for crate in ("engine", "tokio", "serde_json"):
            library = artifacts[crate]
            command += ["--extern", f"{crate}={library}", "-L", f"dependency={Path(library).parent}"]
        run(command, cwd=ROOT)
        run(["docker", "create", "--name", name, "--pull=never", "--platform", "linux/amd64",
             "--env-file", str(environment), "-p", f"127.0.0.1:{port}:1433",
             "--mount", f"type=bind,source={tls},target=/tls,readonly",
             "--mount", f"type=bind,source={tls / 'mssql.conf'},target=/var/opt/mssql/mssql.conf,readonly", IMAGE])
        created = True
        run(["docker", "start", name])
        login = ["docker", "exec", name, "sh", "-c",
                 '/opt/mssql-tools18/bin/sqlcmd -S localhost -U sa -P "$MSSQL_SA_PASSWORD" -C -b -Q "SELECT 1"']
        deadline = time.monotonic() + 150
        while subprocess.run(login, capture_output=True, timeout=30).returncode:
            state = json.loads(run(["docker", "inspect", name]).stdout)[0]["State"]
            if not state["Running"] or time.monotonic() > deadline:
                raise RuntimeError(f"fixture never accepted an sa login; container running: {state['Running']}")
            time.sleep(1)
        cases = [
            ("trusted localhost", "localhost", "VerifyFull", "ca", None),
            ("trusted wrong name", "127.0.0.1", "VerifyFull", "ca", "name"),
            ("unrelated CA", "localhost", "VerifyFull", "unrelated", "issuer"),
            ("Require control", "localhost", "Require", "unrelated", None),
            ("Require IP control", "127.0.0.1", "Require", "unrelated", None),
        ]
        for index, (label, host, mode, authority, expected) in enumerate(cases):
            child_env = dict(os.environ, SSL_CERT_FILE=str(work / f"{authority}.pem"),
                             SSL_CERT_DIR=str(empty), MSSQL_SA_PASSWORD=password)
            result = run([str(probe), str(work / f"engine-{index}"), host, str(port), mode],
                         env=child_env, timeout=40)
            value = json.loads(result.stdout)
            print(f"{label}: {json.dumps(value)}", flush=True)
            if expected is None:
                assert "Ok" in value, f"{label} did not complete Engine login and decode server identity"
                assert value["Ok"]["versionNum"] == 160000, "expected SQL Server 2022"
            else:
                error = value.get("Err", {})
                assert error.get("kind") == "Tls", f"{label} was not a TLS validation rejection"
                message = error["message"].lower()
                assert "certificate" in message, f"{label} was not a certificate rejection"
                if expected == "name":
                    assert "not valid for name" in message or "notvalidforname" in message, message
                    assert "issuer" not in message, message
                else:
                    assert "unknownissuer" in message or "unknown issuer" in message, message
        print("PASS: public Engine verified CA and hostname, rejected unrelated CA, and completed Require controls", flush=True)
    finally:
        original_failure = sys.exc_info()[0] is not None
        cleanup_errors = []
        try:
            if created:
                run(["docker", "rm", "-f", "-v", name], timeout=30)
            remaining = run(["docker", "ps", "-a", "--filter", f"name=^/{name}$", "--format", "{{.Names}}"])
            assert not remaining.stdout.strip(), f"fixture container remains: {name}"
        except Exception as error:
            cleanup_errors.append(str(error))
        finally:
            try:
                temporary.cleanup()
                assert not work.exists(), f"temporary certificates remain: {work}"
            except Exception as error:
                cleanup_errors.append(str(error))
        if cleanup_errors:
            print(f"cleanup failed: {cleanup_errors}", file=sys.stderr, flush=True)
            if not original_failure:
                raise RuntimeError("fixture cleanup failed")
        else:
            print(f"cleanup verified: {name} absent; {work} absent", flush=True)


def interrupted(_signum, _frame):
    raise KeyboardInterrupt()


if __name__ == "__main__":
    signal.signal(signal.SIGTERM, interrupted)
    main()
