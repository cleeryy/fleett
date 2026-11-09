#!/usr/bin/env python3
"""Tiny GitHub stand-in for tests/image/test-fetch-s6-overlay.sh.

Serves a directory tree over HTTP so the s6-overlay helper can be exercised
without touching github.com:

* any path under /broken/ answers 504, like the `fra` edge on 2026-11-08;
* a GET under /releases/assets/ requires Accept: application/octet-stream,
  which is the contract of the GitHub release-assets API;
* everything else is a normal static file (release JSON, tarballs, mirror).

Usage: mock_server.py <root-dir> <port-file>
The chosen port is written to <port-file> once the listener is bound.
"""

import http.server
import os
import sys


def main() -> None:
    root = os.path.abspath(sys.argv[1])
    port_file = sys.argv[2]

    class Handler(http.server.SimpleHTTPRequestHandler):
        def __init__(self, *args, **kwargs):
            super().__init__(*args, directory=root, **kwargs)

        def do_GET(self):  # noqa: N802 (http.server API)
            if self.path.startswith("/broken/"):
                self.send_error(504, "Gateway Timeout")
                return
            if "/releases/assets/" in self.path:
                accept = self.headers.get("Accept", "")
                if "application/octet-stream" not in accept:
                    self.send_error(406, "Accept: application/octet-stream required")
                    return
            super().do_GET()

        def log_message(self, *args):
            pass

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    with open(port_file, "w", encoding="utf-8") as handle:
        handle.write(str(server.server_address[1]))
    server.serve_forever()


if __name__ == "__main__":
    main()
