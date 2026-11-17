#!/usr/bin/env python3
"""Black-box configuration/auth tests for the built binary; no real credentials.
Run with CENTAUR_CLI_TEST_BINARY pointing to the executable.
"""
import base64
import http.server
import json
import os
import pathlib
import subprocess
import tempfile
import threading
import unittest
import urllib.parse


class TokenServer(http.server.BaseHTTPRequestHandler):
    requests = 0
    reject = False
    lifecycle = []
    prune_policy = True
    prune_error = 0
    prune_page_error = False

    def lifecycle_response(self, value):
        assert self.headers['Authorization'] == 'Bearer fixture-access-token'
        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.end_headers()
        self.wfile.write(json.dumps(value).encode())

    def do_DELETE(self):
        path = urllib.parse.unquote(self.path)
        if path.endswith('/prune'):
            assert path in ['/api/session/api:acl_test:old/prune', '/api/session/api:acl_test:old2/prune']
            type(self).lifecycle.append(('DELETE', path))
            if path.endswith('old2/prune') and type(self).prune_error:
                self.send_response(type(self).prune_error)
                self.end_headers()
                self.wfile.write(b'changed or cleanup failed')
            else:
                self.lifecycle_response({'deleted': path.endswith('old/prune')})
            return
        assert path == '/api/session/api:acl_test:old'
        type(self).lifecycle.append(('DELETE', path))
        self.lifecycle_response({'deleted': True})

    def do_GET(self):
        type(self).lifecycle.append(('GET', self.path))
        query = urllib.parse.parse_qs(urllib.parse.urlsplit(self.path).query)
        if query.get('prunable_only') == ['true']:
            second = bool(query.get('after_thread_key', [''])[0])
            if second and type(self).prune_page_error:
                self.send_response(500)
                self.end_headers()
                return
            name = 'old2' if second else 'old'
            value = {'items': [{'thread_key': f'api:acl_test:{name}', 'status': 'failed',
                                'archived': second, 'updated_at': '2020-01-01T00:00:00Z'}],
                     'next_after_thread_key': None if second else 'api:acl_test:old'}
            if type(self).prune_policy:
                value['prune_older_than_days'] = 30
            self.lifecycle_response(value)
            return
        self.lifecycle_response({'items': [], 'next_after_thread_key': None})

    def do_POST(self):
        if self.path.endswith('/archive'):
            path = urllib.parse.unquote(self.path)
            assert path == '/api/session/api:acl_test:old/archive'
            body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
            type(self).lifecycle.append(('POST', path, body))
            self.lifecycle_response(body)
            return
        type(self).requests += 1
        assert self.path == "/oauth/token"
        expected = base64.b64encode(b"acl_test:fixture-secret").decode()
        assert self.headers["Authorization"] == "Basic " + expected
        body = self.rfile.read(int(self.headers["Content-Length"]))
        assert body == b"grant_type=client_credentials"
        self.send_response(401 if self.reject else 200)
        self.send_header("Content-Type", "application/json")
        self.end_headers()
        self.wfile.write(json.dumps({
            "error": "fixture-secret" if self.reject else None,
            "access_token": "fixture-access-token",
            "expires_in": 3600,
        }).encode())

    def log_message(self, *_args):
        pass


class Configuration(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="centaur-cli-test-", dir="/tmp")
        self.addCleanup(self.directory.cleanup)
        self.config = pathlib.Path(self.directory.name) / "config.json"
        self.env = {key: value for key, value in os.environ.items() if not key.startswith("CENTAUR_")}
        self.env["CENTAUR_CONFIG"] = str(self.config)
        self.binary = os.environ["CENTAUR_CLI_TEST_BINARY"]
        TokenServer.requests = 0
        TokenServer.reject = False
        TokenServer.lifecycle = []
        TokenServer.prune_policy = True
        TokenServer.prune_error = 0
        TokenServer.prune_page_error = False
        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), TokenServer)
        self.addCleanup(self.server.server_close)
        self.addCleanup(self.server.shutdown)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.endpoint = f"http://127.0.0.1:{self.server.server_port}"
        self.run_cli("config", "set", "endpoint", self.endpoint)
        self.run_cli("config", "set", "client-id", "acl_test")

    def run_cli(self, *args, input=None, ok=True):
        result = subprocess.run([self.binary, *args], env=self.env, input=input,
                                capture_output=True, text=True, timeout=15)
        if ok:
            self.assertEqual(result.returncode, 0, result.stderr)
        else:
            self.assertNotEqual(result.returncode, 0)
        self.assertNotIn("fixture-secret", result.stdout + result.stderr)
        self.assertNotIn("fixture-access-token", result.stdout + result.stderr)
        return result

    def test_login_is_verified_private_and_bound_to_endpoint(self):
        self.run_cli("login", "--secret-stdin", input="fixture-secret\n")
        self.assertEqual(TokenServer.requests, 1)
        self.assertEqual(self.config.stat().st_mode & 0o777, 0o600)
        saved = json.loads(self.config.read_text())
        self.assertEqual(saved["credential-endpoint"], self.endpoint)
        self.env["CENTAUR_API_URL"] = "http://127.0.0.1:1"
        result = self.run_cli("chat", "list", ok=False)
        self.assertIn("different endpoint", result.stderr)
        self.assertEqual(TokenServer.requests, 1)
        del self.env["CENTAUR_API_URL"]
        self.run_cli("config", "set", "client-id", "acl_different")
        self.assertNotIn("client-secret", json.loads(self.config.read_text()))

    def test_rejected_login_never_saves_or_prints_credentials(self):
        TokenServer.reject = True
        self.run_cli("login", "--secret-stdin", input="fixture-secret\n", ok=False)
        self.assertNotIn("client-secret", json.loads(self.config.read_text()))

    def test_environment_secret_does_not_force_an_interactive_prompt(self):
        self.env["CENTAUR_CLIENT_SECRET"] = "fixture-secret"
        self.run_cli("login", input="")
        self.assertEqual(TokenServer.requests, 1)

    def test_delete_requires_confirmation_and_calls_real_delete_endpoint(self):
        self.run_cli('login', '--secret-stdin', input='fixture-secret\n')
        result = self.run_cli('chat', 'delete', 'old', ok=False)
        self.assertIn('--yes', result.stderr)
        self.assertEqual(TokenServer.lifecycle, [])
        self.run_cli('chat', 'delete', 'old', '--yes')
        self.assertEqual(TokenServer.lifecycle, [('DELETE', '/api/session/api:acl_test:old')])

    def test_archive_restore_and_inclusive_listing(self):
        self.run_cli('login', '--secret-stdin', input='fixture-secret\n')
        self.run_cli('chat', 'archive', 'old')
        self.run_cli('chat', 'unarchive', 'old')
        self.run_cli('chat', 'list', '--all')
        self.assertEqual(TokenServer.lifecycle[:2], [
            ('POST', '/api/session/api:acl_test:old/archive', {'archived': True}),
            ('POST', '/api/session/api:acl_test:old/archive', {'archived': False}),
        ])
        self.assertIn('include_archived=true', TokenServer.lifecycle[2][1])

    def test_prune_previews_all_pages_then_uses_only_guarded_deletion(self):
        self.run_cli('login', '--secret-stdin', input='fixture-secret\n')
        preview = self.run_cli('chat', 'prune')
        self.assertIn('matched=2', preview.stdout)
        self.assertTrue(all(entry[0] == 'GET' for entry in TokenServer.lifecycle))
        TokenServer.lifecycle = []
        result = self.run_cli('chat', 'prune', '--yes')
        self.assertIn('deleted=1', result.stdout)
        self.assertIn('skipped=1', result.stdout)
        deletes = [entry[1] for entry in TokenServer.lifecycle if entry[0] == 'DELETE']
        self.assertEqual(deletes, ['/api/session/api:acl_test:old/prune', '/api/session/api:acl_test:old2/prune'])

    def test_prune_refuses_a_backend_without_the_age_filter_contract(self):
        self.run_cli('login', '--secret-stdin', input='fixture-secret\n')
        TokenServer.prune_policy = False
        self.run_cli('chat', 'prune', '--yes', ok=False)
        self.assertTrue(all(entry[0] == 'GET' for entry in TokenServer.lifecycle))

    def test_prune_reports_partial_failures_and_skips_conflicts(self):
        self.run_cli('login', '--secret-stdin', input='fixture-secret\n')
        for status in [409, 500]:
            with self.subTest(status=status):
                TokenServer.lifecycle = []
                TokenServer.prune_error = status
                result = self.run_cli('chat', 'prune', '--yes', ok=status == 409)
                self.assertIn('deleted=1', result.stdout)
                self.assertIn('skipped=1' if status == 409 else 'failed=1', result.stdout)
                self.assertEqual(sum(entry[0] == 'DELETE' for entry in TokenServer.lifecycle), 2)

    def test_prune_reports_progress_when_a_later_page_fails(self):
        self.run_cli('login', '--secret-stdin', input='fixture-secret\n')
        TokenServer.prune_page_error = True
        result = self.run_cli('chat', 'prune', '--yes', ok=False)
        self.assertIn('deleted=1', result.stdout)
        self.assertIn('failed=1', result.stdout)
        self.assertEqual(sum(entry[0] == 'DELETE' for entry in TokenServer.lifecycle), 1)

    def test_insecure_remote_endpoint_is_rejected(self):
        self.run_cli("config", "set", "endpoint", "http://remote.example", ok=False)
        self.assertEqual(json.loads(self.config.read_text())["endpoint"], self.endpoint)


if __name__ == "__main__":
    unittest.main()
