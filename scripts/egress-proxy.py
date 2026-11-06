#!/usr/bin/env python3
"""The egress test's recording proxy (ADR 0004, "Egress").

    scripts/egress-proxy.py --log FILE [--listen 127.0.0.1:18480]
        [--edge 127.0.0.1:18443] --allow HOST [--allow HOST ...]

An HTTP proxy on loopback that the live tests send every request through
(FAMILYCHAT_HARNESS_EGRESS_PROXY for the harness client, and HTTPS_PROXY,
HTTP_PROXY and ALL_PROXY for any other client that honours them). Each
connection is one line in the log:

    ALLOW host:port   a CONNECT to a harness host, tunnelled to the edge
    DENY host:port    a CONNECT to any other host, answered 403
    DENY host         a plain-HTTP request (the harness is HTTPS only)

The tunnel is raw TCP: TLS stays end to end between the client (which
trusts only the harness CA) and the edge, which picks the host by SNI.
When the edge is down (`scripts/homeserver-harness.sh stop-edge`) a
CONNECT is answered 502 at once, which a client sees as a failed
connection, as it would without the proxy.

scripts/check-egress.sh reads the log and fails the build on any host that
is not a harness homeserver. Python's standard library only.
"""

import argparse
import asyncio
import sys

# The longest request head the proxy reads before giving up on a client.
MAX_HEAD = 16 * 1024
# How long a client has to send its request head.
HEAD_TIMEOUT = 10
# How long opening the tunnel to the edge may take.
CONNECT_TIMEOUT = 10
# Bytes copied per read.
CHUNK = 64 * 1024


class Recorder:
    """Appends one line per connection to the log, flushed at once."""

    def __init__(self, path):
        self.file = open(path, "a", encoding="utf-8", buffering=1)

    def record(self, line):
        self.file.write(line + "\n")
        self.file.flush()


def split_host_port(authority, default_port):
    """`host:port` (or `[v6]:port`) as a lower-case host and a port."""
    authority = authority.strip()
    if authority.startswith("["):
        host, _, rest = authority[1:].partition("]")
        port = rest[1:] if rest.startswith(":") else ""
    else:
        host, _, port = authority.rpartition(":")
        if not host:
            host, port = authority, ""
    host = host.strip().rstrip(".").lower()
    try:
        port_number = int(port) if port else default_port
    except ValueError:
        port_number = default_port
    return host, port_number


def plain_request_host(target, headers):
    """The host a plain-HTTP proxy request names."""
    if "://" in target:
        authority = target.split("://", 1)[1].split("/", 1)[0]
        authority = authority.rsplit("@", 1)[-1]
        return split_host_port(authority, 80)[0]
    for line in headers:
        name, _, value = line.partition(":")
        if name.strip().lower() == "host":
            return split_host_port(value, 80)[0]
    return "(no host)"


async def respond(writer, status):
    writer.write(
        f"HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".encode()
    )
    try:
        await writer.drain()
    except ConnectionError:
        pass
    writer.close()


async def pipe(reader, writer):
    try:
        while True:
            data = await reader.read(CHUNK)
            if not data:
                break
            writer.write(data)
            await writer.drain()
    except (ConnectionError, asyncio.CancelledError):
        pass
    finally:
        try:
            if writer.can_write_eof():
                writer.write_eof()
        except (OSError, RuntimeError):
            pass


async def tunnel(client_reader, client_writer, edge_host, edge_port):
    try:
        edge_reader, edge_writer = await asyncio.wait_for(
            asyncio.open_connection(edge_host, edge_port), CONNECT_TIMEOUT
        )
    except (OSError, asyncio.TimeoutError):
        await respond(client_writer, "502 Bad Gateway")
        return
    client_writer.write(b"HTTP/1.1 200 Connection established\r\n\r\n")
    await client_writer.drain()
    await asyncio.gather(
        pipe(client_reader, edge_writer), pipe(edge_reader, client_writer)
    )
    for writer in (edge_writer, client_writer):
        writer.close()


def handler(args, recorder):
    allowed = {host.lower().rstrip(".") for host in args.allow}
    edge_host, edge_port = split_host_port(args.edge, 443)

    async def handle(reader, writer):
        try:
            head = await asyncio.wait_for(
                reader.readuntil(b"\r\n\r\n"), HEAD_TIMEOUT
            )
        except (
            asyncio.IncompleteReadError,
            asyncio.LimitOverrunError,
            asyncio.TimeoutError,
            ConnectionError,
        ):
            writer.close()
            return
        lines = head.decode("latin-1").split("\r\n")
        parts = lines[0].split()
        if len(parts) != 3:
            recorder.record("DENY (malformed request)")
            await respond(writer, "400 Bad Request")
            return
        method, target, _ = parts
        if method.upper() != "CONNECT":
            recorder.record(f"DENY {plain_request_host(target, lines[1:])}")
            await respond(writer, "403 Forbidden")
            return
        host, port = split_host_port(target, 443)
        if host not in allowed or port != 443:
            recorder.record(f"DENY {host}:{port}")
            await respond(writer, "403 Forbidden")
            return
        recorder.record(f"ALLOW {host}:{port}")
        await tunnel(reader, writer, edge_host, edge_port)

    return handle


async def serve(args):
    recorder = Recorder(args.log)
    listen_host, listen_port = split_host_port(args.listen, 18480)
    server = await asyncio.start_server(
        handler(args, recorder), listen_host, listen_port, limit=MAX_HEAD
    )
    bound = server.sockets[0].getsockname()
    print(f"egress proxy on {bound[0]}:{bound[1]}, log {args.log}", flush=True)
    if args.port_file:
        with open(args.port_file, "w", encoding="utf-8") as port_file:
            port_file.write(f"{bound[1]}\n")
    async with server:
        await server.serve_forever()


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--log", required=True, help="the log to append to")
    parser.add_argument(
        "--listen", default="127.0.0.1:18480", help="address to listen on (port 0: any)"
    )
    parser.add_argument(
        "--edge", default="127.0.0.1:18443", help="the harness front end"
    )
    parser.add_argument(
        "--allow", action="append", default=[], help="a harness host (repeat)"
    )
    parser.add_argument("--port-file", help="write the port listened on here")
    args = parser.parse_args()
    if not args.allow:
        parser.error("name at least one harness host with --allow")
    listen_host, _ = split_host_port(args.listen, 18480)
    if listen_host not in ("127.0.0.1", "::1", "localhost"):
        parser.error("the proxy listens on loopback only")
    try:
        asyncio.run(serve(args))
    except KeyboardInterrupt:
        pass
    return 0


if __name__ == "__main__":
    sys.exit(main())
