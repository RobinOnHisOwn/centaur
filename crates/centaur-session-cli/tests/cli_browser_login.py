#!/usr/bin/env python3
"""Executable browser/OAuth fixtures; loopback only, synthetic credentials."""
import http.server
import json
import os
import pathlib
import select
import subprocess
import tempfile
import threading
import unittest
import urllib.parse
import urllib.request


class Fixture(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def send_json(self, value):
        data = json.dumps(value).encode()
        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        base = self.server.base
        if self.path.startswith('/.well-known/'):
            self.send_json(dict(issuer=base + '/cli', client_id='centaur-cli',
                                authorization_endpoint=base + '/cli/oauth/authorize',
                                token_endpoint=base + '/cli/oauth/token', revocation_endpoint=base + '/cli/oauth/revoke',
                                session_resource=base + '/api/cli/v1', console_resource=base + '/api/cli/v1',
                                code_challenge_methods_supported=['S256']))
        elif self.path == '/api/cli/v1/me':
            self.send_json(dict(user='usr_fixture', thread_key_prefix='personal:usr_fixture:',
                                session_ready=True, principal='prn_fixture', protocol_version=1))
        elif self.path.startswith('/api/cli/v1/roles'):
            self.send_json({'data': []})
        else:
            self.send_json(dict(items=[], next_after_thread_key=None))

    def do_POST(self):
        body = self.rfile.read(int(self.headers.get('Content-Length', 0))).decode()
        self.server.calls.append((self.path, urllib.parse.parse_qs(body)))
        if self.path.endswith('/revoke'):
            self.send_json({})
        else:
            self.send_json(dict(access_token='synthetic-access', refresh_token='synthetic-rotated-refresh',
                                expires_in=300, token_type='Bearer', scope='cli:identity cli:sessions:read cli:sessions:write cli:integrations'))


class BrowserLogin(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(dir='/tmp')
        self.config = pathlib.Path(self.tmp.name) / 'config.json'
        self.env = {k: v for k, v in os.environ.items() if not k.startswith('CENTAUR_')}
        self.env['CENTAUR_CONFIG'] = str(self.config)
        self.binary = os.environ['CENTAUR_CLI_TEST_BINARY']
        self.server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Fixture)
        self.server.base = f'http://127.0.0.1:{self.server.server_port}'
        self.server.calls = []
        threading.Thread(target=self.server.serve_forever, daemon=True).start()

    def tearDown(self):
        self.server.shutdown()
        self.server.server_close()
        self.tmp.cleanup()

    def cli(self, *args):
        return subprocess.run([self.binary, *args], env=self.env, capture_output=True, text=True, timeout=15)

    def login(self):
        process = subprocess.Popen([self.binary, 'login', '--server', self.server.base, '--no-open'],
                                   env=self.env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        try:
            self.assertTrue(select.select([process.stdout], [], [], 10)[0], 'login did not publish its authorization URL')
            line = process.stdout.readline().strip()
            self.assertTrue(line.startswith('http://'), line + process.stderr.read() if process.poll() is not None else line)
            params = urllib.parse.parse_qs(urllib.parse.urlsplit(line).query)
            self.assertEqual(params['code_challenge_method'], ['S256'])
            callback = params['redirect_uri'][0]
            query = urllib.parse.urlencode(dict(code='synthetic-code', state=params['state'][0], iss=self.server.base + '/cli'))
            with urllib.request.urlopen(callback + '?' + query, timeout=5) as response:
                self.assertEqual(response.status, 200)
            out, err = process.communicate(timeout=10)
            self.assertEqual(process.returncode, 0, err)
            self.assertNotIn('synthetic-access', out + err)
            self.assertNotIn('synthetic-rotated-refresh', out + err)
        finally:
            if process.poll() is None:
                process.kill()
                process.wait()

    def test_one_browser_login_lists_chats_and_revokes_on_logout(self):
        self.login()
        self.assertEqual(self.config.stat().st_mode & 0o777, 0o600)
        listed = self.cli('chat', 'list', '--json')
        self.assertEqual(listed.returncode, 0, listed.stderr)
        status = self.cli('auth', 'status', '--json')
        self.assertEqual(status.returncode, 0, status.stderr)
        self.assertEqual(json.loads(status.stdout)['identity']['user'], 'usr_fixture')
        connections = self.cli('auth', 'connections', 'list')
        self.assertEqual(connections.returncode, 0, connections.stderr)
        admin = self.cli('admin', 'status', '--json')
        self.assertEqual(admin.returncode, 0, admin.stderr)
        self.assertEqual(json.loads(admin.stdout)['data']['auth_mode'], 'browser')
        logged_out = self.cli('logout')
        self.assertEqual(logged_out.returncode, 0, logged_out.stderr)
        self.assertTrue(any(path.endswith('/revoke') for path, _ in self.server.calls))
        self.assertNotIn('synthetic-rotated-refresh', self.config.read_text())

    def test_environment_machine_identity_never_silently_overrides_browser_identity(self):
        self.login()
        self.env['CENTAUR_API_TOKEN'] = 'synthetic-machine-token'
        result = self.cli('chat', 'list', '--json')
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn('synthetic-machine-token', result.stderr)


if __name__ == '__main__':
    unittest.main()
